use std::collections::HashSet;

use crossbeam_channel::Sender;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use crate::audio::MidiEvent;

/// Virtual piano using Amiga tracker keyboard layout.
///
/// Uses the Kitty keyboard protocol for press/release detection.
/// If the terminal doesn't support it, the piano is disabled.
pub struct VirtualPiano {
    base_octave: i8,
    held_keys: HashSet<KeyCode>,
    midi_tx: Sender<MidiEvent>,
    enabled: bool,
}

const VELOCITY: u8 = 100;

impl VirtualPiano {
    pub fn new(midi_tx: Sender<MidiEvent>, enabled: bool) -> Self {
        VirtualPiano {
            base_octave: 4,
            held_keys: HashSet::new(),
            midi_tx,
            enabled,
        }
    }

    pub fn handle_key_event(&mut self, event: KeyEvent) {
        if !self.enabled {
            return;
        }

        match event.kind {
            KeyEventKind::Press => {
                // Octave controls
                match event.code {
                    KeyCode::Char('[') => {
                        if self.base_octave > 0 {
                            self.base_octave -= 1;
                            log::info!("Piano: octave down → {}", self.base_octave);
                        }
                        return;
                    }
                    KeyCode::Char(']') => {
                        if self.base_octave < 8 {
                            self.base_octave += 1;
                            log::info!("Piano: octave up → {}", self.base_octave);
                        }
                        return;
                    }
                    _ => {}
                }

                // Dedup: ignore if already held
                if self.held_keys.contains(&event.code) {
                    return;
                }

                if let Some(note) = self.key_to_note(event.code) {
                    self.held_keys.insert(event.code);
                    // NoteOn: 0x90, note, velocity
                    let _ = self.midi_tx.send((0, [0x90, note, VELOCITY]));
                    log::info!("Piano: NoteOn note={note} ({})", note_name(note));
                }
            }
            KeyEventKind::Release => {
                if let Some(note) = self.key_to_note(event.code) {
                    self.held_keys.remove(&event.code);
                    // NoteOff: 0x80, note, 0
                    let _ = self.midi_tx.send((0, [0x80, note, 0]));
                    log::info!("Piano: NoteOff note={note} ({})", note_name(note));
                }
            }
            KeyEventKind::Repeat => {
                // Ignore repeats
            }
        }
    }

    /// Send NoteOff for all currently held keys.
    pub fn all_notes_off(&mut self) {
        let keys: Vec<KeyCode> = self.held_keys.drain().collect();
        for code in keys {
            if let Some(note) = self.key_to_note(code) {
                let _ = self.midi_tx.send((0, [0x80, note, 0]));
            }
        }
    }

    /// Map a key code to a MIDI note number using Amiga tracker layout.
    fn key_to_note(&self, code: KeyCode) -> Option<u8> {
        let (semitone_offset, octave_offset) = match code {
            // Lower row: base octave
            KeyCode::Char('z') | KeyCode::Char('Z') => (0, 0),
            KeyCode::Char('s') | KeyCode::Char('S') => (1, 0),
            KeyCode::Char('x') | KeyCode::Char('X') => (2, 0),
            KeyCode::Char('d') | KeyCode::Char('D') => (3, 0),
            KeyCode::Char('c') | KeyCode::Char('C') => (4, 0),
            KeyCode::Char('v') | KeyCode::Char('V') => (5, 0),
            KeyCode::Char('g') | KeyCode::Char('G') => (6, 0),
            KeyCode::Char('b') | KeyCode::Char('B') => (7, 0),
            KeyCode::Char('h') | KeyCode::Char('H') => (8, 0),
            KeyCode::Char('n') | KeyCode::Char('N') => (9, 0),
            KeyCode::Char('j') | KeyCode::Char('J') => (10, 0),
            KeyCode::Char('m') | KeyCode::Char('M') => (11, 0),
            KeyCode::Char(',') => (12, 0),
            KeyCode::Char('l') | KeyCode::Char('L') => (13, 0),
            KeyCode::Char('.') => (14, 0),
            KeyCode::Char(';') => (15, 0),
            KeyCode::Char('/') => (16, 0),

            // Upper row: base octave + 1
            KeyCode::Char('q') | KeyCode::Char('Q') => (0, 1),
            KeyCode::Char('2') => (1, 1),
            KeyCode::Char('w') | KeyCode::Char('W') => (2, 1),
            KeyCode::Char('3') => (3, 1),
            KeyCode::Char('e') | KeyCode::Char('E') => (4, 1),
            KeyCode::Char('r') | KeyCode::Char('R') => (5, 1),
            KeyCode::Char('5') => (6, 1),
            KeyCode::Char('t') | KeyCode::Char('T') => (7, 1),
            KeyCode::Char('6') => (8, 1),
            KeyCode::Char('y') | KeyCode::Char('Y') => (9, 1),
            KeyCode::Char('7') => (10, 1),
            KeyCode::Char('u') | KeyCode::Char('U') => (11, 1),
            KeyCode::Char('i') | KeyCode::Char('I') => (12, 1),
            KeyCode::Char('9') => (13, 1),
            KeyCode::Char('o') | KeyCode::Char('O') => (14, 1),
            KeyCode::Char('0') => (15, 1),
            KeyCode::Char('p') | KeyCode::Char('P') => (16, 1),

            _ => return None,
        };

        let midi_note =
            (self.base_octave as i16 + octave_offset) * 12 + semitone_offset as i16;

        if (0..=127).contains(&midi_note) {
            Some(midi_note as u8)
        } else {
            None
        }
    }
}

fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let octave = (note / 12) as i8 - 1;
    let name = NAMES[(note % 12) as usize];
    format!("{name}{octave}")
}
