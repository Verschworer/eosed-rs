// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// Rust twin of eosed/app.py: the interactive editor. Textual's panes and
// modal screens become ratatui widgets and mode flags, but the contract is
// the same BINDINGS set: the write gate armed by 'w', the undo log that
// replays edits under the selection they were made under, structure/full
// sweeps with the early-stop gap, integrity and usage over the same walk,
// the front panel over the *panel* protocol (never the editor one).

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use ratatui::Frame;

use eosed::backend_midir::MidirBackend;
use eosed::bridge::{
    autodetect, standard_bridge, Backend, BridgeLike, SignedParameterRange, AUTODETECT_TIMEOUT,
    DEFAULT_TIMEOUT, SEND_GAP,
};
use eosed::config::{
    load_cache_depth, load_compact_view, load_sample_usage_early_stop,
    load_send_pc_on_preset_select, EarlyStop, Settings, DEFAULT_CONFIG_PATH,
};
use eosed::demo::Demo;
use eosed::lcd;
use eosed::messages as m;
use eosed::panel::{open_session, request_screen};
use eosed::params;
use eosed::tui_logic as tl;

struct Args {
    port: Option<String>,
    device_id: Option<u8>,
    timeout: Duration,
    config: String,
    gap: Duration,
    write_gap: Option<Duration>,
    demo: bool,
    allow_write: bool,
    capture: bool,
    panel_render: String,
}

fn die_usage(msg: &str) -> ! {
    eprintln!("error: {msg}");
    eprintln!("usage: eosed_tui [--port NAME] [--device-id N] [--timeout SECS] [--config PATH] [--gap MS] [--write-gap MS] [--allow-write] [--demo] [--panel-render quadrant|half|braille]");
    std::process::exit(2);
}

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut a = Args {
        port: None,
        device_id: None,
        timeout: DEFAULT_TIMEOUT,
        config: DEFAULT_CONFIG_PATH.to_string(),
        gap: SEND_GAP,
        write_gap: None,
        demo: false,
        allow_write: false,
        capture: false,
        panel_render: "quadrant".into(),
    };
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].clone();
        match arg.as_str() {
            "--port" => {
                if i + 1 >= argv.len() {
                    die_usage("--port needs a value");
                }
                a.port = Some(argv[i + 1].clone());
                i += 2;
            }
            "--device-id" => {
                if i + 1 >= argv.len() {
                    die_usage("--device-id needs a value");
                }
                let v: u32 = argv[i + 1].parse().unwrap_or(999);
                if v > 127 {
                    die_usage("--device-id must be 0-127");
                }
                a.device_id = Some(v as u8);
                i += 2;
            }
            "--timeout" => {
                if i + 1 >= argv.len() {
                    die_usage("--timeout needs a value");
                }
                let s: f64 = argv[i + 1].parse().unwrap_or(0.0);
                if s <= 0.0 {
                    die_usage("--timeout must be a positive number of seconds");
                }
                a.timeout = Duration::from_secs_f64(s);
                i += 2;
            }
            "--config" => {
                a.config = argv[i + 1].clone();
                i += 2;
            }
            "--gap" => {
                a.gap = Duration::from_millis(argv[i + 1].parse().unwrap_or(0));
                i += 2;
            }
            "--write-gap" => {
                a.write_gap = Some(Duration::from_millis(argv[i + 1].parse().unwrap_or(0)));
                i += 2;
            }
            "--panel-render" => {
                let v = argv[i + 1].clone();
                if !["quadrant", "half", "braille"].contains(&v.as_str()) {
                    die_usage("--panel-render must be quadrant, half or braille");
                }
                a.panel_render = v;
                i += 2;
            }
            "--demo" => {
                a.demo = true;
                i += 1;
            }
            "--capture-screens" => {
                a.capture = true;
                i += 1;
            }
            "--allow-write" => {
                a.allow_write = true;
                i += 1;
            }
            _ => die_usage(&format!("unknown argument {arg:?}")),
        }
    }
    a
}

#[derive(Clone, Debug, PartialEq)]
enum ChangeValue {
    Int(i64),
    Name(String),
}

struct Change {
    param_id: Option<u32>,
    old: ChangeValue,
    new: ChangeValue,
    voice: Option<usize>,
    link: Option<usize>,
    scope: String,
}

impl Change {
    fn label(&self) -> String {
        match self.param_id {
            None => "preset name".into(),
            Some(pid) => params::by_id(pid)
                .map(|p| p.name.to_string())
                .unwrap_or_else(|| format!("id {pid}")),
        }
    }
    fn describe(&self, value: &ChangeValue) -> String {
        match value {
            ChangeValue::Name(n) => format!("{n:?}"),
            ChangeValue::Int(v) => match self.param_id {
                Some(pid) => match params::by_id(pid) {
                    Some(p) => params::describe_value(p, *v as i32),
                    None => v.to_string(),
                },
                None => v.to_string(),
            },
        }
    }
}

#[derive(Clone, PartialEq)]
enum PromptKind {
    Goto,
    Rename,
    Voice,
    Link,
    EditValue(u32),
}

#[derive(Clone, PartialEq)]
enum Mode {
    Normal,
    Prompt(PromptKind),
    ConfirmSweep(String),
    Master,
    Help,
    History,
    Panel,
}

#[derive(Clone, PartialEq)]
enum Focus {
    Browser,
    Params,
}

const HINT_BLOCKS: &[&str] = &[
    "q Quit",
    "p Presets",
    "s Samples",
    "g Goto",
    "r Refresh",
    "o Rename",
    "v Voices",
    "l Links",
    "u Find usage",
    "i Integrity",
    "c Cache structure",
    "C Cache everything",
    "x Clear usage cache",
    "e Extended view",
    "Esc Back",
    "k Front panel",
    "m Master",
    "w Write gate",
    "z Undo",
    "Z Undo all",
    "h History",
    "? Help",
    "+ Value +1",
    "- Value -1",
];

struct App {
    bridge: Box<dyn BridgeLike>,
    device_id: u8,
    description: String,
    allow_write: bool,
    demo: bool,
    timeout: Duration,
    gap_setting: Option<usize>,
    panel_render: String,
    send_port: Option<String>,

    bank: String,
    window_start: usize,
    names: BTreeMap<u32, String>,
    browser_cursor: usize,

    current_preset: Option<u32>,
    current_sample: Option<u32>,
    current_voice: Option<usize>,
    current_link: Option<usize>,

    param_ids: Vec<u32>,
    param_label: String,
    param_values: HashMap<u32, i64>,
    params_cursor: usize,
    sample_rows: Vec<(i64, String, String)>,
    borrow_rows: Vec<(String, String, String, String)>,

    changes: Vec<Change>,
    ranges: HashMap<u32, SignedParameterRange>,
    overviews: BTreeMap<u32, tl::Overview>,
    sample_cache: HashMap<u32, String>,
    usage_index: HashMap<i64, Vec<(u32, String)>>,
    usage_scanned: bool,

    status: String,
    pending: Option<String>,
    extended: bool,
    mode: Mode,
    input: String,
    focus: Focus,
    sweep: Option<tl::SweepState>,
    sweep_after: Option<(String, u32)>, // (kind, subject)
    panel_lines: Vec<String>,
    running: bool,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    fn new(
        mut bridge: Box<dyn BridgeLike>,
        allow_write: bool,
        demo: bool,
        timeout: Duration,
        settings: &mut Settings,
        config: &str,
        panel_render: String,
        send_port: Option<String>,
    ) -> App {
        let gap = match load_sample_usage_early_stop(Some(config), settings) {
            Some(EarlyStop::Int(i)) => Some(i.max(0) as usize),
            Some(EarlyStop::Fullscan) => None,
            None => Some(tl::SAMPLE_USAGE_EARLY_STOP_DEFAULT),
        };
        let compact = load_compact_view(Some(config), settings).unwrap_or(true);
        let inquiry = bridge.inquire(None).ok();
        let device_id = inquiry
            .as_ref()
            .map(|q| q.device_id)
            .unwrap_or(m::DEFAULT_DEVICE_ID);
        let revision = inquiry.map(|q| q.revision.clone()).unwrap_or_default();
        let mut app = App {
            bridge,
            device_id,
            description: format!("device id {device_id} · fw {revision}"),
            allow_write,
            demo,
            timeout,
            gap_setting: gap,
            panel_render,
            send_port,
            bank: "preset".into(),
            window_start: 0,
            names: BTreeMap::new(),
            browser_cursor: 0,
            current_preset: None,
            current_sample: None,
            current_voice: None,
            current_link: None,
            param_ids: Vec::new(),
            param_label: String::new(),
            param_values: HashMap::new(),
            params_cursor: 0,
            sample_rows: Vec::new(),
            borrow_rows: Vec::new(),
            changes: Vec::new(),
            ranges: HashMap::new(),
            overviews: BTreeMap::new(),
            sample_cache: HashMap::new(),
            usage_index: HashMap::new(),
            usage_scanned: false,
            status: "select a preset ('p' lists the bank, '?' for keys)".into(),
            pending: None,
            extended: !compact,
            mode: Mode::Normal,
            input: String::new(),
            focus: Focus::Browser,
            sweep: None,
            sweep_after: None,
            panel_lines: Vec::new(),
            running: true,
        };
        app.load_bank_page(0);
        app
    }

    // -- bank browser ------------------------------------------------------
    fn load_bank_page(&mut self, start: usize) {
        let range = (start as u32, (start + tl::BROWSER_MIN_WINDOW) as u32);
        let fetched = if self.bank == "preset" {
            self.bridge.catalog_presets(range, Some(self.timeout), None)
        } else {
            self.bridge.catalog_samples(range, Some(self.timeout), None)
        };
        for (k, v) in fetched {
            self.names.insert(k, v);
        }
        self.window_start = start;
        if self.browser_cursor >= self.browser_rows().len() {
            self.browser_cursor = 0;
        }
    }

    fn browser_rows(&self) -> Vec<(u32, String)> {
        let end = (self.window_start + tl::BROWSER_MIN_WINDOW) as u32;
        self.names
            .range(self.window_start as u32..end)
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    }

    fn select_row(&mut self) {
        let rows = self.browser_rows();
        let (number, name) = match rows.get(self.browser_cursor) {
            Some(r) => r.clone(),
            None => {
                self.status = "select a preset or sample first".into();
                return;
            }
        };
        if self.bank == "preset" {
            self.select_preset(number);
        } else {
            self.current_sample = Some(number);
            self.current_voice = None;
            self.current_link = None;
            self.status = format!("sample {}: {}", tl::slot_label("sample", number), name);
        }
    }

    // -- preset / voice / link views --------------------------------------
    fn select_preset(&mut self, preset: u32) {
        self.current_preset = Some(preset);
        self.current_voice = None;
        self.current_link = None;
        if let Err(e) = self
            .bridge
            .set_parameter(params::by_name("PRESET_SELECT").unwrap().id, preset as i64)
        {
            self.status = format!("error: {e}");
            return;
        }
        if load_send_pc_on_preset_select(None, &mut Settings::new("eosed", &self.config_path()))
            .unwrap_or(false)
        {
            let _ = self
                .bridge
                .send_program_change(preset, None, Some(self.timeout));
        }
        self.show_preset_overview();
    }

    fn config_path(&self) -> String {
        DEFAULT_CONFIG_PATH.to_string()
    }

    fn show_preset_overview(&mut self) {
        let preset = match self.current_preset {
            Some(p) => p,
            None => return,
        };
        self.param_ids = tl::group_param_ids("global");
        self.param_label = "global".into();
        match self
            .bridge
            .get_parameters(&self.param_ids, Some(self.timeout))
        {
            Ok(values) => self.param_values = values,
            Err(e) => {
                self.status = format!("error: {e}");
                return;
            }
        }
        self.sample_rows = self
            .overviews
            .get(&preset)
            .map(|ov| ov.sample_rows.clone())
            .unwrap_or_default();
        self.borrow_rows.clear();
        self.params_cursor = 0;
        let pending = self.pending.take();
        self.status = pending.unwrap_or_else(|| {
            format!(
                "preset {preset}: {} parameters ({})",
                self.param_label,
                self.param_ids.len()
            )
        });
    }

    fn walk_preset(&mut self, preset: u32) -> Result<usize, String> {
        if let Some(ov) = self.overviews.get(&preset) {
            return Ok(ov.voice_count);
        }
        let mut by_voice: BTreeMap<usize, Vec<i64>> = BTreeMap::new();
        let mut count = 0usize;
        for voice in 0..tl::MAX_VOICE_SCAN {
            match tl::voice_sample_info(&mut *self.bridge, preset, voice, Some(self.timeout)) {
                None => break,
                Some((_, numbers)) => {
                    by_voice.insert(voice as usize, numbers);
                    count = voice as usize + 1;
                }
            }
        }
        let mut memo = HashMap::new();
        let rows = tl::resolve_sample_rows(
            &mut *self.bridge,
            &by_voice,
            &mut memo,
            &self.sample_cache,
            true,
            Some(self.timeout),
        );
        self.overviews.insert(
            preset,
            tl::Overview {
                voice_count: count,
                sample_rows: rows,
            },
        );
        Ok(count)
    }

    fn select_voice(&mut self, voice: usize) {
        let preset = match self.current_preset {
            Some(p) => p,
            None => return,
        };
        self.current_voice = Some(voice);
        self.current_link = None;
        self.param_ids = tl::group_param_ids("voice");
        self.param_label = format!("voice V{}", voice + 1);
        let voice_select = params::by_name("VOICE_SELECT").unwrap().id;
        if let Err(e) = self.bridge.set_parameter(voice_select, voice as i64) {
            self.status = format!("error: {e}");
            return;
        }
        match self
            .bridge
            .get_parameters(&self.param_ids, Some(self.timeout))
        {
            Ok(values) => self.param_values = values,
            Err(e) => {
                self.status = format!("error: {e}");
                return;
            }
        }
        let ov = self.walk_preset(preset);
        self.sample_rows = ov
            .ok()
            .and_then(|_| self.overviews.get(&preset).map(|o| o.sample_rows.clone()))
            .unwrap_or_default();
        self.borrow_rows.clear();
        self.params_cursor = 0;
        let pending = self.pending.clone();
        self.pending = None;
        self.status = pending.unwrap_or_else(|| {
            format!(
                "preset {preset}: voice V{}, {} sample(s) used",
                voice + 1,
                self.sample_rows.len()
            )
        });
    }

    fn select_link(&mut self, link: usize) {
        let preset = match self.current_preset {
            Some(p) => p,
            None => return,
        };
        self.current_voice = None;
        self.current_link = Some(link);
        self.param_ids = tl::group_param_ids("link");
        self.param_label = format!("link L{}", link + 1);
        let link_select = params::by_name("LINK_SELECT").unwrap().id;
        if let Err(e) = self.bridge.set_parameter(link_select, link as i64) {
            self.status = format!("error: {e}");
            return;
        }
        match self
            .bridge
            .get_parameters(&self.param_ids, Some(self.timeout))
        {
            Ok(values) => self.param_values = values,
            Err(e) => {
                self.status = format!("error: {e}");
                return;
            }
        }
        self.sample_rows.clear();
        self.borrow_rows.clear();
        self.params_cursor = 0;
        self.status = format!(
            "preset {preset}: {} parameters ({})",
            self.param_label,
            self.param_ids.len()
        );
    }

    // -- editing -------------------------------------------------------------
    fn current_scope(&self) -> (Option<usize>, Option<usize>, String) {
        if self.current_voice.is_some() {
            (
                self.current_voice,
                None,
                format!("V{}", self.current_voice.unwrap_or(0) + 1),
            )
        } else if self.current_link.is_some() {
            (
                None,
                self.current_link,
                format!("L{}", self.current_link.unwrap() + 1),
            )
        } else {
            (None, None, "global".into())
        }
    }

    fn record_change(&mut self, param_id: Option<u32>, old: ChangeValue, new: ChangeValue) {
        let (voice, link, scope) = self.current_scope();
        self.changes.push(Change {
            param_id,
            old,
            new,
            voice,
            link,
            scope,
        });
    }

    fn nudge(&mut self, delta: i64) {
        if self.param_ids.is_empty() || self.params_cursor >= self.param_ids.len() {
            self.status = "no parameter under the cursor".into();
            return;
        }
        if !self.allow_write {
            self.status = "writes disabled -- press 'w' to arm write mode".into();
            return;
        }
        let param_id = self.param_ids[self.params_cursor];
        let param = match params::by_id(param_id) {
            Some(p) => p,
            None => return,
        };
        let current = match self.bridge.get_parameter(param_id, Some(self.timeout)) {
            Ok(v) => v,
            Err(e) => {
                self.status = format!("error: {e}");
                return;
            }
        };
        let rng = match self.ranges.get(&param_id) {
            Some(r) => r.clone(),
            None => match self
                .bridge
                .get_parameter_range(param_id, Some(self.timeout))
            {
                Ok(r) => {
                    self.ranges.insert(param_id, r.clone());
                    r
                }
                Err(e) => {
                    self.status = format!("error: {e}");
                    return;
                }
            },
        };
        let target = (current + delta).clamp(rng.minimum, rng.maximum);
        if target == current {
            let edge = if delta > 0 { "maximum" } else { "minimum" };
            self.status = format!("{} already at its {} ({current})", param.name, edge);
            return;
        }
        if let Err(e) = self.bridge.set_parameter(param_id, target) {
            self.status = format!("error: {e}");
            return;
        }
        let ids = self.param_ids.clone();
        self.param_values = self
            .bridge
            .get_parameters(&ids, Some(self.timeout))
            .unwrap_or_default();
        self.invalidate_write_sensitive_caches();
        // Consecutive nudges of the same parameter in the same scope collapse
        // into one undo entry, keeping its ORIGINAL old value.
        let (voice, link, _scope) = self.current_scope();
        let collapse = self.changes.last().is_some_and(|last| {
            last.param_id == Some(param_id)
                && last.voice == voice
                && last.link == link
                && matches!(&last.new, ChangeValue::Int(v) if *v == current)
        });
        if collapse {
            let idx = self.changes.len() - 1;
            self.changes[idx].new = ChangeValue::Int(target);
        } else {
            self.record_change(
                Some(param_id),
                ChangeValue::Int(current),
                ChangeValue::Int(target),
            );
        }
        self.status = format!(
            "{} = {} (was {})",
            param.name,
            params::describe_value(param, target as i32),
            current
        );
    }

    fn apply_edit(&mut self, param_id: u32, value: i64) {
        if let Err(e) = self.bridge.set_parameter(param_id, value) {
            self.status = format!("error: {e}");
            return;
        }
        self.param_values = self
            .bridge
            .get_parameters(&self.param_ids, Some(self.timeout))
            .unwrap_or_default();
        self.invalidate_write_sensitive_caches();
        let (voice, link, scope) = self.current_scope();
        self.changes.push(Change {
            param_id: Some(param_id),
            old: ChangeValue::Int(value),
            new: ChangeValue::Int(value),
            voice,
            link,
            scope,
        });
        self.status = format!("set id {param_id} = {value}");
    }

    fn invalidate_write_sensitive_caches(&mut self) {
        // Any real write could change names, voice/zone references or more:
        // clear uniformly rather than reasoning about which cache is safe.
        self.overviews.clear();
        self.usage_index.clear();
        self.usage_scanned = false;
        self.names.clear();
        self.sample_cache.clear();
        self.ranges.clear();
    }

    fn undo(&mut self, count: usize) {
        if self.changes.is_empty() {
            self.status = "nothing to undo".into();
            return;
        }
        if !self.allow_write {
            self.status = "writes disabled -- press 'w' to arm write mode".into();
            return;
        }
        let preset = match self.current_preset {
            Some(p) => p,
            None => {
                self.status = "undo failed: no preset selected".into();
                return;
            }
        };
        let preset_select = params::by_name("PRESET_SELECT").unwrap().id;
        let voice_select = params::by_name("VOICE_SELECT").unwrap().id;
        let link_select = params::by_name("LINK_SELECT").unwrap().id;
        if let Err(e) = self.bridge.set_parameter(preset_select, preset as i64) {
            self.status = format!("undo failed: {e}");
            return;
        }
        let n = count.min(self.changes.len());
        let mut reverted: Vec<Change> = Vec::new();
        for _ in 0..n {
            let change = match self.changes.pop() {
                Some(c) => c,
                None => break,
            };
            // Restore the selection this edit was made under before writing.
            if change.voice.is_some() {
                if let Err(e) = self
                    .bridge
                    .set_parameter(voice_select, change.voice.unwrap_or(0) as i64)
                {
                    self.status = format!("undo failed: {e}");
                    return;
                }
            } else if change.link.is_some() {
                if let Err(e) = self
                    .bridge
                    .set_parameter(link_select, change.link.unwrap() as i64)
                {
                    self.status = format!("undo failed: {e}");
                    return;
                }
            }
            let result = match (&change.param_id, &change.old) {
                (None, ChangeValue::Name(n)) => self.bridge.set_preset_name(preset, n),
                (Some(pid), ChangeValue::Int(v)) => self.bridge.set_parameter(*pid, *v),
                _ => Ok(()),
            };
            if result.is_err() {
                self.status = format!("undo failed: {}", result.err().unwrap());
                return; // the log still describes what is applied
            }
            reverted.push(change);
        }
        self.invalidate_write_sensitive_caches();
        let message = if reverted.len() == 1 {
            let c = &reverted[0];
            let scope = if c.scope != "global" {
                format!(" [{}]", c.scope)
            } else {
                String::new()
            };
            format!(
                "reverted {}{} from {} to {} — Δ{} left",
                c.label(),
                scope,
                c.describe(&c.new),
                c.describe(&c.old),
                self.changes.len()
            )
        } else if !reverted.is_empty() {
            format!(
                "reverted {} change(s) on preset {} — back to as loaded",
                reverted.len(),
                preset
            )
        } else {
            "nothing to undo".into()
        };
        self.status = message.clone();
        self.pending = Some(message);
        self.reload_current_view();
    }

    fn reload_current_view(&mut self) {
        if self.current_preset.is_none() {
            return;
        }
        if self.current_voice.is_some() {
            self.select_voice(self.current_voice.unwrap());
        } else if self.current_link.is_some() {
            self.select_link(self.current_link.unwrap());
        } else {
            self.show_preset_overview();
        }
    }

    // -- sweeps -------------------------------------------------------------
    fn start_sweep(&mut self, depth: &str, kind: &str, subject: u32) {
        self.sweep = Some(tl::SweepState::new(depth, self.gap_setting));
        self.sweep_after = Some((kind.to_string(), subject));
    }

    fn cache_at_depth(&mut self, depth: &str) {
        if self.sweep.is_some() {
            self.status = "a scan is already running ('escape' to cancel)".into();
            return;
        }
        let estimate = self
            .bridge
            .preset_memory(Some(self.timeout))
            .ok()
            .and_then(|mem| {
                tl::estimate_sweep_seconds(
                    depth,
                    Some((mem.total_kb.max(mem.free_kb) - mem.free_kb) as u64),
                )
            });
        match estimate {
            Some(e) if e >= tl::SWEEP_CONFIRM_SECONDS => {
                self.mode = Mode::ConfirmSweep(depth.to_string());
                self.status = format!("cache ({depth}) would take roughly {} on this bank — Enter to confirm, Esc to cancel", tl::humanize_seconds(e));
            }
            _ => self.start_sweep(depth, "cache", 0),
        }
    }

    fn step_sweep(&mut self) {
        if let Some(sweep) = &mut self.sweep {
            let msg = sweep.step(&mut *self.bridge, Some(self.timeout));
            if let Some(m) = msg {
                self.status = m;
            }
            if sweep.done() {
                let (kind, subject) = self
                    .sweep_after
                    .clone()
                    .unwrap_or_else(|| ("cache".into(), 0));
                let cancelled = sweep.cancelled;
                let stopped_at = sweep.stopped_at;
                let note = sweep.sweep_note();
                let (preset_count, sample_count) =
                    (sweep.preset_names.len(), sweep.sample_names.len());
                if cancelled {
                    self.status = format!("scan cancelled at preset {stopped_at}, nothing cached");
                } else {
                    // A complete sweep's findings are authoritative: replace,
                    // never merge, so stale rows cannot linger.
                    self.overviews = sweep.overviews.clone();
                    self.usage_index = sweep.sample_index.clone();
                    self.usage_scanned = true;
                    for (&k, v) in &sweep.preset_names {
                        self.names.insert(k, v.clone());
                    }
                    for (&k, v) in &sweep.sample_names {
                        self.sample_cache.insert(k, v.clone());
                    }
                    match kind.as_str() {
                        "cache" => self.status = format!("cache all ({}): {preset_count} preset name(s), {sample_count} sample name(s){note}", sweep.depth),
                        "usage" => {
                            let matches = self.usage_index.get(&(subject as i64)).cloned().unwrap_or_default();
                            let mut where_s = matches.iter().take(5).map(|(p, n)| format!("{p} {n:?}")).collect::<Vec<_>>().join(", ");
                            if matches.len() > 5 { where_s = format!("{where_s}, +{} more — see the Parameters pane", matches.len() - 5); }
                            self.status = if matches.is_empty() { format!("sample {subject}: used by 0 preset(s){note}") } else { format!("sample {subject}: used by {} preset(s){note} — {where_s}", matches.len()) };
                            self.borrow_rows = matches.iter().map(|(p, n)| ("—".into(), tl::slot_label("preset", *p), n.clone(), String::new())).collect();
                            self.param_ids.clear();
                            self.param_label = format!("sample {subject} usage");
                        }
                        "integrity" => {
                            let findings = tl::dangling_sample_refs(&self.overviews);
                            if findings.is_empty() {
                                self.status = format!("no dangling sample references{note}");
                            } else {
                                let affected: std::collections::BTreeSet<u32> = findings.iter().map(|(p, _, _)| *p).collect();
                                let where_s = findings.iter().take(5).map(|(p, n, l)| format!("{} {} → {}", tl::slot_label("preset", *p), l, tl::slot_label("sample", *n as u32))).collect::<Vec<_>>().join(", ");
                                self.status = format!("{} dangling reference(s) across {} preset(s){note} — {where_s}", findings.len(), affected.len());
                                self.borrow_rows = findings.iter().map(|(p, n, l)| ("—".into(), tl::slot_label("preset", *p), l.clone(), format!("→ {} (missing)", tl::slot_label("sample", *n as u32)))).collect();
                                self.param_ids.clear();
                                self.param_label = "dangling sample references".into();
                            }
                        }
                        _ => {}
                    }
                }
                self.sweep = None;
                self.sweep_after = None;
            }
        }
    }

    // -- prompts -------------------------------------------------------------
    fn commit_prompt(&mut self) {
        let kind = self.mode.clone();
        if let Mode::Prompt(pk) = kind {
            match pk {
                PromptKind::Goto => match self.input.parse::<u32>() {
                    Ok(n) if n <= 999 => {
                        self.mode = Mode::Normal;
                        self.input.clear();
                        if self.bank == "preset" {
                            self.select_preset(n);
                        } else {
                            self.current_sample = Some(n);
                            self.status = format!(
                                "sample {}: {}",
                                tl::slot_label("sample", n),
                                self.names
                                    .get(&n)
                                    .cloned()
                                    .unwrap_or_else(|| "unknown".into())
                            );
                        }
                    }
                    _ => self.status = "enter a number 0-999".into(),
                },
                PromptKind::Voice => match self.input.parse::<usize>() {
                    Ok(n) if n >= 1 => {
                        self.mode = Mode::Normal;
                        self.input.clear();
                        self.select_voice(n - 1);
                    }
                    _ => self.status = "enter a voice number".into(),
                },
                PromptKind::Link => match self.input.parse::<usize>() {
                    Ok(n) if n >= 1 => {
                        self.mode = Mode::Normal;
                        self.input.clear();
                        self.select_link(n - 1);
                    }
                    _ => self.status = "enter a link number".into(),
                },
                PromptKind::Rename => {
                    let number = if self.bank == "preset" {
                        self.current_preset
                    } else {
                        self.current_sample
                    };
                    match number {
                        Some(n) => {
                            let previous = self.current_item_name();
                            let name = self.input.clone();
                            if name == previous {
                                self.status = "name unchanged".into();
                            } else {
                                if self.bank == "preset" {
                                    self.record_change(
                                        None,
                                        ChangeValue::Name(previous),
                                        ChangeValue::Name(name.clone()),
                                    );
                                    if let Err(e) = self.bridge.set_preset_name(n, &name) {
                                        self.status = format!("error: {e}");
                                        self.mode = Mode::Normal;
                                        return;
                                    }
                                } else {
                                    if let Err(e) = self.bridge.set_sample_name(n, &name) {
                                        self.status = format!("error: {e}");
                                        self.mode = Mode::Normal;
                                        return;
                                    }
                                }
                                self.invalidate_write_sensitive_caches();
                                self.load_bank_page(self.window_start);
                                self.status = format!("renamed {} {} to {:?}", self.bank, n, name);
                            }
                            self.mode = Mode::Normal;
                            self.input.clear();
                        }
                        None => self.status = "select a preset or sample first".into(),
                    }
                }
                PromptKind::EditValue(pid) => match self.input.parse::<i64>() {
                    Ok(v) => {
                        let rng = self.ranges.get(&pid).cloned();
                        if let Some(r) = rng {
                            let v = v.clamp(r.minimum, r.maximum);
                            self.mode = Mode::Normal;
                            self.input.clear();
                            self.apply_edit(pid, v);
                        } else {
                            self.status = "range not known; nudge instead".into();
                        }
                    }
                    Err(_) => self.status = "enter a number".into(),
                },
            }
        }
    }

    fn current_item_name(&mut self) -> String {
        let number = if self.bank == "preset" {
            self.current_preset
        } else {
            self.current_sample
        };
        match number {
            Some(n) => {
                if let Some(s) = self.names.get(&n) {
                    return s.clone();
                }
                if self.bank == "preset" {
                    self.bridge
                        .get_preset_name(n, Some(self.timeout))
                        .unwrap_or_default()
                } else {
                    self.bridge
                        .get_sample_name(n, Some(self.timeout))
                        .unwrap_or_default()
                }
            }
            None => String::new(),
        }
    }

    // -- master ------------------------------------------------------------
    fn fire_master(&mut self, action: &str) {
        if !self.allow_write {
            self.status = "writes disabled -- press 'w' to arm write mode".into();
            return;
        }
        match action {
            "delete_preset" => match self.current_preset {
                Some(p) => self.bridge.delete_preset(p),
                None => {
                    self.status = "select a preset first".into();
                    return;
                }
            },
            "erase_bank" => self.bridge.erase_ram_bank(),
            "erase_all_presets" => self.bridge.erase_all_ram_presets(),
            "erase_all_samples" => self.bridge.erase_all_ram_samples(),
            _ => {}
        }
        self.invalidate_write_sensitive_caches();
        self.load_bank_page(self.window_start);
        self.mode = Mode::Normal;
        self.status = format!("fired: {action}");
    }

    // -- front panel ----------------------------------------------------------
    fn open_panel(&mut self) {
        if self.demo {
            self.status = "front panel needs live hardware".into();
            return;
        }
        let port = match &self.send_port {
            Some(p) => p.clone(),
            None => {
                self.status = "panel needs a --port".into();
                return;
            }
        };
        let mut backend = MidirBackend::new();
        let outs = backend.list_out();
        let idx = match outs.iter().position(|n| n == &port) {
            Some(i) => i,
            None => {
                self.status = format!("no such output port {port:?}");
                return;
            }
        };
        let mut out = match backend.open_out(idx) {
            Ok(o) => o,
            Err(e) => {
                self.status = format!("could not open panel send port: {e}");
                return;
            }
        };
        let session = match open_session(self.device_id) {
            Ok(f) => f,
            Err(e) => {
                self.status = format!("panel session could not be opened: {e}");
                return;
            }
        };
        out.send(&session);
        let req = match request_screen(self.device_id) {
            Ok(f) => f,
            Err(e) => {
                self.status = format!("panel screen request failed: {e}");
                return;
            }
        };
        out.send(&req);
        let ins = backend.list_in();
        let iidx = match ins.iter().position(|n| n == &port) {
            Some(i) => i,
            None => {
                self.status = "no matching input port".into();
                return;
            }
        };
        let mut inp = match backend.open_in(iidx) {
            Ok(i) => i,
            Err(e) => {
                self.status = format!("could not open panel receive port: {e}");
                return;
            }
        };
        let mut bitmap = None;
        let deadline = std::time::Instant::now() + self.timeout;
        while bitmap.is_none() {
            if let Some(msg) = inp.poll() {
                if msg.len() > 500 && msg[5] == 0x50 {
                    bitmap = lcd::decode_display(&msg);
                }
            } else if std::time::Instant::now() >= deadline {
                break;
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        match bitmap {
            Some(bm) => {
                self.panel_lines = match self.panel_render.as_str() {
                    "half" => lcd::to_halfblocks(&bm),
                    "braille" => lcd::to_braille(&bm, false),
                    _ => lcd::to_quadrants(&bm),
                };
                self.mode = Mode::Panel;
                self.status = "front panel (Esc to exit) — F-keys need the write gate".into();
            }
            None => self.status = "panel screen reply timed out".into(),
        }
    }

    // -- key handling ---------------------------------------------------------
    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if self.mode != Mode::Normal {
            match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Normal;
                    self.input.clear();
                }
                KeyCode::Enter => {
                    if let Mode::ConfirmSweep(depth) = self.mode.clone() {
                        self.mode = Mode::Normal;
                        self.start_sweep(&depth, "cache", 0);
                    } else {
                        self.commit_prompt();
                    }
                }
                KeyCode::Char(c) if self.mode == Mode::Master => match c {
                    'd' => self.fire_master("delete_preset"),
                    'b' => self.fire_master("erase_bank"),
                    'p' => self.fire_master("erase_all_presets"),
                    's' => self.fire_master("erase_all_samples"),
                    _ => {}
                },
                KeyCode::Char(c)
                    if self.mode != Mode::Help
                        && self.mode != Mode::History
                        && self.mode != Mode::Panel
                        && (c == '-'
                            || c.is_ascii_digit()
                            || self.mode == Mode::Prompt(PromptKind::Rename)) =>
                {
                    self.input.push(c);
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.running = false,
            KeyCode::Char('p') => {
                self.bank = "preset".into();
                self.names.clear();
                self.load_bank_page(0);
            }
            KeyCode::Char('s') => {
                self.bank = "sample".into();
                self.names.clear();
                self.load_bank_page(0);
            }
            KeyCode::Char('g') => {
                self.mode = Mode::Prompt(PromptKind::Goto);
                self.input.clear();
                self.status = "goto (0-999): ".into();
            }
            KeyCode::Char('r') => self.load_bank_page(self.window_start),
            KeyCode::Char('o') => {
                if !self.allow_write {
                    self.status = "writes disabled -- press 'w' to arm write mode".into();
                    return;
                }
                if (self.bank == "preset" && self.current_preset.is_none())
                    || (self.bank == "sample" && self.current_sample.is_none())
                {
                    self.status = format!("select a {} first", self.bank);
                    return;
                }
                self.mode = Mode::Prompt(PromptKind::Rename);
                self.input = self.current_item_name().trim_end().to_string();
                self.status = format!(
                    "rename {} (Enter to apply, Esc to cancel): {}",
                    self.bank, self.input
                );
            }
            KeyCode::Char('v') => {
                if self.current_preset.is_none() {
                    self.status = "select a preset first".into();
                    return;
                }
                let preset = self.current_preset.unwrap();
                match self.walk_preset(preset) {
                    Ok(0) | Err(_) => self.status = "this preset has no voices".into(),
                    Ok(1) => self.select_voice(0),
                    Ok(count) => {
                        self.mode = Mode::Prompt(PromptKind::Voice);
                        self.input.clear();
                        self.status = format!("voice (V1-V{count}): ");
                    }
                }
            }
            KeyCode::Char('l') => {
                if self.current_preset.is_none() {
                    self.status = "select a preset first".into();
                    return;
                }
                let preset = self.current_preset.unwrap();
                match self.bridge.preset_num_links(preset, Some(self.timeout)) {
                    Ok(0) => self.status = "this preset has no links".into(),
                    Ok(1) => self.select_link(0),
                    Ok(count) => {
                        self.mode = Mode::Prompt(PromptKind::Link);
                        self.input.clear();
                        self.status = format!("link (1-{count}): ");
                    }
                    Err(e) => self.status = format!("error: {e}"),
                }
            }
            KeyCode::Char('u') => {
                if self.bank != "sample" || self.current_sample.is_none() {
                    self.status = "select a sample first".into();
                    return;
                }
                if self.sweep.is_some() {
                    self.status = "a scan is already running ('escape' to cancel)".into();
                    return;
                }
                let sample = self.current_sample.unwrap();
                if self.usage_scanned {
                    let matches = self
                        .usage_index
                        .get(&(sample as i64))
                        .cloned()
                        .unwrap_or_default();
                    self.borrow_rows = matches
                        .iter()
                        .map(|(p, n)| {
                            (
                                "—".into(),
                                tl::slot_label("preset", *p),
                                n.clone(),
                                String::new(),
                            )
                        })
                        .collect();
                    self.param_ids.clear();
                    self.param_label = format!("sample {sample} usage");
                    let where_s = matches
                        .iter()
                        .take(5)
                        .map(|(p, n)| format!("{p} {n:?}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.status = if matches.is_empty() {
                        format!("sample {sample}: used by 0 preset(s) (cached)")
                    } else {
                        format!(
                            "sample {sample}: used by {} preset(s) (cached) — {where_s}",
                            matches.len()
                        )
                    };
                } else {
                    self.start_sweep("structure", "usage", sample);
                }
            }
            KeyCode::Char('i') => {
                if self.sweep.is_some() {
                    self.status = "a scan is already running ('escape' to cancel)".into();
                    return;
                }
                if self.usage_scanned {
                    let findings = tl::dangling_sample_refs(&self.overviews);
                    if findings.is_empty() {
                        self.status = "no dangling sample references (cached)".into();
                    } else {
                        let affected: std::collections::BTreeSet<u32> =
                            findings.iter().map(|(p, _, _)| *p).collect();
                        self.borrow_rows = findings
                            .iter()
                            .map(|(p, n, l)| {
                                (
                                    "—".into(),
                                    tl::slot_label("preset", *p),
                                    l.clone(),
                                    format!("→ {} (missing)", tl::slot_label("sample", *n as u32)),
                                )
                            })
                            .collect();
                        self.param_ids.clear();
                        self.param_label = "dangling sample references".into();
                        self.status = format!(
                            "{} dangling reference(s) across {} preset(s) (cached)",
                            findings.len(),
                            affected.len()
                        );
                    }
                } else {
                    self.start_sweep("structure", "integrity", 0);
                }
            }
            KeyCode::Char('c') => self.cache_at_depth("structure"),
            KeyCode::Char('C') => self.cache_at_depth("full"),
            KeyCode::Char('x') => {
                if self.sweep.is_some() {
                    self.status = "a scan is running ('escape' to cancel first)".into();
                    return;
                }
                if self.usage_index.is_empty() && !self.usage_scanned {
                    self.status = "no sample-usage cache to clear".into();
                    return;
                }
                self.usage_index.clear();
                self.usage_scanned = false;
                self.status = "sample-usage cache cleared — next 'u' re-scans".into();
            }
            KeyCode::Char('e') => {
                self.extended = !self.extended;
            }
            KeyCode::Esc => {
                if self.sweep.is_some() {
                    self.status = "cancelling scan ...".into();
                } else if self.current_preset.is_none()
                    || (self.current_voice.is_none() && self.current_link.is_none())
                {
                } else {
                    self.current_voice = None;
                    self.current_link = None;
                    self.show_preset_overview();
                }
            }
            KeyCode::Char('k') => self.open_panel(),
            KeyCode::Char('m') => {
                self.mode = Mode::Master;
                self.status = "master (write-gated): d delete preset · b erase RAM bank · p erase all presets · s erase all samples · Esc back".into();
            }
            KeyCode::Char('w') => {
                self.allow_write = !self.allow_write;
                self.status = if self.allow_write {
                    "write gate armed".into()
                } else {
                    "write gate locked".into()
                };
            }
            KeyCode::Char('z') => self.undo(1),
            KeyCode::Char('Z') => {
                let n = self.changes.len();
                self.undo(n);
            }
            KeyCode::Char('h') => {
                if self.changes.is_empty() {
                    self.status = "no changes recorded on this preset".into();
                } else {
                    self.mode = Mode::History;
                }
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('+') | KeyCode::Char('=') => self.nudge(1),
            KeyCode::Char('-') => self.nudge(-1),
            KeyCode::Tab => {
                self.focus = if self.focus == Focus::Browser {
                    Focus::Params
                } else {
                    Focus::Browser
                }
            }
            KeyCode::Up => {
                if self.focus == Focus::Browser {
                    if self.browser_cursor > 0 {
                        self.browser_cursor -= 1;
                    }
                } else if self.params_cursor > 0 {
                    self.params_cursor -= 1;
                }
            }
            KeyCode::Down => {
                if self.focus == Focus::Browser {
                    let rows = self.browser_rows();
                    if self.browser_cursor + 1 < rows.len() {
                        self.browser_cursor += 1;
                    }
                    // Infinite scroll: approaching the bottom of a window that
                    // is not the end of the bank extends it.
                    else if rows.len() >= tl::BROWSER_MIN_WINDOW
                        && self.window_start + tl::BROWSER_MIN_WINDOW <= 999
                    {
                        self.load_bank_page(self.window_start + tl::BROWSER_EXTEND_CHUNK);
                        self.browser_cursor = tl::BROWSER_MIN_WINDOW;
                    }
                } else if self.params_cursor + 1 < self.param_ids.len() {
                    self.params_cursor += 1;
                }
            }
            KeyCode::Enter => {
                if self.focus == Focus::Browser {
                    self.select_row();
                } else if !self.param_ids.is_empty() && self.params_cursor < self.param_ids.len() {
                    let pid = self.param_ids[self.params_cursor];
                    let current = self.param_values.get(&pid).copied().unwrap_or(0);
                    if !self.allow_write {
                        self.status = "writes disabled -- press 'w' to arm write mode".into();
                        return;
                    }
                    let param = match params::by_id(pid) {
                        Some(p) => p,
                        None => return,
                    };
                    let rng = match self.bridge.get_parameter_range(pid, Some(self.timeout)) {
                        Ok(r) => {
                            self.ranges.insert(pid, r.clone());
                            r
                        }
                        Err(e) => {
                            self.status = format!("error: {e}");
                            return;
                        }
                    };
                    self.mode = Mode::Prompt(PromptKind::EditValue(pid));
                    self.input = current.to_string();
                    self.status = format!(
                        "value for {} ({}..{}): ",
                        param.name, rng.minimum, rng.maximum
                    );
                }
            }
            _ => {}
        }
    }

    // -- rendering -----------------------------------------------------------
    fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(area);
        let (header, body, status, hints) = (chunks[0], chunks[1], chunks[2], chunks[3]);

        let subtitle = format!(
            "{} · write {} · Δ{}",
            self.description,
            if self.allow_write { "ARMED" } else { "locked" },
            self.changes.len()
        );
        let header_block = Block::default().borders(Borders::ALL).title("eosed");
        frame.render_widget(Paragraph::new(subtitle).block(header_block), header);

        let widths: Vec<Constraint> = if self.extended {
            vec![
                Constraint::Percentage(25),
                Constraint::Percentage(45),
                Constraint::Percentage(30),
            ]
        } else {
            vec![Constraint::Percentage(30), Constraint::Percentage(70)]
        };
        let panes = Layout::horizontal(widths).split(body);

        let title = if self.bank == "preset" {
            "Presets"
        } else {
            "Samples"
        };
        let rows: Vec<Row> = self
            .browser_rows()
            .iter()
            .enumerate()
            .map(|(i, (n, name))| {
                let marker = if i == self.browser_cursor { ">" } else { " " };
                Row::new(vec![
                    format!("{marker} {}", tl::slot_label(&self.bank, *n)),
                    name.clone(),
                ])
            })
            .collect();
        let browser = Table::new(rows, [Constraint::Length(7), Constraint::Min(12)])
            .block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(browser, panes[0]);

        if self.mode == Mode::Panel {
            let panel = Paragraph::new(self.panel_lines.join("\n"))
                .block(Block::default().borders(Borders::ALL).title("front panel"));
            frame.render_widget(panel, panes[1]);
        } else {
            let param_rows: Vec<Row> = if !self.param_ids.is_empty() {
                self.param_ids
                    .iter()
                    .enumerate()
                    .map(|(i, &pid)| {
                        let param = params::by_id(pid).unwrap();
                        let value = self.param_values.get(&pid);
                        let display = value
                            .map(|v| params::describe_value_aligned(param, *v as i32))
                            .unwrap_or_default();
                        let marker = if i == self.params_cursor && self.focus == Focus::Params {
                            ">"
                        } else {
                            " "
                        };
                        Row::new(vec![
                            format!("{marker} {pid:>3}", pid = pid),
                            param.name.to_string(),
                            display,
                            param.unit.unwrap_or("").to_string(),
                        ])
                    })
                    .collect()
            } else {
                self.borrow_rows
                    .iter()
                    .map(|r| Row::new(vec![r.0.clone(), r.1.clone(), r.2.clone(), r.3.clone()]))
                    .collect()
            };
            let params_block = Block::default()
                .borders(Borders::ALL)
                .title(format!("parameters — {}", self.param_label));
            frame.render_widget(
                Table::new(
                    param_rows,
                    [
                        Constraint::Length(7),
                        Constraint::Min(14),
                        Constraint::Length(12),
                        Constraint::Length(6),
                    ],
                )
                .block(params_block),
                panes[1],
            );
            if self.extended {
                let sample_rows: Vec<Row> = self
                    .sample_rows
                    .iter()
                    .map(|(n, name, voices)| {
                        Row::new(vec![
                            tl::slot_label("sample", *n as u32),
                            name.clone(),
                            voices.clone(),
                        ])
                    })
                    .collect();
                frame.render_widget(
                    Table::new(
                        sample_rows,
                        [
                            Constraint::Length(6),
                            Constraint::Min(10),
                            Constraint::Length(8),
                        ],
                    )
                    .block(Block::default().borders(Borders::ALL).title("samples")),
                    panes[2],
                );
            }
        }

        frame.render_widget(Paragraph::new(self.status.clone()), status);
        let blocks: Vec<String> = HINT_BLOCKS.iter().map(|s| s.to_string()).collect();
        let legend = tl::wrap_blocks(&blocks, area.width as usize);
        let legend_lines: Vec<String> = legend.lines().take(2).map(|l| l.to_string()).collect();
        frame.render_widget(Paragraph::new(legend_lines.join("\n")), hints);
    }
}

fn capture_screens(a: &Args) {
    use ratatui::backend::TestBackend;
    let dir = "docs/screenshots";
    std::fs::create_dir_all(dir).unwrap();
    let fresh_app = || -> App {
        let mut settings = Settings::new("eosed", "nonexistent.toml");
        App::new(
            Box::new(Demo::new()),
            true,
            true,
            a.timeout,
            &mut settings,
            &a.config.clone(),
            a.panel_render.clone(),
            None,
        )
    };
    let shot = |app: &App, name: &str, w: u16| {
        let mut terminal = ratatui::Terminal::new(TestBackend::new(w, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let content = terminal.backend().buffer().content();
        let width = w as usize;
        let mut out = String::new();
        for chunk in content.chunks(width) {
            let line: String = chunk.iter().map(|c| c.symbol()).collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        std::fs::write(format!("{dir}/{name}.txt"), out).unwrap();
        println!("  wrote {name}.txt");
    };
    let mut app = fresh_app();
    app.select_preset(0);
    shot(&app, "compact_view", 100);
    let mut app2 = fresh_app();
    app2.select_preset(0);
    app2.select_voice(0);
    shot(&app2, "extended_view_voice", 110);
    let mut app3 = fresh_app();
    app3.select_preset(0);
    app3.mode = Mode::Master;
    shot(&app3, "master_menu", 100);
    let mut app4 = fresh_app();
    app4.select_preset(0);
    app4.record_change(Some(1), ChangeValue::Int(0), ChangeValue::Int(5));
    app4.mode = Mode::History;
    shot(&app4, "history", 100);
    // The panel shots in upstream use a scripted bitmap; ours render the
    // same synthetic 240x64 screen through the ported lcd encoders.
    let mut bitmap = vec![vec![0u8; 240]; 64];
    #[allow(clippy::needless_range_loop)] // rectangle fill mirrors upstream's scripted bitmap
    for r in 8..40 {
        for c in 10..60 {
            bitmap[r][c] = 1;
        }
    }
    #[allow(clippy::needless_range_loop)] // rectangle fill mirrors upstream's scripted bitmap
    for r in 20..50 {
        for c in 80..150 {
            bitmap[r][c] = 1;
        }
    }
    let half = lcd::to_halfblocks(&bitmap);
    std::fs::write(format!("{dir}/front_panel_halfblock.txt"), half.join("\n")).unwrap();
    println!("  wrote front_panel_halfblock.txt");
    let bra = lcd::to_braille(&bitmap, false);
    std::fs::write(format!("{dir}/front_panel_braille.txt"), bra.join("\n")).unwrap();
    println!("  wrote front_panel_braille.txt");
    println!("  edit_value screen: N/A (the ratatui port edits values inline, no separate dialog)");
}

fn main() {
    let a = parse_args();
    if a.capture {
        capture_screens(&a);
        return;
    }
    ratatui::run(move |terminal| {
        let mut settings = Settings::new("eosed", &a.config.clone());
        let _ = load_cache_depth(Some(&a.config.clone()), &mut settings);
        let app = if a.demo {
            App::new(
                Box::new(Demo::new()),
                true,
                true,
                a.timeout,
                &mut settings,
                &a.config.clone(),
                a.panel_render.clone(),
                None,
            )
        } else {
            let mut backend = MidirBackend::new();
            let bridge = if let Some(port) = &a.port {
                match standard_bridge(
                    &mut backend,
                    port,
                    a.gap,
                    a.write_gap,
                    a.device_id.unwrap_or(m::DEFAULT_DEVICE_ID),
                    a.timeout,
                ) {
                    Ok(b) => b,
                    Err(e) => die(&e.to_string()),
                }
            } else {
                match autodetect(
                    &mut backend,
                    a.gap,
                    a.write_gap,
                    a.device_id,
                    AUTODETECT_TIMEOUT,
                    Some(&a.config.clone()),
                    &mut settings,
                    &|name| eprintln!("  trying {name} ..."),
                ) {
                    Ok(b) => b,
                    Err(e) => die(&e.to_string()),
                }
            };
            let panel_port = a.port.clone().or_else(|| {
                eosed::config::load_last_ports(Some(&a.config.clone()), &mut settings)
                    .map(|(s, _)| s)
            });
            App::new(
                Box::new(bridge),
                a.allow_write,
                false,
                a.timeout,
                &mut settings,
                &a.config.clone(),
                a.panel_render.clone(),
                panel_port,
            )
        };
        let mut app = app;
        while app.running {
            terminal.draw(|frame| app.render(frame)).unwrap();
            if event::poll(Duration::from_millis(120)).unwrap_or(false) {
                if let Ok(Event::Key(k)) = event::read() {
                    app.handle_key(k);
                }
            }
            app.step_sweep();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn demo_render_shows_the_canned_bank() {
        let mut settings = Settings::new("eosed", "nonexistent.toml");
        let app = App::new(
            Box::new(Demo::new()),
            true,
            true,
            Duration::from_secs(2),
            &mut settings,
            "nonexistent.toml",
            "quadrant".into(),
            None,
        );
        let backend = TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("P000"));
        assert!(content.contains("Demo Grand Piano"));
        assert!(content.contains("eosed"));
    }

    #[test]
    fn demo_select_and_rename_flow() {
        let mut settings = Settings::new("eosed", "nonexistent.toml");
        let mut app = App::new(
            Box::new(Demo::new()),
            true,
            true,
            Duration::from_secs(2),
            &mut settings,
            "nonexistent.toml",
            "quadrant".into(),
            None,
        );
        app.select_row(); // first browser row = preset 0
        assert_eq!(app.current_preset, Some(0));
        assert!(app.status.contains("preset 0: global parameters"));
        app.input = "Renamed Demo".into();
        app.mode = Mode::Prompt(PromptKind::Rename);
        app.commit_prompt();
        assert_eq!(
            app.names.get(&0).map(|s| s.trim_end().to_string()),
            Some("Renamed Demo".to_string())
        );
        assert_eq!(app.changes.len(), 1);
    }

    #[test]
    fn write_gate_blocks_nudge() {
        let mut settings = Settings::new("eosed", "nonexistent.toml");
        let mut app = App::new(
            Box::new(Demo::new()),
            false,
            true,
            Duration::from_secs(2),
            &mut settings,
            "nonexistent.toml",
            "quadrant".into(),
            None,
        );
        app.select_preset(0);
        app.focus = Focus::Params;
        app.params_cursor = 0; // first global param
        app.nudge(1);
        assert!(app.status.contains("writes disabled"));
        app.allow_write = true;
        app.nudge(1);
        assert!(app.status.contains("(was"));
    }

    #[test]
    fn consecutive_nudges_collapse_into_one_undo_entry() {
        let mut settings = Settings::new("eosed", "nonexistent.toml");
        let mut app = App::new(
            Box::new(Demo::new()),
            true,
            true,
            Duration::from_secs(2),
            &mut settings,
            "nonexistent.toml",
            "quadrant".into(),
            None,
        );
        app.select_preset(0);
        app.params_cursor = 0;
        let pid = app.param_ids[0];
        let before = app.bridge.get_parameter(pid, None).unwrap();
        app.nudge(1);
        app.nudge(1);
        assert_eq!(
            app.changes.len(),
            1,
            "consecutive nudges of the same param collapse"
        );
        assert_eq!(app.changes[0].old, ChangeValue::Int(before));
        assert_eq!(app.changes[0].new, ChangeValue::Int(before + 2));
        app.undo(1);
        assert_eq!(app.bridge.get_parameter(pid, None).unwrap(), before);
    }

    #[test]
    fn history_lists_changes_with_scope() {
        let mut settings = Settings::new("eosed", "nonexistent.toml");
        let mut app = App::new(
            Box::new(Demo::new()),
            true,
            true,
            Duration::from_secs(2),
            &mut settings,
            "nonexistent.toml",
            "quadrant".into(),
            None,
        );
        app.select_preset(0);
        app.params_cursor = 0;
        app.nudge(2);
        app.mode = Mode::History;
        let mut terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("was"),
            "history screen must show the old value"
        );
    }
}
