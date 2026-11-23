// SPDX-License-Identifier: GPL-2.0-or-later
// Port of vinsynlib.config.Settings + eos/config.py wrappers.
// Semantics pinned by the Python source (vinsynlib 0.4.4):
// - a missing file reads as empty and is NOT an error;
// - a file that exists but cannot be parsed reports "unreadable", and
//   update() refuses to overwrite it (that refusal is why escaping matters);
// - a file whose header names ANOTHER tool in the family reads as empty
//   and update() refuses to write it;
// - files holding tables/lists are refused (this writer emits scalars only);
// - only IO errors are swallowed on write;
// - writes are atomic via a temp beside the target, always UTF-8.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG_PATH: &str = "config.toml";
const HEADER_SUFFIX: &str = " local config - gitignored, safe to delete.";

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

pub const CACHE_DEPTHS: [&str; 3] = ["names", "structure", "full"];
pub const FULLSCAN: &str = "fullscan";

#[derive(Debug, Clone, PartialEq)]
pub enum EarlyStop {
    Int(i64),
    Fullscan,
}

fn owner(first_line: Option<&str>) -> Option<String> {
    let line = first_line?.trim();
    if !line.starts_with('#') {
        return None;
    }
    let body = line[1..].trim();
    if !body.ends_with(HEADER_SUFFIX) {
        return None;
    }
    let name = body[..body.len() - HEADER_SUFFIX.len()].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// TOML basic string, escaped exactly like `_toml_string`: the usual
/// backslash escapes, and control characters with no TOML escape become
/// \uXXXX.
fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            c => {
                let o = c as u32;
                if o < 0x20 || o == 0x7F {
                    out.push_str(&format!("\\u{o:04X}"));
                } else {
                    out.push(c);
                }
            }
        }
    }
    out.push('"');
    out
}

#[allow(dead_code)]
fn toml_key(key: &str) -> String {
    if !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        key.to_string()
    } else {
        toml_string(key)
    }
}

pub struct Settings {
    app_name: String,
    default_path: String,
    warned: HashSet<String>,
}

impl Settings {
    pub fn new(app_name: &str, default_path: &str) -> Settings {
        Settings {
            app_name: app_name.to_string(),
            default_path: default_path.to_string(),
            warned: HashSet::new(),
        }
    }

    fn target<'a>(&'a self, path: Option<&'a str>) -> &'a str {
        path.unwrap_or(&self.default_path)
    }

    fn read_document(&self, target: &str) -> (toml::Value, String, Option<String>) {
        if !Path::new(target).exists() {
            return (toml::Value::Table(toml::Table::new()), "ok".into(), None);
        }
        let raw = match std::fs::read(Path::new(target)) {
            Ok(r) => r,
            Err(_) => {
                return (
                    toml::Value::Table(toml::Table::new()),
                    "unreadable".into(),
                    None,
                )
            }
        };
        let text = match std::str::from_utf8(&raw) {
            Ok(t) => t.to_string(),
            Err(_) => {
                // Not valid UTF-8: almost certainly written by a build using
                // the locale codec (cp1252). Decode leniently so hand-edited
                // keys survive the upgrade; the next save repairs encoding.
                encoding_rs::WINDOWS_1252
                    .decode_without_bom_handling(&raw)
                    .0
                    .to_string()
            }
        };
        if text.is_empty() {
            return (
                toml::Value::Table(toml::Table::new()),
                "ok".into(),
                Some(String::new()),
            );
        }
        match text.parse::<toml::Value>() {
            Ok(v) => {
                let first = text.split('\n').next().map(|l| l.to_string());
                (v, "ok".into(), first)
            }
            Err(_) => (
                toml::Value::Table(toml::Table::new()),
                "unreadable".into(),
                None,
            ),
        }
    }

    /// The file's keys and "ok"/"unreadable"; a foreign-owned file reads
    /// empty but is not an error.
    pub fn read(&mut self, path: Option<&str>) -> (toml::Table, String) {
        let target = self.target(path);
        let (doc, status, first) = self.read_document(target);
        if status != "ok" {
            if let toml::Value::Table(t) = doc {
                return (t, status);
            }
            return (toml::Table::new(), status);
        }
        if let Some(o) = owner(first.as_deref()) {
            if o != self.app_name {
                return (toml::Table::new(), "ok".into());
            }
        }
        match doc {
            toml::Value::Table(t) => (t, "ok".into()),
            _ => (toml::Table::new(), "ok".into()),
        }
    }

    fn warn_once(&mut self, reason: &str, message: &str) {
        if self.warned.insert(reason.to_string()) {
            eprintln!("{message}");
        }
    }

    pub fn update(&mut self, path: Option<&str>, changes: &[(String, Option<ConfigValue>)]) {
        let target = self.target(path).to_string();
        let (doc, status, first) = self.read_document(&target);
        if status == "unreadable" {
            self.warn_once(
                "unreadable",
                &format!("{}: {} could not be parsed, so settings are not being saved. Fix or delete it; nothing has been overwritten.", self.app_name, target),
            );
            return;
        }
        if let Some(o) = owner(first.as_deref()) {
            if o != self.app_name {
                self.warn_once(
                    "foreign",
                    &format!("{}: {} is {}'s settings file, so {} is leaving it alone. Use --config to name a path of its own.", self.app_name, target, o, self.app_name),
                );
                return;
            }
        }
        let table = match doc {
            toml::Value::Table(t) => t,
            _ => toml::Table::new(),
        };
        if table
            .values()
            .any(|v| matches!(v, toml::Value::Array(_) | toml::Value::Table(_)))
        {
            self.warn_once(
                "complex",
                &format!("{}: {} holds tables or lists, which {} does not write, so it is leaving it alone. Use --config to name a path of its own.", self.app_name, target, self.app_name),
            );
            return;
        }
        let mut data = table;
        for (key, value) in changes {
            match value {
                None => {
                    data.remove(key);
                }
                Some(v) => {
                    data.insert(key.clone(), toml_value(v));
                }
            }
        }
        let mut lines = vec![format!("# {}{}", self.app_name, HEADER_SUFFIX)];
        for (key, value) in &data {
            // Raw key, exactly like the Python writer: a dotted key lands
            // unquoted, i.e. becomes a nested table on the next read — which
            // the complex refusal then catches. `toml_key` belongs to the
            // ui-state writer, not here; matching the original byte-for-byte
            // is the point.
            let rendered = match value {
                toml::Value::Boolean(b) => format!("{}", b),
                toml::Value::String(s) => toml_string(s),
                toml::Value::Integer(i) => format!("{i}"),
                toml::Value::Float(f) => format!("{f}"),
                _ => format!("{value}"),
            };
            lines.push(format!("{} = {}", key, rendered));
        }
        write_atomic(&target, &format!("{}\n", lines.join("\n")));
    }

    pub fn load_ports(&mut self, path: Option<&str>) -> Option<(String, String)> {
        let data = self.read(path).0;
        let send = data.get("port").and_then(as_str);
        let recv = data.get("recv_port").and_then(as_str);
        match (send, recv) {
            (Some(s), Some(r)) if !s.is_empty() && !r.is_empty() => Some((s, r)),
            _ => None,
        }
    }

    pub fn save_ports(&mut self, send_port: &str, recv_port: &str, path: Option<&str>) {
        self.update(
            path,
            &[
                ("port".into(), Some(ConfigValue::Str(send_port.to_string()))),
                (
                    "recv_port".into(),
                    Some(ConfigValue::Str(recv_port.to_string())),
                ),
            ],
        );
    }
}

fn toml_value(v: &ConfigValue) -> toml::Value {
    match v {
        ConfigValue::Bool(b) => toml::Value::Boolean(*b),
        ConfigValue::Int(i) => toml::Value::Integer(*i),
        ConfigValue::Float(f) => toml::Value::Float(*f),
        ConfigValue::Str(s) => toml::Value::String(s.clone()),
    }
}

fn as_str(v: &toml::Value) -> Option<String> {
    match v {
        toml::Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn as_bool(v: &toml::Value) -> Option<bool> {
    match v {
        toml::Value::Boolean(b) => Some(*b),
        _ => None,
    }
}

fn write_atomic(target: &str, text: &str) {
    let tmp = format!("{}.{}.tmp", target, std::process::id());
    let result = (|| -> io::Result<()> {
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

// --- eos/config.py thin wrappers -------------------------------------------

pub fn load_last_ports(path: Option<&str>, settings: &mut Settings) -> Option<(String, String)> {
    settings.load_ports(path)
}

pub fn save_last_ports(
    send_port: &str,
    recv_port: &str,
    path: Option<&str>,
    settings: &mut Settings,
) {
    settings.save_ports(send_port, recv_port, path)
}

pub fn load_compact_view(path: Option<&str>, settings: &mut Settings) -> Option<bool> {
    settings.read(path).0.get("compact_view").and_then(as_bool)
}

pub fn load_cache_all_on_startup(path: Option<&str>, settings: &mut Settings) -> Option<bool> {
    settings
        .read(path)
        .0
        .get("cache_all_on_startup")
        .and_then(as_bool)
}

pub fn load_cache_structure_on_startup(
    path: Option<&str>,
    settings: &mut Settings,
) -> Option<bool> {
    settings
        .read(path)
        .0
        .get("cache_structure_on_startup")
        .and_then(as_bool)
}

pub fn load_cache_depth(path: Option<&str>, settings: &mut Settings) -> Option<String> {
    if let Some(s) = settings
        .read(path)
        .0
        .get("cache_depth")
        .and_then(|v| match v {
            toml::Value::String(s) => Some(s.clone()),
            _ => None,
        })
    {
        let normalized = s.trim().to_lowercase();
        if CACHE_DEPTHS.contains(&normalized.as_str()) {
            return Some(normalized);
        }
    }
    None
}

pub fn load_sample_usage_early_stop(
    path: Option<&str>,
    settings: &mut Settings,
) -> Option<EarlyStop> {
    match settings.read(path).0.get("sample_usage_early_stop") {
        Some(toml::Value::Integer(i)) => Some(EarlyStop::Int(*i)),
        Some(toml::Value::String(s)) if s.trim().to_lowercase() == FULLSCAN => {
            Some(EarlyStop::Fullscan)
        }
        _ => None,
    }
    // Note: TOML booleans are Boolean, not Integer, so `isinstance(value, bool)`
    // guard in Python is naturally covered — TOML true never lands in Integer.
}

pub fn load_send_pc_on_preset_select(path: Option<&str>, settings: &mut Settings) -> Option<bool> {
    settings
        .read(path)
        .0
        .get("send_pc_on_preset_select")
        .and_then(as_bool)
}

pub fn read_all(path: Option<&str>, settings: &mut Settings) -> HashMap<String, toml::Value> {
    settings
        .read(path)
        .0
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

pub fn default_config_path() -> PathBuf {
    PathBuf::from(DEFAULT_CONFIG_PATH)
}
