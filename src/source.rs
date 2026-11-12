//! Where a MIDI message came from (P4a of `docs/patch-graph.md` §11.6): the number that an event carries from the program's inputs to the audio thread, the set of them that a lane
//! hears, and the events of a block as the engine is given them.
//!
//! A source is the computer's keys (`Source::KEYS`: the keys, the page's piano and keys, and every browser, which are one `NoteInput` path) or one MIDI device, which has a number of its
//! own for as long as the program runs (`midi::SourceTable`). A lane has the set of sources it hears (`SourceSet`, every source until it is told otherwise). A message that is sent as
//! `Source::BROADCAST` is no source's and is heard by every lane whatever its set is: the panic.
//!
//! The first 48 numbers are the **inputs** (the keys and the devices), the next 16 are the **pattern boxes'** (P4h) and the 32 after them are the **note splitters'** (P4i, two for each: the pick and the rest): a box's notes and a
//! splitter's are made inside the engine and carry the box's or the splitter's number, so that a lane hears them by the same set that says which inputs it hears, and `SourceSet::ALL`, what a lane hears until it is told
//! otherwise, is the inputs: a box or a splitter is heard by the lanes whose set has it and by no other.

use std::fmt;

/// How many sources there can be: a set of them is a `u128`.
pub const MAX_SOURCES: usize = INPUT_SOURCES + MAX_BOXES + SPLITTER_SOURCES;

/// How many of the sources are inputs, the computer's keys and the devices (the numbers 0 to 47): the others are the pattern boxes' (P4h) and the note splitters' (P4i).
pub const INPUT_SOURCES: usize = 48;

/// How many pattern boxes a session can have: each has a source of its own, the numbers 48 to 63.
pub const MAX_BOXES: usize = 16;

/// How many note splitters a session can have, and how many sources they have (two each: the pick and the rest, the numbers 64 to 95).
pub const MAX_SPLITTERS: usize = 16;
pub const SPLITTER_SOURCES: usize = 2 * MAX_SPLITTERS;

/// How many of the sources a note splitter can hear: the inputs and the pattern boxes, the numbers 0 to 63 (**never another splitter**: a splitter that heard another could make a loop).
pub const HEARD_BY_SPLITTERS: usize = INPUT_SOURCES + MAX_BOXES;

/// Which of the two outputs of a note splitter a source is: the one that has the note that the splitter picked (the lowest, or the highest), or the one that has the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Output {
    Pick,
    Rest,
}

/// A source of MIDI messages: the computer's keys, a device, a pattern box or an output of a note splitter. A number below `MAX_SOURCES`, or `BROADCAST`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Source(u8);

impl Source {
    /// The computer's keys, the page's piano and keys and every browser's: the source of what `NoteInput::new` sends, and of every event that is not said to be another's.
    pub const KEYS: Source = Source(0);
    /// The last input there is, which the devices beyond the 46th share (`Source::device`).
    pub const LAST_INPUT: Source = Source(INPUT_SOURCES as u8 - 1);
    /// No source's: what is heard by every lane, whatever the set of sources it hears is. A panic is sent as this, so that it reaches a lane that does not hear the keys.
    pub const BROADCAST: Source = Source(u8::MAX);

    /// The source of the device that was seen `nth`, counting from 0 (the keys are another): the 47th and the ones after it are all `Source::LAST_INPUT`.
    pub const fn device(nth: usize) -> Source {
        if nth + 1 >= INPUT_SOURCES { Source::LAST_INPUT } else { Source(nth as u8 + 1) }
    }

    /// The source of the pattern box that has the number `nth`, 0 to 15 (`MAX_BOXES`; a session refuses more, and a number past it is the last box's).
    pub const fn pattern(nth: usize) -> Source {
        if nth >= MAX_BOXES { Source((INPUT_SOURCES + MAX_BOXES) as u8 - 1) } else { Source((INPUT_SOURCES + nth) as u8) }
    }

    /// The source of an output of the note splitter that has the number `nth`, 0 to 15 (`MAX_SPLITTERS`; a session refuses more, and a number past it is the last splitter's): the numbers 64 to 95, the pick and then the rest of each.
    #[cfg_attr(not(test), expect(dead_code, reason = "the controller makes the note splitters in step 127 (P4i-B): until then the engine has them and nothing asks for one"))]
    pub const fn splitter(nth: usize, output: Output) -> Source {
        let nth = if nth >= MAX_SPLITTERS { MAX_SPLITTERS - 1 } else { nth };
        Source((INPUT_SOURCES + MAX_BOXES + 2 * nth + output as usize) as u8)
    }

    /// Whether this is a pattern box's.
    #[cfg(test)]
    pub const fn is_pattern(self) -> bool {
        self.0 as usize >= INPUT_SOURCES && (self.0 as usize) < INPUT_SOURCES + MAX_BOXES
    }

    /// Whether this is an output of a note splitter, and which: the splitter's number and the output.
    #[cfg_attr(not(test), expect(dead_code, reason = "the controller makes the note splitters in step 127 (P4i-B): until then the engine has them and nothing asks for one"))]
    pub const fn splitter_output(self) -> Option<(usize, Output)> {
        let first = INPUT_SOURCES + MAX_BOXES;
        if (self.0 as usize) < first || self.0 as usize >= MAX_SOURCES {
            return None;
        }
        let at = self.0 as usize - first;
        Some((at / 2, if at.is_multiple_of(2) { Output::Pick } else { Output::Rest }))
    }

    /// Its number, 0 for the keys: below `MAX_SOURCES`, or 255 for `BROADCAST`.
    pub const fn number(self) -> u8 {
        self.0
    }

    /// Whether this is `BROADCAST`.
    pub const fn is_broadcast(self) -> bool {
        self.0 == u8::MAX
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            0 => write!(f, "the keys"),
            u8::MAX => write!(f, "every source"),
            n if n as usize >= INPUT_SOURCES + MAX_BOXES => {
                let at = n as usize - INPUT_SOURCES - MAX_BOXES;
                write!(f, "the {} of splitter {}", if at.is_multiple_of(2) { "pick" } else { "rest" }, at / 2 + 1)
            }
            n if n as usize >= INPUT_SOURCES => write!(f, "pattern box {}", n as usize - INPUT_SOURCES + 1),
            n => write!(f, "device {n}"),
        }
    }
}

/// A set of sources, one bit for each: what a lane hears.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceSet(u128);

impl SourceSet {
    /// Every input, the keys and the devices, those that are not there yet too: what a lane hears until it is told otherwise. **A pattern box and a splitter are not one of them**: their notes are heard by the lanes whose set has their sources, which a cable says.
    pub const ALL: SourceSet = SourceSet::first(INPUT_SOURCES);
    /// No source.
    pub const NONE: SourceSet = SourceSet(0);

    /// The sources numbered 0 up to `count`: the inputs, say, or the sources that a note splitter can hear.
    pub const fn first(count: usize) -> SourceSet {
        SourceSet(if count >= 128 { u128::MAX } else { (1u128 << count) - 1 })
    }

    /// The set that has the sources whose bits `bits` has (bit `n` is the source numbered `n`).
    #[cfg(test)]
    pub const fn from_bits(bits: u128) -> SourceSet {
        SourceSet(bits)
    }

    /// The bits of the set.
    #[cfg(test)]
    pub const fn bits(self) -> u128 {
        self.0
    }

    /// Whether `source` is in the set. `BROADCAST` is in none: it is heard whatever the set is, and no set is made of it.
    pub const fn contains(self, source: Source) -> bool {
        source.0 < MAX_SOURCES as u8 && self.0 & (1u128 << source.0) != 0
    }

    /// The set and `source`. `BROADCAST` is no source, and the set is as it was.
    pub const fn with(self, source: Source) -> SourceSet {
        if source.0 < MAX_SOURCES as u8 { SourceSet(self.0 | 1u128 << source.0) } else { self }
    }

    /// The set without `source`.
    pub const fn without(self, source: Source) -> SourceSet {
        if source.0 < MAX_SOURCES as u8 { SourceSet(self.0 & !(1u128 << source.0)) } else { self }
    }

    /// The sources of `self` that `other` has not.
    pub const fn minus(self, other: SourceSet) -> SourceSet {
        SourceSet(self.0 & !other.0)
    }

    /// The sources that both have.
    pub const fn and(self, other: SourceSet) -> SourceSet {
        SourceSet(self.0 & other.0)
    }

    /// The sources that either has.
    pub const fn or(self, other: SourceSet) -> SourceSet {
        SourceSet(self.0 | other.0)
    }

    /// Whether the set has no source.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The sources in the set, the lowest number first.
    pub fn iter(self) -> impl Iterator<Item = Source> {
        (0..MAX_SOURCES as u8).filter(move |&n| self.0 & (1u128 << n) != 0).map(Source)
    }
}

/// A MIDI message on its way from a source to the audio thread: the bytes, where in the next block it goes (0, the start of it, for what comes in live), and who sent it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MidiEvent {
    pub source: Source,
    pub frame: u64,
    pub bytes: [u8; 3],
}

impl MidiEvent {
    /// A message from the keys.
    pub const fn keys(frame: u64, bytes: [u8; 3]) -> MidiEvent {
        MidiEvent { source: Source::KEYS, frame, bytes }
    }

    /// A message from `source`.
    pub const fn from_source(source: Source, frame: u64, bytes: [u8; 3]) -> MidiEvent {
        MidiEvent { source, frame, bytes }
    }
}

/// A frame and bytes with no source are from the keys.
impl From<(u64, [u8; 3])> for MidiEvent {
    fn from((frame, bytes): (u64, [u8; 3])) -> MidiEvent {
        MidiEvent::keys(frame, bytes)
    }
}

/// Equal to the frame and bytes of a message from the keys (so that a test can say what was sent by the frame and the bytes, as it did before there were sources).
impl PartialEq<(u64, [u8; 3])> for MidiEvent {
    fn eq(&self, other: &(u64, [u8; 3])) -> bool {
        *self == MidiEvent::keys(other.0, other.1)
    }
}

/// The events of a block as the engine is given them: each with its frame in the block and its bytes, as the plugins are given them, and beside them the source of each. No sources
/// at all says that every event is from the keys, so that what has no use for a source (a plugin, a test that plays a chord) says its events as it always did.
#[derive(Clone, Copy, Debug)]
pub struct Events<'a> {
    events: &'a [(u64, [u8; 3])],
    sources: &'a [Source],
}

impl<'a> Events<'a> {
    /// Events that are all from the keys.
    #[cfg_attr(not(test), expect(dead_code, reason = "the tests play notes that are all from the keys; the program has the sources of its events"))]
    pub const fn keys(events: &'a [(u64, [u8; 3])]) -> Events<'a> {
        Events { events, sources: &[] }
    }

    /// Events and the source of each (`sources` is as long as `events`; an event that has none is from the keys).
    pub fn new(events: &'a [(u64, [u8; 3])], sources: &'a [Source]) -> Events<'a> {
        debug_assert!(sources.is_empty() || sources.len() == events.len(), "a source for each event, or none");
        Events { events, sources }
    }

    /// How many events there are.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether there are none.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// The frame and the bytes of every event, whatever its source: for what hears every source (the modulators that belong to no lane).
    pub fn all(&self) -> &'a [(u64, [u8; 3])] {
        self.events
    }

    /// The source, the frame and the bytes of each event, in order.
    pub fn iter(&self) -> impl Iterator<Item = (Source, u64, [u8; 3])> + 'a {
        let sources = self.sources;
        self.events.iter().enumerate().map(move |(i, &(frame, bytes))| (sources.get(i).copied().unwrap_or(Source::KEYS), frame, bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_are_source_0_and_devices_are_numbered_from_1_to_the_last_input_that_the_rest_share() {
        assert_eq!(Source::KEYS.number(), 0);
        assert_eq!(Source::device(0).number(), 1);
        assert_eq!(Source::device(1).number(), 2);
        assert_eq!(Source::device(45).number(), 46);
        assert_eq!(Source::device(46), Source::LAST_INPUT);
        assert_eq!(Source::LAST_INPUT.number(), 47);
        assert_eq!(Source::device(47), Source::LAST_INPUT);
        assert_eq!(Source::device(1_000_000), Source::LAST_INPUT);
        assert!(!Source::LAST_INPUT.is_pattern(), "no device is a box, however many there are");
    }

    #[test]
    fn the_boxes_have_the_sixteen_numbers_after_the_inputs_and_are_in_no_default_set() {
        assert_eq!(MAX_BOXES, 16);
        assert_eq!((Source::pattern(0).number(), Source::pattern(1).number(), Source::pattern(15).number()), (48, 49, 63));
        assert_eq!(Source::pattern(16).number(), 63, "a number past the sixteenth is the last box's (a session refuses it)");
        assert!((0..MAX_BOXES).all(|nth| Source::pattern(nth).is_pattern()));
        assert!(!Source::KEYS.is_pattern() && !Source::device(3).is_pattern() && !Source::BROADCAST.is_pattern());
        for nth in 0..MAX_BOXES {
            assert!(!SourceSet::ALL.contains(Source::pattern(nth)), "a lane that is not told which sources it hears hears no box: box {nth}");
            assert!(SourceSet::NONE.with(Source::pattern(nth)).contains(Source::pattern(nth)), "and a set that is told one has it");
        }
        assert_eq!(SourceSet::NONE.with(Source::pattern(2)).with(Source::KEYS).iter().map(Source::number).collect::<Vec<_>>(), [0, 50]);
    }

    #[test]
    fn broadcast_is_no_source_of_the_96() {
        assert!(Source::BROADCAST.is_broadcast());
        assert!(!Source::KEYS.is_broadcast());
        assert!(!Source::LAST_INPUT.is_broadcast() && !Source::pattern(15).is_broadcast() && !Source::splitter(15, Output::Rest).is_broadcast());
        assert!(!SourceSet::ALL.contains(Source::BROADCAST), "no set has it: a lane hears it whatever its set is");
        assert_eq!(SourceSet::NONE.with(Source::BROADCAST), SourceSet::NONE);
        assert_eq!(SourceSet::ALL.without(Source::BROADCAST), SourceSet::ALL);
    }

    #[test]
    fn the_splitters_have_two_numbers_each_after_the_boxes_and_are_in_no_default_set() {
        assert_eq!((MAX_SPLITTERS, SPLITTER_SOURCES, MAX_SOURCES, HEARD_BY_SPLITTERS), (16, 32, 96, 64));
        assert_eq!((Source::splitter(0, Output::Pick).number(), Source::splitter(0, Output::Rest).number()), (64, 65));
        assert_eq!((Source::splitter(1, Output::Pick).number(), Source::splitter(15, Output::Rest).number()), (66, 95));
        assert_eq!(Source::splitter(16, Output::Pick), Source::splitter(15, Output::Pick), "a number past the sixteenth is the last splitter's (a session refuses it)");
        assert_eq!(Source::splitter(99, Output::Rest), Source::splitter(15, Output::Rest));
        for nth in 0..MAX_SPLITTERS {
            for output in [Output::Pick, Output::Rest] {
                let source = Source::splitter(nth, output);
                assert_eq!(source.splitter_output(), Some((nth, output)));
                assert!(!source.is_pattern() && !source.is_broadcast());
                assert!(!SourceSet::ALL.contains(source), "a lane that is not told which sources it hears hears no splitter: splitter {nth}");
                assert!(!SourceSet::first(HEARD_BY_SPLITTERS).contains(source), "and a splitter does not hear a splitter");
                assert!(SourceSet::NONE.with(source).contains(source), "and a set that is told one has it");
                assert_eq!(SourceSet::NONE.with(source).without(source), SourceSet::NONE);
            }
        }
        assert_eq!(Source::KEYS.splitter_output(), None);
        assert_eq!(Source::LAST_INPUT.splitter_output(), None);
        assert_eq!(Source::pattern(0).splitter_output(), None);
        assert_eq!(Source::pattern(15).splitter_output(), None, "the last box is not the first splitter's");
        assert_eq!(Source::BROADCAST.splitter_output(), None);
        assert_eq!(Source(MAX_SOURCES as u8 - 1).splitter_output(), Some((15, Output::Rest)), "the last number there is");
        assert_eq!(Source(MAX_SOURCES as u8).splitter_output(), None, "and the one after it is no source at all");
        assert_eq!(SourceSet::NONE.with(Source::splitter(2, Output::Rest)).with(Source::pattern(1)).with(Source::KEYS).iter().map(Source::number).collect::<Vec<_>>(), [0, 49, 69]);
        assert_eq!(SourceSet::NONE.with(Source::splitter(15, Output::Rest)).bits(), 1u128 << 95, "the bits go past the sixty-fourth");
    }

    #[test]
    fn the_first_sources_are_the_numbers_below_a_count() {
        assert_eq!(SourceSet::first(0), SourceSet::NONE);
        assert_eq!(SourceSet::first(INPUT_SOURCES), SourceSet::ALL);
        assert_eq!(SourceSet::first(3).bits(), 0b111);
        assert_eq!(SourceSet::first(HEARD_BY_SPLITTERS).iter().count(), 64);
        assert!(SourceSet::first(HEARD_BY_SPLITTERS).contains(Source::pattern(15)) && !SourceSet::first(HEARD_BY_SPLITTERS).contains(Source::splitter(0, Output::Pick)));
        assert_eq!(SourceSet::first(MAX_SOURCES).iter().count(), MAX_SOURCES);
        assert_eq!(SourceSet::first(128).bits(), u128::MAX);
        assert_eq!(SourceSet::first(1000).bits(), u128::MAX);
    }

    #[test]
    fn a_set_has_the_sources_that_were_put_in_it() {
        let set = SourceSet::NONE.with(Source::KEYS).with(Source::device(2)).with(Source::LAST_INPUT);
        assert_eq!(set.bits(), 1 | 1 << 3 | 1 << 47);
        assert!(set.contains(Source::KEYS) && set.contains(Source::device(2)) && set.contains(Source::LAST_INPUT));
        assert!(!set.contains(Source::device(0)) && !set.contains(Source::device(1)));
        assert_eq!(set.iter().map(Source::number).collect::<Vec<_>>(), [0, 3, 47]);
        assert_eq!(set.without(Source::device(2)).iter().map(Source::number).collect::<Vec<_>>(), [0, 47]);
        assert_eq!(SourceSet::from_bits(set.bits()), set);
    }

    #[test]
    fn every_input_and_no_source() {
        assert!(SourceSet::NONE.is_empty() && !SourceSet::ALL.is_empty());
        assert_eq!(SourceSet::ALL.iter().count(), INPUT_SOURCES);
        assert_eq!(SourceSet::NONE.iter().count(), 0);
        assert!(SourceSet::ALL.contains(Source::KEYS));
        assert!((0..INPUT_SOURCES - 1).all(|nth| SourceSet::ALL.contains(Source::device(nth))));
        assert!((0..INPUT_SOURCES - 1).all(|nth| !SourceSet::NONE.contains(Source::device(nth))));
    }

    #[test]
    fn sets_are_combined_by_what_they_have() {
        let a = SourceSet::NONE.with(Source::KEYS).with(Source::device(0));
        let b = SourceSet::NONE.with(Source::device(0)).with(Source::device(1));
        assert_eq!(a.and(b).bits(), 1 << 1);
        assert_eq!(a.or(b).bits(), 1 | 1 << 1 | 1 << 2);
        assert_eq!(a.minus(b).bits(), 1);
        assert_eq!(b.minus(a).bits(), 1 << 2);
        assert_eq!(a.minus(a), SourceSet::NONE);
    }

    #[test]
    fn a_message_with_no_source_is_from_the_keys_and_is_equal_to_its_frame_and_bytes() {
        let event = MidiEvent::from((5, [0x90, 60, 100]));
        assert_eq!(event, MidiEvent { source: Source::KEYS, frame: 5, bytes: [0x90, 60, 100] });
        assert_eq!(event, (5, [0x90, 60, 100]));
        assert_ne!(event, (6, [0x90, 60, 100]));
        assert_ne!(event, (5, [0x90, 61, 100]));
        let other = MidiEvent::from_source(Source::device(0), 5, [0x90, 60, 100]);
        assert_ne!(other, (5, [0x90, 60, 100]), "a message from a device is not one from the keys");
        assert_ne!(other, event);
    }

    #[test]
    fn events_with_no_sources_are_all_from_the_keys() {
        let list = [(0, [0x90, 60, 100]), (7, [0x80, 60, 0])];
        let events = Events::keys(&list);
        assert_eq!((events.len(), events.is_empty()), (2, false));
        assert_eq!(events.iter().collect::<Vec<_>>(), [(Source::KEYS, 0, [0x90, 60, 100]), (Source::KEYS, 7, [0x80, 60, 0])]);
        assert!(Events::keys(&[]).is_empty());
    }

    #[test]
    fn events_have_the_source_beside_each() {
        let list = [(0, [0x90, 60, 100]), (3, [0x90, 64, 90]), (7, [0x80, 60, 0])];
        let sources = [Source::device(1), Source::KEYS, Source::BROADCAST];
        let events = Events::new(&list, &sources);
        assert_eq!(
            events.iter().collect::<Vec<_>>(),
            [(Source::device(1), 0, [0x90, 60, 100]), (Source::KEYS, 3, [0x90, 64, 90]), (Source::BROADCAST, 7, [0x80, 60, 0])]
        );
    }

    #[test]
    fn a_source_is_named_for_a_log_line() {
        assert_eq!(Source::KEYS.to_string(), "the keys");
        assert_eq!(Source::device(2).to_string(), "device 3");
        assert_eq!(Source::pattern(0).to_string(), "pattern box 1");
        assert_eq!(Source::pattern(15).to_string(), "pattern box 16");
        assert_eq!(Source::splitter(0, Output::Pick).to_string(), "the pick of splitter 1");
        assert_eq!(Source::splitter(0, Output::Rest).to_string(), "the rest of splitter 1");
        assert_eq!(Source::splitter(15, Output::Rest).to_string(), "the rest of splitter 16");
        assert_eq!(Source::BROADCAST.to_string(), "every source");
    }
}
