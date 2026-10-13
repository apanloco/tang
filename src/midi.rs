use std::collections::HashSet;
use std::sync::Arc;

use midir::{MidiInput, MidiInputConnection};

use crate::midi_monitor::MidiMonitor;
use crate::note_input::{Delivery, NoteInput};

pub struct MidiManager {
    notes: NoteInput,
    device_filter: Option<String>,
    connections: Vec<MidiInputConnection<()>>,
    connected_names: HashSet<String>,
    monitor: Arc<MidiMonitor>,
}

impl MidiManager {
    pub fn new(notes: NoteInput, device_filter: Option<String>, monitor: Arc<MidiMonitor>) -> Self {
        MidiManager {
            notes,
            device_filter,
            connections: Vec::new(),
            connected_names: HashSet::new(),
            monitor,
        }
    }

    /// Open all available MIDI input ports (or those matching the filter).
    /// Returns the number of newly opened connections.
    pub fn open_ports(&mut self) -> anyhow::Result<usize> {
        let midi_in = MidiInput::new("tang")?;
        let ports = midi_in.ports();
        let mut opened = 0;

        for port in &ports {
            let name = match midi_in.port_name(port) {
                Ok(n) => n,
                Err(_) => continue,
            };

            // Skip already connected
            if self.connected_names.contains(&name) {
                continue;
            }

            // Apply device filter
            if let Some(ref filter) = self.device_filter {
                if !name.contains(filter.as_str()) {
                    continue;
                }
            }

            let notes = self.notes.clone();
            let monitor = self.monitor.clone();
            let log_name = name.clone();
            let conn_name = name.clone();

            // Need a fresh MidiInput for each connection
            let midi_in_for_port = MidiInput::new("tang")?;
            match midi_in_for_port.connect(
                port,
                &conn_name,
                move |_timestamp_us, bytes, _| {
                    let status = bytes[0];
                    let kind = match status & 0xF0 {
                        0x90 => "NoteOn ",
                        0x80 => "NoteOff",
                        0xB0 => "CC     ",
                        0xE0 => "Bend   ",
                        0xD0 => "ChanPrs",
                        0xA0 => "KeyPrs ",
                        0xC0 => "PgmChg ",
                        _ => "Other  ",
                    };
                    let ch = status & 0x0F;
                    let note_info = match status & 0xF0 {
                        0x90 | 0x80 if bytes.len() >= 2 => {
                            format!(" {}", crate::note_name(bytes[1]))
                        }
                        _ => String::new(),
                    };
                    log::info!("MIDI in  [{log_name}] {kind} ch={ch}{note_info} data={bytes:02x?}");
                    // Feed the on-screen `--debug` monitor (no-op unless enabled).
                    monitor.record(bytes);
                    // Locked mode, the held notes and the channel to the audio
                    // thread: the note path every source shares.
                    if notes.send(bytes) == Delivery::Full {
                        log::warn!("MIDI channel full — dropping event from {log_name}");
                    }
                },
                (),
            ) {
                Ok(conn) => {
                    log::info!("Opened MIDI input: {name}");
                    self.connected_names.insert(name);
                    self.connections.push(conn);
                    opened += 1;
                }
                Err(e) => {
                    log::warn!("Failed to open MIDI input {name}: {e}");
                }
            }
        }

        Ok(opened)
    }

    /// Poll for newly connected MIDI devices. Call periodically from main loop.
    pub fn poll_new_devices(&mut self) {
        match self.open_ports() {
            Ok(0) => {}
            Ok(n) => log::info!("Opened {n} new MIDI device(s)"),
            Err(e) => log::warn!("MIDI poll error: {e}"),
        }
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }
}
