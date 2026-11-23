// SPDX-License-Identifier: GPL-2.0-or-later
// Differential twin of scratch/difftest_bridge_py.py: dump receive/retry/trim,
// send handshake, parameter ops. Prints every frame the bridge SENT.

use eosed::bridge::{signed_value, EosBridge, MidiIo, ThrottledOut};
use eosed::messages as m;
use std::collections::VecDeque;
use std::time::Duration;

struct Fake {
    scripted: VecDeque<Vec<u8>>,
    wire: VecDeque<Vec<u8>>,
    sent: Vec<Vec<u8>>,
}
impl MidiIo for Fake {
    fn send(&mut self, message: &[u8]) {
        self.sent.push(message.to_vec());
        if let Some(r) = self.scripted.pop_front() {
            self.wire.push_back(r);
        }
    }
    fn poll(&mut self) -> Option<Vec<u8>> {
        self.wire.pop_front()
    }
}

fn hex(frames: &[Vec<u8>]) -> String {
    frames
        .iter()
        .map(|f| {
            f.iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn main() {
    let dev = 0u8;

    // -- scenario 1: OLD dump, first packet corrupt, retry clean, pad trim --
    let header = m::OldDumpHeader {
        byte_count: 300,
        packet_number: 0,
        device_id: dev,
    }
    .encode()
    .unwrap();
    let bad = {
        let mut msg = m::OldDumpMessage {
            packet_number: 1,
            data: vec![0u8; 256],
            device_id: dev,
            checksum_byte: None,
        };
        let mut f = msg.encode().unwrap();
        let idx = f.len() - 2;
        f[idx] ^= 0x40; // corrupt the checksum byte on the wire
        msg.checksum_byte = Some(f[idx]); // as the decoder will see it
        f
    };
    let good1 = m::OldDumpMessage {
        packet_number: 1,
        data: (0u16..256).map(|i| i as u8).collect(),
        device_id: dev,
        checksum_byte: None,
    }
    .encode()
    .unwrap();
    let good2 = m::OldDumpMessage {
        packet_number: 2,
        data: vec![0xABu8; 256],
        device_id: dev,
        checksum_byte: None,
    }
    .encode()
    .unwrap();
    let eof = m::EndOfFile { device_id: dev }.encode();
    let fake = Fake {
        scripted: vec![header, bad, good1, good2, eof.clone()].into(),
        wire: VecDeque::new(),
        sent: Vec::new(),
    };
    let mut b = EosBridge::new(
        ThrottledOut::new(fake, Duration::from_millis(1), None),
        0,
        Duration::from_millis(50),
    );
    let data = b.dump_preset_old(42, None, 3).unwrap();
    println!("dump1_sent|{}", hex(&b.midi_out.inner_mut().sent));
    println!(
        "dump1_data|{}",
        data.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );

    // -- scenario 2: NEW dump clean --
    let nheader = m::NewDumpHeader {
        preset: 3,
        total_bytes: 10,
        num_global_params: 1,
        num_link_params: 0,
        num_voice_params: 1,
        num_zone_params: 0,
        device_id: dev,
    }
    .encode()
    .unwrap();
    let nd = m::NewDumpMessage {
        packet_number: 1,
        data: (0..10u8).collect(),
        device_id: dev,
        checksum_byte: None,
    }
    .encode()
    .unwrap();
    let fake = Fake {
        scripted: vec![nheader, nd, eof.clone()].into(),
        wire: VecDeque::new(),
        sent: Vec::new(),
    };
    let mut b = EosBridge::new(
        ThrottledOut::new(fake, Duration::from_millis(1), None),
        0,
        Duration::from_millis(50),
    );
    let (h, data) = b.dump_preset_new(3, None, 3).unwrap();
    println!("dump2_sent|{}", hex(&b.midi_out.inner_mut().sent));
    println!(
        "dump2_hdr|{} {} {} {} {} {}",
        h.preset,
        h.total_bytes,
        h.num_global_params,
        h.num_link_params,
        h.num_voice_params,
        h.num_zone_params
    );
    println!(
        "dump2_data|{}",
        data.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );

    // -- scenario 3: OLD send handshake --
    let payload: Vec<u8> = {
        let mut v = vec![0x09, 0x00];
        v.extend((0..300u32).map(|i| (i % 251) as u8));
        v
    };
    let a0 = m::Ack {
        packet_number: 0,
        device_id: dev,
    }
    .encode();
    let a1 = m::Ack {
        packet_number: 1,
        device_id: dev,
    }
    .encode();
    let a2 = m::Ack {
        packet_number: 2,
        device_id: dev,
    }
    .encode();
    let fake = Fake {
        scripted: vec![a0, a1, a2].into(),
        wire: VecDeque::new(),
        sent: Vec::new(),
    };
    let mut b = EosBridge::new(
        ThrottledOut::new(fake, Duration::from_millis(1), None),
        0,
        Duration::from_millis(50),
    );
    let preset = b.send_preset_old(&payload, true, None, 2).unwrap();
    println!("send3_sent|{}", hex(&b.midi_out.inner_mut().sent));
    println!("send3_preset|{preset}");

    // -- scenario 4: parameter ops --
    let preq_reply = m::ParameterEdit {
        values: vec![(0, 16372)],
        device_id: dev,
        checksum_byte: None,
    }
    .encode()
    .unwrap();
    let fake = Fake {
        scripted: vec![preq_reply].into(),
        wire: VecDeque::new(),
        sent: Vec::new(),
    };
    let mut b = EosBridge::new(
        ThrottledOut::new(fake, Duration::from_millis(1), None),
        0,
        Duration::from_millis(50),
    );
    println!("param|{}", b.get_parameter(0, None).unwrap());
    b.set_parameters(&(0..100u32).map(|i| (i, i as i64 - 40)).collect::<Vec<_>>())
        .unwrap();
    println!("setparams_sent|{}", b.midi_out.inner_mut().sent.len());

    // -- scenario 5: program change --
    let fake = Fake {
        scripted: VecDeque::new(),
        wire: VecDeque::new(),
        sent: Vec::new(),
    };
    let mut b = EosBridge::new(
        ThrottledOut::new(fake, Duration::from_millis(1), None),
        0,
        Duration::from_millis(50),
    );
    b.send_program_change(52, Some(3), None).unwrap();
    println!("pc|{}", hex(&b.midi_out.inner_mut().sent));

    // -- scenario 6: signed_value sweep over the whole table --
    let mut out = Vec::new();
    for p in eosed::params::PARAMS_ITER {
        for raw in [0u32, 1, 0x1FFF, 0x2000, 0x3FFF] {
            out.push(format!("{}|{raw}|{}", p.id, signed_value(p.id, raw)));
        }
    }
    println!("signed|{}", out.len());
    for l in out {
        println!("{l}");
    }
}
