// SPDX-License-Identifier: GPL-2.0-or-later
// Differential twin of difftest_config_py.py. Same scenarios, same order.

use eosed::config::*;

fn hx(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn norm(v: &toml::Value) -> (String, String) {
    match v {
        toml::Value::Boolean(b) => ("bool".into(), if *b { "1".into() } else { "0".into() }),
        toml::Value::Integer(i) => ("int".into(), format!("{i}")),
        toml::Value::String(s) => ("str".into(), hx(s.as_bytes())),
        toml::Value::Float(f) => ("float".into(), format!("{f}")),
        toml::Value::Table(_) => ("table".into(), String::new()),
        toml::Value::Array(_) => ("array".into(), String::new()),
        _ => ("other".into(), String::new()),
    }
}

fn emit_kv(data: &toml::Table) {
    for (k, v) in data {
        let (t, val) = norm(v);
        println!("kv|{k}|{t}|{val}");
    }
}

fn fbytes(p: &str) -> String {
    std::fs::read(p).map(|b| hx(&b)).unwrap_or_default()
}

fn scenario(name: &str, content: Option<&[u8]>, updates: &[&[(String, Option<ConfigValue>)]]) {
    let path = format!("s_{name}.toml");
    if let Some(c) = content {
        std::fs::write(&path, c).unwrap();
    }
    let mut s = Settings::new("eosed", "config.toml");
    let (data, status) = s.read(Some(&path));
    println!("scen|{name}|read_status|{status}");
    emit_kv(&data);
    for upd in updates {
        s.update(Some(&path), upd);
    }
    println!("scen|{name}|file|{}", fbytes(&path));
}

fn main() {
    let dir = std::env::args().nth(1).expect("dir arg");
    std::env::set_current_dir(&dir).unwrap();

    scenario(
        "missing",
        None,
        &[&[
            ("port".into(), Some(ConfigValue::Str("Out\"Quoted".into()))),
            (
                "recv_port".into(),
                Some(ConfigValue::Str("ALSA: Midi".into())),
            ),
        ]],
    );

    scenario("valid", Some(b"# eosed local config - gitignored, safe to delete.\nport = \"Motif\\\"7\"\nrecv_port = \"ALSA: Midi\"\ncompact_view = true\ncache_depth = \" FULL \"\nsample_usage_early_stop = 7\nsend_pc_on_preset_select = false\nnote = \"caf\xe9\"\n"), &[
        &[("port".into(), Some(ConfigValue::Str("New Port".into()))),
          ("recv_port".into(), None)],
        &[("sample_usage_early_stop".into(), Some(ConfigValue::Str("fullscan".into())))],
    ]);

    {
        let mut s = Settings::new("eosed", "config.toml");
        let (data, _) = s.read(Some("s_valid.toml"));
        let cv = data.get("compact_view");
        println!(
            "wr|compact_view|{}",
            match cv {
                Some(toml::Value::Boolean(b)) => (if *b { "1" } else { "0" }).to_string(),
                _ => "none".into(),
            }
        );
        if let Some(toml::Value::String(d)) = data.get("cache_depth") {
            let n = d.trim().to_lowercase();
            println!(
                "wr|cache_depth|{}",
                if CACHE_DEPTHS.contains(&n.as_str()) {
                    n
                } else {
                    "none".into()
                }
            );
        } else {
            println!("wr|cache_depth|none");
        }
        match data.get("sample_usage_early_stop") {
            Some(toml::Value::Integer(i)) => println!("wr|early_stop|int:{i}"),
            Some(toml::Value::String(s)) if s.trim().to_lowercase() == FULLSCAN => {
                println!("wr|early_stop|fullscan")
            }
            _ => println!("wr|early_stop|none"),
        }
        let sp = data.get("send_pc_on_preset_select");
        println!(
            "wr|send_pc|{}",
            match sp {
                Some(toml::Value::Boolean(b)) => (if *b { "1" } else { "0" }).to_string(),
                _ => "none".into(),
            }
        );
        let ports = s.load_ports(Some("s_valid.toml"));
        println!(
            "wr|ports|{}",
            match ports {
                Some((a, b)) => format!("('{a}', '{b}')"),
                None => "None".into(),
            }
        );
    }

    scenario(
        "foreign",
        Some(b"# k2k local config - gitignored, safe to delete.\nport = \"other\"\n"),
        &[&[("port".into(), Some(ConfigValue::Str("mine".into())))]],
    );
    scenario(
        "unparseable",
        Some(b"port = \"unterminated\n"),
        &[&[("port".into(), Some(ConfigValue::Str("mine".into())))]],
    );
    scenario(
        "tables",
        Some(b"[section]\nx = 1\n"),
        &[&[("port".into(), Some(ConfigValue::Str("mine".into())))]],
    );
    scenario(
        "cp1252",
        Some(b"# eosed local config - gitignored, safe to delete.\nnote = \"caf\xe9\"\n"),
        &[&[("note2".into(), Some(ConfigValue::Str("x".into())))]],
    );
    scenario(
        "control",
        None,
        &[&[
            ("note2".into(), Some(ConfigValue::Str("a\x01b\x7fc".into()))),
            ("dotted.key".into(), Some(ConfigValue::Str("y".into()))),
        ]],
    );
    scenario(
        "empty",
        Some(b""),
        &[&[("port".into(), Some(ConfigValue::Str("p".into())))]],
    );
    scenario(
        "emdash",
        Some(b"# eosed\xe2\x80\x93 local config - gitignored, safe to delete.\nport = \"old\"\n"),
        &[&[("recv_port".into(), Some(ConfigValue::Str("r".into())))]],
    );
}
