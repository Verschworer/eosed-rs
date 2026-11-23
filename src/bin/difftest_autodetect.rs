// SPDX-License-Identifier: GPL-2.0-or-later
// Differential twin of difftest_autodetect_py.py: sweep semantics against a
// scripted fake backend. Same scenarios, same canonical output lines.

use eosed::bridge::*;
use eosed::config::Settings;
use eosed::messages as m;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

struct QIn {
    q: Rc<RefCell<VecDeque<Vec<u8>>>>,
}
impl InPort for QIn {
    fn poll(&mut self) -> Option<Vec<u8>> {
        self.q.borrow_mut().pop_front()
    }
}

struct QOut {
    queues: HashMap<String, Rc<RefCell<VecDeque<Vec<u8>>>>>,
    replies: HashMap<String, Vec<Vec<u8>>>,
}
impl OutPort for QOut {
    fn send(&mut self, message: &[u8]) {
        if message.len() >= 6
            && message[0] == 0xF0
            && message[1] == 0x7E
            && message[3] == 0x06
            && message[4] == 0x01
        {
            for (name, q) in &self.queues {
                if let Some(r) = self.replies.get(name) {
                    q.borrow_mut().extend(r.iter().cloned());
                }
            }
        }
    }
}

struct FakeBackend {
    in_names: Vec<String>,
    out_names: Vec<String>,
    queues: HashMap<String, Rc<RefCell<VecDeque<Vec<u8>>>>>,
    replies: HashMap<String, Vec<Vec<u8>>>,
}

impl Backend for FakeBackend {
    fn list_in(&mut self) -> Vec<String> {
        self.in_names.clone()
    }
    fn list_out(&mut self) -> Vec<String> {
        self.out_names.clone()
    }
    fn open_in(&mut self, index: usize) -> Result<Box<dyn InPort>> {
        if index >= self.in_names.len() {
            return Err(BridgeError::Protocol("no such input index".into()));
        }
        Ok(Box::new(QIn {
            q: self.queues[&self.in_names[index]].clone(),
        }))
    }
    fn open_out(&mut self, index: usize) -> Result<Box<dyn OutPort>> {
        if index >= self.out_names.len() {
            return Err(BridgeError::Protocol("no such output index".into()));
        }
        Ok(Box::new(QOut {
            queues: self.queues.clone(),
            replies: self.replies.clone(),
        }))
    }
}

fn inquiry_reply(device_id: u8, member: (u8, u8), rev: &str) -> Vec<u8> {
    let mut v = vec![
        0xF0,
        0x7E,
        device_id,
        0x06,
        0x02,
        m::MANUFACTURER_ID,
        0x01,
        0x04,
        member.0,
        member.1,
    ];
    v.extend(rev.bytes());
    v.push(0xF7);
    v
}

fn hx(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn run_scenario(
    name: &str,
    in_names: &[&str],
    out_names: &[&str],
    replies: &HashMap<String, Vec<Vec<u8>>>,
    cached: Option<(&str, &str)>,
    device_id: Option<u8>,
) {
    let path = format!("autod_{name}.toml");
    {
        let mut s = Settings::new("eosed", &path);
        if let Some((snd, rcv)) = cached {
            s.update(
                Some(&path),
                &[
                    (
                        "port".into(),
                        Some(eosed::config::ConfigValue::Str(snd.into())),
                    ),
                    (
                        "recv_port".into(),
                        Some(eosed::config::ConfigValue::Str(rcv.into())),
                    ),
                ],
            );
        } else {
            std::fs::write(&path, "").unwrap();
        }
    }
    let mut backend = FakeBackend {
        in_names: in_names.iter().map(|s| s.to_string()).collect(),
        out_names: out_names.iter().map(|s| s.to_string()).collect(),
        queues: in_names
            .iter()
            .map(|s| (s.to_string(), Rc::new(RefCell::new(VecDeque::new()))))
            .collect(),
        replies: replies.clone(),
    };
    let on_try = |n: &str| println!("probe|{n}");
    let settings = &mut Settings::new("eosed", &path);
    match autodetect(
        &mut backend,
        Duration::from_millis(1),
        None,
        device_id,
        Duration::from_millis(20),
        Some(&path),
        settings,
        &on_try,
    ) {
        Ok(b) => {
            let r = b.inquiry.clone().unwrap();
            println!(
                "connected|{}|{}|{}",
                b.description.unwrap(),
                r.device_id,
                r.revision
            );
        }
        Err(e) => println!("error|{}", e.to_string().replace('\n', "\\n")),
    }
    println!("cfg|{}", hx(&std::fs::read(&path).unwrap()));
}

fn main() {
    let _ = std::env::set_current_dir(std::env::args().nth(1).unwrap());

    let r_e4xt = inquiry_reply(0, (0x04, 0x05), "4.70");
    let r_ultra = inquiry_reply(5, (0x07, 0x05), "4.70");
    let noise = vec![0x90, 0x40, 0x7F];

    // A: cached pair answers immediately
    let mut rep = HashMap::new();
    rep.insert("In1".to_string(), vec![r_e4xt.clone()]);
    run_scenario(
        "cache",
        &["In1", "In2"],
        &["Out1", "Out2"],
        &rep,
        Some(("Out1", "In1")),
        None,
    );

    // B: sweep, same machine heard on two inputs
    let mut rep = HashMap::new();
    rep.insert("In1".to_string(), vec![r_e4xt.clone()]);
    rep.insert("In2".to_string(), vec![r_e4xt.clone()]);
    run_scenario(
        "dedupe",
        &["In1", "In2"],
        &["Out1", "Out2"],
        &rep,
        None,
        None,
    );

    // C: two distinct devices, no selection -> refuse
    let mut rep = HashMap::new();
    rep.insert("In1".to_string(), vec![r_e4xt.clone()]);
    rep.insert("In2".to_string(), vec![r_ultra.clone()]);
    run_scenario(
        "ambiguous",
        &["In1", "In2"],
        &["Out1", "Out2"],
        &rep,
        None,
        None,
    );

    // D: same but device_id selects
    run_scenario(
        "selected",
        &["In1", "In2"],
        &["Out1", "Out2"],
        &rep,
        None,
        Some(5),
    );

    // E: nothing answers
    let rep = HashMap::new();
    run_scenario("none", &["In1"], &["Out1", "Out2"], &rep, None, None);

    // F: noise before the real reply; malformed reply ignored
    let mut rep = HashMap::new();
    rep.insert("In1".to_string(), vec![noise.clone(), r_e4xt.clone()]);
    rep.insert(
        "In2".to_string(),
        vec![vec![
            0xF0, 0x7E, 1, 0x06, 0x02, 0x99, 1, 2, 3, 4, b'1', b'1', b'1', b'1', 0xF7,
        ]],
    );
    run_scenario("noise", &["In1", "In2"], &["Out1"], &rep, None, None);

    // G: cached pair answers with the wrong device (wanted id 0, cache pair
    // replies 5) -> falls through to the full sweep, which sees two ids -> refuse
    let mut rep = HashMap::new();
    rep.insert("In1".to_string(), vec![r_ultra.clone()]);
    rep.insert("In2".to_string(), vec![r_e4xt.clone()]);
    run_scenario(
        "wrongcache",
        &["In1", "In2"],
        &["Out1", "Out2"],
        &rep,
        Some(("Out1", "In1")),
        Some(0),
    );

    // H: drain cap — 70 non-inquiry messages flood one port
    let mut rep = HashMap::new();
    rep.insert("In1".to_string(), vec![noise.clone(); 70]);
    run_scenario("flood", &["In1", "In2"], &["Out1"], &rep, None, None);
}
