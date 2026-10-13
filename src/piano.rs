use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use crate::note_input::{Delivery, NoteInput};

/// Virtual piano using Amiga tracker keyboard layout.
///
/// Uses the Kitty keyboard protocol for press/release detection.
/// If the terminal doesn't support it, the piano is disabled.
pub struct VirtualPiano {
    base_octave: i8,
    held_keys: HashSet<KeyCode>,
    notes: NoteInput,
    enabled: bool,
}

const VELOCITY: u8 = 100;

impl VirtualPiano {
    pub fn new(notes: NoteInput, enabled: bool) -> Self {
        VirtualPiano {
            base_octave: 4,
            held_keys: HashSet::new(),
            notes,
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
                    // Locked mode drops an off-scale note-on (silent — no sound, no
                    // held visualization), and then the key is not held either.
                    if self.notes.note_on(note, VELOCITY) == Delivery::Blocked {
                        return;
                    }
                    self.held_keys.insert(event.code);
                    log::info!("Piano: NoteOn note={note} ({})", note_name(note));
                }
            }
            KeyEventKind::Release => {
                if let Some(note) = self.key_to_note(event.code) {
                    self.held_keys.remove(&event.code);
                    self.notes.note_off(note);
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
                self.notes.note_off(note);
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

        let midi_note = (self.base_octave as i16 + octave_offset) * 12 + semitone_offset as i16;

        if (0..=127).contains(&midi_note) {
            Some(midi_note as u8)
        } else {
            None
        }
    }
}

fn note_name(note: u8) -> String {
    crate::note_name(note)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::audio::MidiEvent;
    use crate::held_notes::HeldNotes;
    use crate::piano_filter::{PianoFilter, PianoMode};
    use crate::scale::ScaleSetting;

    struct Rig {
        piano: VirtualPiano,
        rx: crossbeam_channel::Receiver<MidiEvent>,
        held: Arc<HeldNotes>,
        filter: Arc<PianoFilter>,
    }

    fn rig(enabled: bool) -> Rig {
        let (tx, rx) = crossbeam_channel::bounded(64);
        let held = Arc::new(HeldNotes::new());
        let filter = Arc::new(PianoFilter::new(ScaleSetting::default(), PianoMode::Highlight));
        let piano = VirtualPiano::new(NoteInput::new(tx, held.clone(), filter.clone()), enabled);
        Rig { piano, rx, held, filter }
    }

    fn press(rig: &mut Rig, key: char) {
        rig.piano.handle_key_event(KeyEvent::new_with_kind(KeyCode::Char(key), KeyModifiers::NONE, KeyEventKind::Press));
    }

    fn release(rig: &mut Rig, key: char) {
        rig.piano.handle_key_event(KeyEvent::new_with_kind(KeyCode::Char(key), KeyModifiers::NONE, KeyEventKind::Release));
    }

    fn sent(rig: &Rig) -> Vec<MidiEvent> {
        rig.rx.try_iter().collect()
    }

    #[test]
    fn a_key_sounds_its_note_while_it_is_down() {
        let mut rig = rig(true);
        press(&mut rig, 'z');
        assert_eq!(sent(&rig), [(0, [0x90, 48, 100])], "the lower row starts at C3, MIDI 48");
        assert_eq!(rig.held.held(), [48]);
        release(&mut rig, 'z');
        assert_eq!(sent(&rig), [(0, [0x80, 48, 0])]);
        assert!(rig.held.held().is_empty());
    }

    #[test]
    fn a_key_can_be_played_again_once_it_is_released() {
        let mut rig = rig(true);
        press(&mut rig, 'z');
        release(&mut rig, 'z');
        sent(&rig);
        press(&mut rig, 'z');
        assert_eq!(sent(&rig), [(0, [0x90, 48, 100])]);
    }

    #[test]
    fn the_upper_row_is_an_octave_above_the_lower() {
        let mut rig = rig(true);
        press(&mut rig, 'q');
        press(&mut rig, 'w');
        assert_eq!(sent(&rig), [(0, [0x90, 60, 100]), (0, [0x90, 62, 100])]);
    }

    #[test]
    fn a_key_held_down_sounds_once_and_a_repeat_is_ignored() {
        let mut rig = rig(true);
        press(&mut rig, 'x');
        press(&mut rig, 'x');
        rig.piano.handle_key_event(KeyEvent::new_with_kind(KeyCode::Char('x'), KeyModifiers::NONE, KeyEventKind::Repeat));
        assert_eq!(sent(&rig).len(), 1);
    }

    #[test]
    fn the_octave_keys_move_the_keyboard() {
        let mut rig = rig(true);
        press(&mut rig, ']');
        press(&mut rig, 'z');
        assert_eq!(sent(&rig), [(0, [0x90, 60, 100])], "an octave up");
        press(&mut rig, '[');
        press(&mut rig, '[');
        release(&mut rig, 'z');
        sent(&rig);
        press(&mut rig, 'c');
        assert_eq!(sent(&rig), [(0, [0x90, 40, 100])], "and two down from there: E");
    }

    #[test]
    fn a_keyboard_the_terminal_cannot_report_is_silent() {
        let mut rig = rig(false);
        press(&mut rig, 'z');
        release(&mut rig, 'z');
        assert!(sent(&rig).is_empty());
        assert!(rig.held.held().is_empty());
    }

    #[test]
    fn a_key_with_no_note_does_nothing() {
        let mut rig = rig(true);
        press(&mut rig, 'a');
        press(&mut rig, '1');
        assert!(sent(&rig).is_empty());
    }

    #[test]
    fn in_locked_mode_an_off_scale_key_is_silent_and_an_in_scale_one_sounds() {
        let mut rig = rig(true);
        rig.filter.set_mode(PianoMode::Locked);
        press(&mut rig, 's'); // C# in octave 4: not in C major
        assert!(sent(&rig).is_empty());
        assert!(rig.held.held().is_empty());
        rig.piano.all_notes_off();
        assert!(sent(&rig).is_empty(), "the key that sounded nothing is not down");
        press(&mut rig, 'x'); // D
        assert_eq!(sent(&rig), [(0, [0x90, 50, 100])]);
    }

    #[test]
    fn all_notes_off_ends_every_key_that_is_down() {
        let mut rig = rig(true);
        press(&mut rig, 'z');
        press(&mut rig, 'c');
        sent(&rig);
        rig.piano.all_notes_off();
        let mut off = sent(&rig);
        off.sort();
        assert_eq!(off, [(0, [0x80, 48, 0]), (0, [0x80, 52, 0])]);
        assert!(rig.held.held().is_empty());
        rig.piano.all_notes_off();
        assert!(sent(&rig).is_empty(), "and then nothing is down");
    }
}
