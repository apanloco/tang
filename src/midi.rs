use std::collections::HashSet;
use std::sync::Arc;

use midir::{MidiInput, MidiInputConnection};

use crate::midi_monitor::MidiMonitor;
use crate::note_input::{Delivery, NoteInput};
use crate::source::Source;

/// The sources of the MIDI devices (P4a of `docs/patch-graph.md` §11.6): each device has a number of its own for as long as the program runs, handed out in the order the devices are
/// first seen, so that a device that is unplugged and plugged in again is the source it was (what a lane hears it by, and what it has sounding, stay with the number).
#[derive(Default)]
pub struct SourceTable {
    /// The names of the devices, in the order they were first seen: the device at place `n` is the source `Source::device(n)`.
    names: Vec<String>,
}

impl SourceTable {
    /// The source of the device called `name`: the one it was given when it was first seen, or the next that is free if it is new.
    pub fn source_of(&mut self, name: &str) -> Source {
        let nth = match self.names.iter().position(|seen| seen == name) {
            Some(nth) => nth,
            None => {
                self.names.push(name.to_string());
                self.names.len() - 1
            }
        };
        Source::device(nth)
    }
}

pub struct MidiManager {
    notes: NoteInput,
    device_filter: Option<String>,
    connections: Vec<MidiInputConnection<()>>,
    connected_names: HashSet<String>,
    monitor: Arc<MidiMonitor>,
    /// The source of each device that has been opened.
    sources: SourceTable,
}

impl MidiManager {
    pub fn new(notes: NoteInput, device_filter: Option<String>, monitor: Arc<MidiMonitor>) -> Self {
        MidiManager {
            notes,
            device_filter,
            connections: Vec::new(),
            connected_names: HashSet::new(),
            monitor,
            sources: SourceTable::default(),
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

            // Each device is a source of its own: what it plays is tagged with it, so that a lane can hear it or not.
            let source = self.sources.source_of(&name);
            let notes = self.notes.for_source(source);
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
                    log::info!("Opened MIDI input: {name} ({source})");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_has_the_source_it_was_given_when_it_was_first_seen_for_as_long_as_the_program_runs() {
        let mut table = SourceTable::default();
        let launchkey = table.source_of("Launchkey 25");
        let seaboard = table.source_of("Seaboard RISE");
        assert_eq!((launchkey, seaboard), (Source::device(0), Source::device(1)));
        assert_ne!(launchkey, Source::KEYS, "no device is the keys");
        assert_eq!(table.source_of("Launchkey 25"), launchkey, "plugged in again, it is the source it was");
        assert_eq!(table.source_of("Seaboard RISE"), seaboard);
        assert_eq!(table.source_of("IAC Driver Bus 1"), Source::device(2), "a device that is new has the next");
    }

    #[test]
    fn the_devices_past_the_last_source_share_it() {
        let mut table = SourceTable::default();
        let sources: Vec<Source> = (0..70).map(|n| table.source_of(&format!("device {n}"))).collect();
        assert_eq!(sources[61], Source::device(61));
        assert!(sources[..62].windows(2).all(|pair| pair[0] != pair[1]), "62 devices have a source each");
        assert!(sources[62..].iter().all(|&source| source == Source::LAST), "the rest share the last: {sources:?}");
        assert_eq!(table.source_of("device 3"), Source::device(3), "and the ones before it are what they were");
    }
}
