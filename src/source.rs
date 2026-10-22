//! Where a MIDI message came from (P4a of `docs/patch-graph.md` §11.6): the number that an event carries from the program's inputs to the audio thread, the set of them that a lane
//! hears, and the events of a block as the engine is given them.
//!
//! A source is the computer's keys (`Source::KEYS`: the keys, the page's piano and keys, and every browser, which are one `NoteInput` path) or one MIDI device, which has a number of its
//! own for as long as the program runs (`midi::SourceTable`). A lane has the set of sources it hears (`SourceSet`, every source until it is told otherwise). A message that is sent as
//! `Source::BROADCAST` is no source's and is heard by every lane whatever its set is: the panic.

use std::fmt;

/// How many sources there can be: a set of them is a `u64`.
pub const MAX_SOURCES: usize = 64;

/// A source of MIDI messages: the computer's keys, or a device. A number below `MAX_SOURCES`, or `BROADCAST`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Source(u8);

impl Source {
    /// The computer's keys, the page's piano and keys and every browser's: the source of what `NoteInput::new` sends, and of every event that is not said to be another's.
    pub const KEYS: Source = Source(0);
    /// The last source there is, which the devices beyond the 63rd share (`Source::device`).
    pub const LAST: Source = Source(MAX_SOURCES as u8 - 1);
    /// No source's: what is heard by every lane, whatever the set of sources it hears is. A panic is sent as this, so that it reaches a lane that does not hear the keys.
    pub const BROADCAST: Source = Source(u8::MAX);

    /// The source of the device that was seen `nth`, counting from 0 (the keys are another): the 63rd and the ones after it are all `Source::LAST`.
    pub const fn device(nth: usize) -> Source {
        if nth + 1 >= MAX_SOURCES { Source::LAST } else { Source(nth as u8 + 1) }
    }

    /// Its number, 0 for the keys: below `MAX_SOURCES`, or 255 for `BROADCAST`.
    #[cfg_attr(not(test), expect(dead_code, reason = "the lanes hear by the number of a source: the next commit of P4a"))]
    pub const fn number(self) -> u8 {
        self.0
    }

    /// Whether this is `BROADCAST`.
    #[cfg_attr(not(test), expect(dead_code, reason = "the lanes hear a broadcast whatever their set is: the next commit of P4a"))]
    pub const fn is_broadcast(self) -> bool {
        self.0 == u8::MAX
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            0 => write!(f, "the keys"),
            u8::MAX => write!(f, "every source"),
            n => write!(f, "device {n}"),
        }
    }
}

/// A set of sources, one bit for each: what a lane hears.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(not(test), expect(dead_code, reason = "a lane has the set of sources it hears: the next commit of P4a"))]
pub struct SourceSet(u64);

#[cfg_attr(not(test), expect(dead_code, reason = "a lane has the set of sources it hears: the next commit of P4a"))]
impl SourceSet {
    /// Every source, those that are not there yet too: what a lane hears until it is told otherwise.
    pub const ALL: SourceSet = SourceSet(u64::MAX);
    /// No source.
    pub const NONE: SourceSet = SourceSet(0);

    /// The set that has the sources whose bits `bits` has (bit `n` is the source numbered `n`).
    pub const fn from_bits(bits: u64) -> SourceSet {
        SourceSet(bits)
    }

    /// The bits of the set.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Whether `source` is in the set. `BROADCAST` is in none: it is heard whatever the set is, and no set is made of it.
    pub const fn contains(self, source: Source) -> bool {
        source.0 < MAX_SOURCES as u8 && self.0 & (1u64 << source.0) != 0
    }

    /// The set and `source`. `BROADCAST` is no source, and the set is as it was.
    pub const fn with(self, source: Source) -> SourceSet {
        if source.0 < MAX_SOURCES as u8 { SourceSet(self.0 | 1u64 << source.0) } else { self }
    }

    /// The set without `source`.
    pub const fn without(self, source: Source) -> SourceSet {
        if source.0 < MAX_SOURCES as u8 { SourceSet(self.0 & !(1u64 << source.0)) } else { self }
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
        (0..MAX_SOURCES as u8).filter(move |&n| self.0 & (1u64 << n) != 0).map(Source)
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
    fn the_keys_are_source_0_and_devices_are_numbered_from_1_to_the_last_that_the_rest_share() {
        assert_eq!(Source::KEYS.number(), 0);
        assert_eq!(Source::device(0).number(), 1);
        assert_eq!(Source::device(1).number(), 2);
        assert_eq!(Source::device(61).number(), 62);
        assert_eq!(Source::device(62), Source::LAST);
        assert_eq!(Source::LAST.number(), 63);
        assert_eq!(Source::device(63), Source::LAST);
        assert_eq!(Source::device(1_000_000), Source::LAST);
    }

    #[test]
    fn broadcast_is_no_source_of_the_64() {
        assert!(Source::BROADCAST.is_broadcast());
        assert!(!Source::KEYS.is_broadcast());
        assert!(!Source::LAST.is_broadcast());
        assert!(!SourceSet::ALL.contains(Source::BROADCAST), "no set has it: a lane hears it whatever its set is");
        assert_eq!(SourceSet::NONE.with(Source::BROADCAST), SourceSet::NONE);
        assert_eq!(SourceSet::ALL.without(Source::BROADCAST), SourceSet::ALL);
    }

    #[test]
    fn a_set_has_the_sources_that_were_put_in_it() {
        let set = SourceSet::NONE.with(Source::KEYS).with(Source::device(2)).with(Source::LAST);
        assert_eq!(set.bits(), 1 | 1 << 3 | 1 << 63);
        assert!(set.contains(Source::KEYS) && set.contains(Source::device(2)) && set.contains(Source::LAST));
        assert!(!set.contains(Source::device(0)) && !set.contains(Source::device(1)));
        assert_eq!(set.iter().map(Source::number).collect::<Vec<_>>(), [0, 3, 63]);
        assert_eq!(set.without(Source::device(2)).iter().map(Source::number).collect::<Vec<_>>(), [0, 63]);
        assert_eq!(SourceSet::from_bits(set.bits()), set);
    }

    #[test]
    fn every_source_and_no_source() {
        assert!(SourceSet::NONE.is_empty() && !SourceSet::ALL.is_empty());
        assert_eq!(SourceSet::ALL.iter().count(), MAX_SOURCES);
        assert_eq!(SourceSet::NONE.iter().count(), 0);
        assert!(SourceSet::ALL.contains(Source::KEYS));
        assert!((0..MAX_SOURCES - 1).all(|nth| SourceSet::ALL.contains(Source::device(nth))));
        assert!((0..MAX_SOURCES - 1).all(|nth| !SourceSet::NONE.contains(Source::device(nth))));
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
        assert_eq!(Source::BROADCAST.to_string(), "every source");
    }
}
