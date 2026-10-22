//! The one path notes take from a source to the audio thread; see
//! `docs/web-ui.md` §3.3.
//!
//! A hardware port's callback, the virtual piano and (later) a browser each used to
//! do the same three things for themselves: drop a note-on the Piano tab's Locked
//! mode forbids, keep the held-notes bitset the Piano tab shows, and put the message
//! on the channel the audio thread drains. `NoteInput` does them in one place. It
//! is `Clone` and lock-free, so every source holds its own copy and calls it from
//! whatever thread it runs on, the real-time MIDI callback included.
//!
//! A `NoteInput` is made for a source (`for_source`; `new` is the keys' own: the computer's
//! keys, the page's piano and keys and every browser): what it sends is tagged with it, so
//! that a lane can hear some sources and not others (P4a of `docs/patch-graph.md` §11.6).

use std::sync::Arc;

use crossbeam_channel::Sender;

use crate::audio::MidiEvent;
use crate::held_notes::HeldNotes;
use crate::piano_filter::PianoFilter;
use crate::source::Source;

/// What became of a message sent with `NoteInput::send`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// On the channel to the audio thread.
    Sent,
    /// A note-on the Locked mode drops: it sounds nowhere and is not held.
    Blocked,
    /// More than three bytes (system exclusive), or none: not a message the
    /// audio thread takes.
    Unsupported,
    /// The channel to the audio thread is full; the message is dropped.
    Full,
}

#[derive(Clone)]
pub struct NoteInput {
    tx: Sender<MidiEvent>,
    held: Arc<HeldNotes>,
    piano_filter: Arc<PianoFilter>,
    /// Who the messages sent are from.
    source: Source,
}

impl NoteInput {
    /// The note path of the keys (`Source::KEYS`): the computer's keys, the page's piano and keys, and every browser.
    pub fn new(tx: Sender<MidiEvent>, held: Arc<HeldNotes>, piano_filter: Arc<PianoFilter>) -> Self {
        NoteInput { tx, held, piano_filter, source: Source::KEYS }
    }

    /// The same path for another source: a MIDI device. The held notes and the Locked mode are the same, the messages are its own.
    pub fn for_source(&self, source: Source) -> NoteInput {
        NoteInput { source, ..self.clone() }
    }

    /// Who the messages that this sends are from.
    #[cfg_attr(not(test), expect(dead_code, reason = "the tests ask what a path is for; the program hands a device its path when it opens it"))]
    pub fn source(&self) -> Source {
        self.source
    }

    /// Send a MIDI message (one to three bytes: a note, a controller, a pitch bend,
    /// ...) to the audio thread, at the start of its next buffer.
    ///
    /// A note-on for a note the Locked mode forbids is dropped (`Blocked`); a
    /// note-off always passes, to end a note that began before the lock. A note-on
    /// and a note-off update the held notes (a note-on with velocity 0 is a note-off).
    /// A message shorter than three bytes is padded with zeros.
    pub fn send(&self, bytes: &[u8]) -> Delivery {
        if bytes.is_empty() || bytes.len() > 3 {
            return Delivery::Unsupported;
        }
        if bytes.len() == 3 {
            match bytes[0] & 0xF0 {
                0x90 if bytes[2] > 0 => {
                    // Locked mode: an off-scale note-on never reaches the audio thread
                    // or the held notes.
                    if self.piano_filter.block_note_on(bytes[1]) {
                        return Delivery::Blocked;
                    }
                    self.held.note_on(bytes[1]);
                }
                0x90 | 0x80 => self.held.note_off(bytes[1]),
                _ => {}
            }
        }
        // Timestamp 0: the start of the next buffer.
        let mut message = [0u8; 3];
        message[..bytes.len()].copy_from_slice(bytes);
        match self.tx.try_send(MidiEvent::from_source(self.source, 0, message)) {
            Ok(()) => Delivery::Sent,
            Err(_) => Delivery::Full,
        }
    }

    /// The notes that are held, as every source of notes keeps them: shared, so that a reader of them (the
    /// browsers' telemetry) sees what any source holds.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime tells the browsers which notes are held"))]
    pub fn held(&self) -> Arc<HeldNotes> {
        self.held.clone()
    }

    /// Let go of everything that sounds: on every channel the pedal goes up, then each note that is held ends (a plugin that has no ear for the controllers
    /// that follow still hears these), and then all notes off and all sound off go to every channel, and the held notes are none. They go to the audio thread at the
    /// start of its next buffer, as every message does. Returns how many of them the channel had no room for (it holds 1024, and a panic is 48 and the notes that are held).
    /// The messages are no source's (`Source::BROADCAST`), so that every lane hears them whatever the sources it hears are, and they are sent whichever source this is.
    pub fn panic(&self) -> usize {
        let mut lost = 0;
        let mut send = |message: [u8; 3]| {
            if self.tx.try_send(MidiEvent::from_source(Source::BROADCAST, 0, message)).is_err() {
                lost += 1;
            }
        };
        for channel in 0..16u8 {
            send([0xB0 | channel, 64, 0]);
        }
        for note in self.held.held() {
            send([0x80, note, 0]);
        }
        for controller in [123u8, 120] {
            for channel in 0..16u8 {
                send([0xB0 | channel, controller, 0]);
            }
        }
        self.held.clear();
        lost
    }

    /// A note-on on channel 1.
    pub fn note_on(&self, note: u8, velocity: u8) -> Delivery {
        self.send(&[0x90, note, velocity])
    }

    /// A note-off on channel 1.
    pub fn note_off(&self, note: u8) -> Delivery {
        self.send(&[0x80, note, 0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::piano_filter::PianoMode;
    use crate::scale::ScaleSetting;

    struct Rig {
        input: NoteInput,
        rx: crossbeam_channel::Receiver<MidiEvent>,
        held: Arc<HeldNotes>,
        filter: Arc<PianoFilter>,
    }

    fn rig_with_capacity(capacity: usize) -> Rig {
        let (tx, rx) = crossbeam_channel::bounded(capacity);
        let held = Arc::new(HeldNotes::new());
        // C major, so that C4 (60) is in the scale and C#4 (61) is not.
        let filter = Arc::new(PianoFilter::new(ScaleSetting::default(), PianoMode::Highlight));
        Rig { input: NoteInput::new(tx, held.clone(), filter.clone()), rx, held, filter }
    }

    fn rig() -> Rig {
        rig_with_capacity(16)
    }

    fn sent(rig: &Rig) -> Vec<MidiEvent> {
        rig.rx.try_iter().collect()
    }

    #[test]
    fn a_note_on_is_sent_and_held_and_a_note_off_ends_both() {
        let rig = rig();
        assert_eq!(rig.input.note_on(60, 100), Delivery::Sent);
        assert_eq!(sent(&rig), [(0, [0x90, 60, 100])]);
        assert_eq!(rig.held.held(), [60]);
        assert_eq!(rig.input.note_off(60), Delivery::Sent);
        assert_eq!(sent(&rig), [(0, [0x80, 60, 0])]);
        assert!(rig.held.held().is_empty());
    }

    #[test]
    fn a_note_on_with_no_velocity_is_a_note_off_and_is_passed_on_as_it_came() {
        let rig = rig();
        rig.input.note_on(60, 90);
        sent(&rig);
        assert_eq!(rig.input.send(&[0x90, 60, 0]), Delivery::Sent);
        assert_eq!(sent(&rig), [(0, [0x90, 60, 0])]);
        assert!(rig.held.held().is_empty());
    }

    #[test]
    fn a_note_off_on_any_channel_releases_the_note() {
        let rig = rig();
        rig.input.send(&[0x93, 64, 100]);
        assert_eq!(rig.held.held(), [64], "a note-on on channel 4");
        rig.input.send(&[0x83, 64, 0]);
        assert!(rig.held.held().is_empty(), "and its note-off");
    }

    #[test]
    fn in_locked_mode_an_off_scale_note_on_is_dropped_and_in_scale_ones_pass() {
        let rig = rig();
        rig.filter.set_mode(PianoMode::Locked);
        assert_eq!(rig.input.note_on(61, 100), Delivery::Blocked, "C# is not in C major");
        assert!(sent(&rig).is_empty());
        assert!(rig.held.held().is_empty(), "and it is not held either");
        assert_eq!(rig.input.note_on(60, 100), Delivery::Sent);
        assert_eq!(rig.held.held(), [60]);
    }

    #[test]
    fn a_note_off_passes_in_locked_mode_so_that_a_note_from_before_the_lock_ends() {
        let rig = rig();
        rig.input.note_on(61, 100);
        sent(&rig);
        rig.filter.set_mode(PianoMode::Locked);
        assert_eq!(rig.input.note_off(61), Delivery::Sent);
        assert_eq!(sent(&rig), [(0, [0x80, 61, 0])]);
        assert!(rig.held.held().is_empty());
    }

    #[test]
    fn highlight_mode_lets_everything_through() {
        let rig = rig();
        assert_eq!(rig.input.note_on(61, 100), Delivery::Sent);
        assert_eq!(rig.held.held(), [61]);
    }

    #[test]
    fn other_messages_pass_without_touching_the_held_notes() {
        let rig = rig();
        for message in [[0xB0u8, 1, 64], [0xE0, 0, 96], [0xD0, 50, 0], [0xA0, 60, 30]] {
            assert_eq!(rig.input.send(&message), Delivery::Sent);
        }
        assert_eq!(sent(&rig).len(), 4);
        assert!(rig.held.held().is_empty());
    }

    #[test]
    fn a_panic_lets_go_of_the_held_notes_and_says_pedal_up_all_notes_off_and_all_sound_off_on_every_channel() {
        let rig = rig_with_capacity(256);
        rig.input.note_on(64, 100);
        rig.input.note_on(60, 100);
        sent(&rig);
        assert_eq!(rig.input.panic(), 0);
        let mut expected: Vec<MidiEvent> = Vec::new();
        let to_all = |bytes: [u8; 3]| MidiEvent::from_source(Source::BROADCAST, 0, bytes);
        // The pedal goes up on every channel (so that the notes that follow are not held by it), each note that is held ends, and then all notes off and all sound off go to every channel.
        expected.extend((0..16u8).map(|channel| to_all([0xB0 | channel, 64, 0])));
        expected.extend([to_all([0x80, 60, 0]), to_all([0x80, 64, 0])]);
        expected.extend((0..16u8).map(|channel| to_all([0xB0 | channel, 123, 0])));
        expected.extend((0..16u8).map(|channel| to_all([0xB0 | channel, 120, 0])));
        assert_eq!(sent(&rig), expected);
        assert!(rig.held.held().is_empty(), "and nothing is held after it");
    }

    #[test]
    fn a_panic_with_nothing_held_is_the_forty_eight_controllers_only() {
        let rig = rig_with_capacity(256);
        assert_eq!(rig.input.panic(), 0);
        assert_eq!(sent(&rig).len(), 48);
    }

    #[test]
    fn a_panic_into_a_channel_that_is_full_says_how_many_messages_it_lost() {
        let rig = rig_with_capacity(10);
        assert_eq!(rig.input.panic(), 48 - 10);
        assert_eq!(sent(&rig).len(), 10);
    }

    #[test]
    fn a_short_message_is_padded_and_a_long_or_empty_one_is_not_a_message() {
        let rig = rig();
        assert_eq!(rig.input.send(&[0xC0, 5]), Delivery::Sent, "a program change has two bytes");
        assert_eq!(rig.input.send(&[0xF8]), Delivery::Sent, "a clock tick has one");
        assert_eq!(sent(&rig), [(0, [0xC0, 5, 0]), (0, [0xF8, 0, 0])]);
        assert_eq!(rig.input.send(&[0xF0, 1, 2, 3, 0xF7]), Delivery::Unsupported);
        assert_eq!(rig.input.send(&[0x90, 60, 100, 1]), Delivery::Unsupported, "four bytes are one too many");
        assert_eq!(rig.input.send(&[]), Delivery::Unsupported);
        assert!(sent(&rig).is_empty());
    }

    #[test]
    fn a_two_byte_note_message_is_sent_but_changes_no_held_note() {
        let rig = rig();
        assert_eq!(rig.input.send(&[0x90, 60]), Delivery::Sent);
        assert!(rig.held.held().is_empty(), "no velocity, so it is not a note");
    }

    #[test]
    fn a_full_channel_drops_the_message_and_says_so() {
        let rig = rig_with_capacity(1);
        assert_eq!(rig.input.note_on(60, 100), Delivery::Sent);
        assert_eq!(rig.input.note_on(62, 100), Delivery::Full);
        assert_eq!(sent(&rig).len(), 1);
    }

    #[test]
    fn a_closed_channel_drops_the_message_and_says_so() {
        let rig = rig();
        let Rig { input, rx, .. } = rig;
        drop(rx);
        assert_eq!(input.note_on(60, 100), Delivery::Full);
    }

    #[test]
    fn copies_of_a_note_input_share_the_channel_and_the_held_notes() {
        let rig = rig();
        let other = rig.input.clone();
        other.note_on(72, 100);
        assert_eq!(sent(&rig), [(0, [0x90, 72, 100])]);
        assert_eq!(rig.held.held(), [72]);
    }

    #[test]
    fn the_held_notes_it_keeps_are_the_ones_the_sources_share() {
        let rig = rig();
        assert!(Arc::ptr_eq(&rig.input.held(), &rig.held));
        rig.input.clone().note_on(64, 100);
        assert_eq!(rig.input.held().held(), [64]);
    }

    #[test]
    fn what_a_note_input_sends_is_from_the_keys_unless_it_was_made_for_another_source() {
        let rig = rig();
        assert_eq!(rig.input.source(), Source::KEYS);
        rig.input.note_on(60, 100);
        let launchkey = rig.input.for_source(Source::device(0));
        assert_eq!(launchkey.source(), Source::device(0));
        assert_eq!(rig.input.source(), Source::KEYS, "the one it was made from is what it was");
        launchkey.note_on(64, 90);
        launchkey.send(&[0xB0, 1, 64]);
        rig.input.note_off(60);
        assert_eq!(
            sent(&rig),
            [
                MidiEvent::from_source(Source::KEYS, 0, [0x90, 60, 100]),
                MidiEvent::from_source(Source::device(0), 0, [0x90, 64, 90]),
                MidiEvent::from_source(Source::device(0), 0, [0xB0, 1, 64]),
                MidiEvent::from_source(Source::KEYS, 0, [0x80, 60, 0]),
            ]
        );
    }

    #[test]
    fn the_paths_of_the_sources_share_the_held_notes_and_the_lock() {
        let rig = rig();
        let device = rig.input.for_source(Source::device(3));
        device.note_on(60, 100);
        assert_eq!(rig.held.held(), [60], "what a device holds is held for the Piano tab as what the keys hold is");
        rig.filter.set_mode(PianoMode::Locked);
        assert_eq!(device.note_on(61, 100), Delivery::Blocked, "C# is not in C major, whoever plays it");
        assert_eq!(sent(&rig).len(), 1);
        assert_eq!(device.note_off(60), Delivery::Sent);
        assert!(rig.held.held().is_empty());
    }

    #[test]
    fn a_panic_is_sent_as_no_source_whichever_source_it_is_asked_of() {
        let rig = rig_with_capacity(256);
        let device = rig.input.for_source(Source::device(1));
        device.note_on(64, 100);
        sent(&rig);
        assert_eq!(device.panic(), 0);
        let panic = sent(&rig);
        assert_eq!(panic.len(), 16 + 1 + 16 + 16);
        assert!(panic.iter().all(|event| event.source == Source::BROADCAST), "no lane is left out by what it hears");
    }

    #[test]
    fn a_note_number_past_127_is_not_held() {
        let rig = rig();
        rig.input.send(&[0x90, 130, 100]);
        assert!(rig.held.held().is_empty());
    }
}
