// SPDX-License-Identifier: GPL-2.0-or-later
// Real MIDI transport: midir (CoreMIDI on macOS, ALSA on Linux, WinMM on
// Windows) implementing the same Backend/InPort/OutPort traits the engine
// runs against in tests.
//
// Incoming SysEx is collected by the midir callback into a queue; poll()
// pops complete messages off it. That mirrors rtmidi's non-blocking
// receive_message() closely enough for the request/reply sequencing in
// bridge.rs. midir delivers whole messages, so no SysEx reassembly is
// needed.
//
// midir 0.11 takes the client by value in connect(), so each open builds a
// fresh client — the same lifecycle the Python side uses with transient
// rtmidi probes (and, like its leak-fix comment, keeps no dangling clients:
// dropping the connection closes it).

use crate::bridge::{Backend, BridgeError, InPort, OutPort, Result};
use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

type Queue = Arc<Mutex<VecDeque<Vec<u8>>>>;

struct QueuedIn {
    // Keeping the connection alive IS the open port; dropping it closes it.
    _conn: MidiInputConnection<Queue>,
    queue: Queue,
}

impl InPort for QueuedIn {
    fn poll(&mut self) -> Option<Vec<u8>> {
        match self.queue.lock() {
            Ok(mut q) => q.pop_front(),
            Err(_) => None, // poisoned lock: treat as no message rather than crash the engine
        }
    }
}

struct DirectOut {
    conn: MidiOutputConnection,
}

impl OutPort for DirectOut {
    fn send(&mut self, message: &[u8]) {
        // The MidiIo contract has no error channel; report on stderr and let
        // the reply timeout surface the failure in the engine.
        if let Err(e) = self.conn.send(message) {
            eprintln!("midi: send failed: {e}");
        }
    }
}

pub struct MidirBackend {
    client_name: String,
}

impl Default for MidirBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MidirBackend {
    pub fn new() -> MidirBackend {
        MidirBackend {
            client_name: "eosed-rs".to_string(),
        }
    }
}

impl Backend for MidirBackend {
    fn list_in(&mut self) -> Vec<String> {
        match MidiInput::new(&self.client_name) {
            Ok(input) => input
                .ports()
                .iter()
                .filter_map(|p| input.port_name(p).ok())
                .collect(),
            Err(e) => {
                eprintln!("warning: no MIDI backend available on this host (input: {e})");
                Vec::new()
            }
        }
    }

    fn list_out(&mut self) -> Vec<String> {
        match MidiOutput::new(&self.client_name) {
            Ok(output) => output
                .ports()
                .iter()
                .filter_map(|p| output.port_name(p).ok())
                .collect(),
            Err(e) => {
                eprintln!("warning: no MIDI backend available on this host (output: {e})");
                Vec::new()
            }
        }
    }

    fn open_in(&mut self, index: usize) -> Result<Box<dyn InPort>> {
        let input = MidiInput::new(&self.client_name).map_err(|e| {
            BridgeError::Protocol(format!(
                "no MIDI backend available on this host (input: {e})"
            ))
        })?;
        let ports = input.ports();
        let port = ports.get(index).ok_or_else(|| {
            BridgeError::Protocol(format!(
                "no input port at index {index} (have {})",
                ports.len()
            ))
        })?;
        let name = input
            .port_name(port)
            .unwrap_or_else(|_| format!("input {index}"));
        let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
        let conn = input
            .connect(
                port,
                &name,
                |_timestamp, message, data: &mut Queue| {
                    if let Ok(mut q) = data.lock() {
                        q.push_back(message.to_vec());
                    }
                },
                queue.clone(),
            )
            .map_err(|e| BridgeError::Protocol(format!("could not open input {name:?}: {e}")))?;
        Ok(Box::new(QueuedIn { _conn: conn, queue }))
    }

    fn open_out(&mut self, index: usize) -> Result<Box<dyn OutPort>> {
        let output = MidiOutput::new(&self.client_name).map_err(|e| {
            BridgeError::Protocol(format!(
                "no MIDI backend available on this host (output: {e})"
            ))
        })?;
        let ports = output.ports();
        let port = ports.get(index).ok_or_else(|| {
            BridgeError::Protocol(format!(
                "no output port at index {index} (have {})",
                ports.len()
            ))
        })?;
        let name = output
            .port_name(port)
            .unwrap_or_else(|_| format!("output {index}"));
        let conn = output
            .connect(port, &name)
            .map_err(|e| BridgeError::Protocol(format!("could not open output {name:?}: {e}")))?;
        Ok(Box::new(DirectOut { conn }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumeration_does_not_panic() {
        // A host with no MIDI subsystem at all must still enumerate cleanly
        // (two empty lists), exactly as the Python backend distinguishes
        // "nothing plugged in" from a hard failure.
        let mut backend = MidirBackend::new();
        let _ins = backend.list_in();
        let _outs = backend.list_out();
    }
}
