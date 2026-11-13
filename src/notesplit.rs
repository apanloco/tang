//! Note splitters (P4i of `docs/patch-graph.md` §11.6): a node that takes notes in and sends them out on two outputs, the **pick** (the lowest note, or the highest) and the **rest** (every other note), so that a chord can be played
//! by two instruments: its bass note by one and its other notes by the other.
//!
//! This is the logic of one splitter as plain code: which notes of a block go to which output, with their frames, and what a splitter holds so that nothing hangs. The engine (`plugin::chain`) owns the splitters, gives each
//! the block's events and merges what it makes into them with the source of each output; the controller keeps the model of the same splitter.
//!
//! **A note is judged when it is pressed.** A note-on is the pick if it is lower than every key that is held (the highest: higher than every key held) and the rest if it is not; its note-off, and whatever else is said of
//! that note, goes to the output that has it, so that every key is on exactly one output at a time and no note can hang.
//!
//! **A key that takes the pick takes it from the key that had it** (step 129: the first rule, that a note stays where it went, left the higher key on the pick when a lower key was pressed after it: a chord that is held and a
//! bass note that is played after it gave the bass synth both). The pick has one key at the most: when a key is the pick, the key that had it **lets go of it**, at the same frame and after the new key has begun (so that a synth that slides
//! from one note to the next slides): it ends on the pick and begins on the rest, on its channel and as hard as it was last struck, and is a key of the rest from then on. **Nothing else moves**: the pick that is let go of
//! leaves the rest where it is, for the other rule, always "the lowest key that is held" as a mono synth has it, would make the next note of a chord jump into the bass when the bass key is let go of a few ms before the others.
//!
//! **The notes of one block are judged together**, with no latency: a note-on is compared with the keys that are held *and* with the note-ons that come later in the same block (a tie keeps the earlier), so that the G, the E and
//! the C of a chord that arrive in one block, in any order, put the C on the pick and the G and the E on the rest, each at its own frame. A chord that straddles two blocks gives the pick its first note for as long as it takes
//! the next block to come: a key that is lower takes the pick from it, and it goes to the rest.
//!
//! **What is not a note goes to both outputs** (controllers, the pedal, the wheel, channel pressure, program change, system messages); a poly aftertouch goes to the output that has its note. The keys of a source are counted
//! apart from another source's, and a source that stops being heard has what it holds let go of on both outputs.
//!
//! **No allocation once it runs.** Everything a block needs is made when the splitter is (`Splitter::new`): the audio thread does not ask for memory in `process`.

use crate::source::{Events, HEARD_BY_SPLITTERS, Output, Source, SourceSet};

/// How many events a splitter makes for one block at the most (the last `RESERVED_FOR_ENDS` are for the ends of notes, pedals and wheels, so that a block that is too full has room for them), and how many events of a block it
/// looks over for the notes that come later: room is made once, so that the audio thread asks for none. (A block that makes more than that is not a block that a MIDI input can make: a box can, at the most.)
pub const MAX_EVENTS_PER_BLOCK: usize = 4096;
const RESERVED_FOR_ENDS: usize = 256;
const MAX_LOOKED_OVER: usize = 4096;

/// The lowest and the highest pitch of no note at all: above every pitch and below every pitch.
const NO_LOWEST: i16 = 128;
const NO_HIGHEST: i16 = -1;

/// Which note a splitter picks out of the notes that are held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Pick {
    /// The lowest: the bass of a chord.
    #[default]
    Lowest,
    /// The highest: the top of a chord, its melody.
    Highest,
}

impl Pick {
    /// The word that a session file has for it (`pick = "highest"`).
    pub fn word(self) -> &'static str {
        match self {
            Pick::Lowest => "lowest",
            Pick::Highest => "highest",
        }
    }

    /// The pick that a word of a session file names, if it names one.
    pub fn parse(word: &str) -> Option<Pick> {
        match word {
            "lowest" => Some(Pick::Lowest),
            "highest" => Some(Pick::Highest),
            _ => None,
        }
    }
}

/// What a splitter is set to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub pick: Pick,
    /// The sources it hears: the inputs and the pattern boxes, never a splitter.
    pub inputs: SourceSet,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { pick: Pick::Lowest, inputs: SourceSet::ALL }
    }
}

/// An event that a splitter made for a block: where in the block it is, its bytes and the output that it is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Made {
    pub frame: u64,
    pub bytes: [u8; 3],
    pub output: Output,
}

/// What one source has sounding through the splitter, as far as the splitter has passed it on: the notes that went to each output (a bit for each, a word for each channel), the channels whose sustain pedal is down and the
/// channels whose pitch wheel is away from its centre. It is what the splitter lets go of when the source is no longer heard, so that no note hangs, a pedal does not hold what nobody plays and a wheel does not leave the pitch bent.
#[derive(Clone, Copy, Default)]
struct Held {
    pick: [u128; 16],
    rest: [u128; 16],
    pedal: u16,
    bent: u16,
}

/// A note splitter.
pub struct Splitter {
    sources: [Source; 2],
    settings: Settings,
    /// What each source that can be heard has sounding (the inputs, then the boxes: `HEARD_BY_SPLITTERS`).
    held: Box<[Held]>,
    /// How many keys hold each pitch (a key is a source, a channel and a pitch), and the pitches that have at least one: so that the lowest and the highest key held are found at once.
    counts: [u16; 128],
    pitches: u128,
    /// How hard the keys that the pick has were last struck, by channel and pitch: what the rest is given when a key lets go of the pick. (The pick has one key at the most, so that the velocity of a pitch on a channel is its key's.)
    pick_velocity: [[u8; 128]; 16],
    /// The sources that are to be let go of at the start of the next block: no longer heard, or gone.
    letting_go: SourceSet,
    /// What it made for the block that was run last.
    made: Vec<Made>,
    /// For each event of the block: the lowest and the highest pitch among the note-ons that come after it.
    lowest_after: Vec<i16>,
    highest_after: Vec<i16>,
}

impl Splitter {
    /// The splitter that has the number `nth` (0 to 15): its outputs are the sources `Source::splitter(nth, …)`. Made where memory may be asked for.
    pub fn new(nth: usize) -> Splitter {
        Splitter {
            sources: [Source::splitter(nth, Output::Pick), Source::splitter(nth, Output::Rest)],
            settings: Settings::default(),
            held: vec![Held::default(); HEARD_BY_SPLITTERS].into_boxed_slice(),
            counts: [0; 128],
            pitches: 0,
            pick_velocity: [[0; 128]; 16],
            letting_go: SourceSet::NONE,
            made: Vec::with_capacity(MAX_EVENTS_PER_BLOCK),
            lowest_after: Vec::with_capacity(MAX_LOOKED_OVER),
            highest_after: Vec::with_capacity(MAX_LOOKED_OVER),
        }
    }

    /// The source of an output.
    pub fn source(&self, output: Output) -> Source {
        self.sources[output as usize]
    }

    /// What it is set to.
    #[cfg(test)]
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// What it made for the block that was run last, in the order of the events that it heard.
    pub fn events(&self) -> &[Made] {
        &self.made
    }

    /// Set what it picks and what it hears. A source that was heard and is not any more is let go of at the start of the next block (what it holds goes, on both outputs); one that is heard again before then keeps what it
    /// holds. A source that a splitter cannot hear (another splitter's, or none) is left out. A note that is held stays where it is, whatever it picks now.
    pub fn set_settings(&mut self, settings: Settings) {
        let inputs = settings.inputs.and(SourceSet::first(HEARD_BY_SPLITTERS));
        let dropped = self.settings.inputs.minus(inputs);
        let added = inputs.minus(self.settings.inputs);
        self.letting_go = self.letting_go.minus(added).or(dropped);
        self.settings = Settings { inputs, ..settings };
    }

    /// Let go of what `source` has sounding through it, at the start of the next block: it has gone (a device that was unplugged, whose keys would be held for ever).
    pub fn release_input(&mut self, source: Source) {
        if usize::from(source.number()) < HEARD_BY_SPLITTERS {
            self.letting_go = self.letting_go.with(source);
        }
    }

    /// Run the splitter for a block: `input` is the block's events, in the order of their frames. What the splitter makes is in `events`.
    pub fn process(&mut self, input: Events<'_>) {
        self.made.clear();
        self.let_go();
        self.look_over(input);
        for (index, (source, frame, bytes)) in input.iter().enumerate() {
            if source.is_broadcast() {
                self.heard_a_panic(bytes);
            } else if self.settings.inputs.contains(source) {
                self.hear(index, source, frame, bytes);
            }
        }
    }

    /// For each event of the block, the lowest and the highest pitch among the note-ons that this splitter hears *after* it: what a note is compared with besides the keys that are held.
    fn look_over(&mut self, input: Events<'_>) {
        self.lowest_after.clear();
        self.highest_after.clear();
        for (source, _, bytes) in input.iter().take(MAX_LOOKED_OVER) {
            let begins = self.settings.inputs.contains(source) && bytes[0] & 0xF0 == 0x90 && bytes[2] > 0;
            // (Its own pitch goes in `highest_after` for now: the lists are made from the end, and each place is read before it is written.)
            self.highest_after.push(if begins { i16::from(bytes[1] & 0x7F) } else { NO_HIGHEST });
        }
        self.lowest_after.resize(self.highest_after.len(), NO_LOWEST);
        let (mut lowest, mut highest) = (NO_LOWEST, NO_HIGHEST);
        for at in (0..self.highest_after.len()).rev() {
            let own = self.highest_after[at];
            self.lowest_after[at] = lowest;
            self.highest_after[at] = highest;
            if own >= 0 {
                lowest = lowest.min(own);
                highest = highest.max(own);
            }
        }
    }

    /// What an event from a source that is heard does.
    fn hear(&mut self, index: usize, source: Source, frame: u64, bytes: [u8; 3]) {
        let from = usize::from(source.number());
        let channel = usize::from(bytes[0] & 0x0F);
        match bytes[0] & 0xF0 {
            0x90 if bytes[2] > 0 => self.note_on(index, from, frame, bytes),
            0x80 | 0x90 => self.note_off(from, frame, bytes),
            0xA0 => {
                // A poly aftertouch is of a note: it goes where the note went, and a note that is not held has none to go to.
                let bit = 1u128 << (bytes[1] & 0x7F);
                if self.held[from].pick[channel] & bit != 0 {
                    self.make(frame, bytes, Output::Pick);
                } else if self.held[from].rest[channel] & bit != 0 {
                    self.make(frame, bytes, Output::Rest);
                }
            }
            0xB0 => {
                match bytes[1] {
                    64 if bytes[2] >= 64 => self.held[from].pedal |= 1 << channel,
                    64 => self.held[from].pedal &= !(1 << channel),
                    120 | 123 => self.forget_channel(from, channel),
                    _ => {}
                }
                self.make_for_both(frame, bytes);
            }
            0xE0 => {
                if is_centre(bytes) {
                    self.held[from].bent &= !(1 << channel);
                } else {
                    self.held[from].bent |= 1 << channel;
                }
                self.make_for_both(frame, bytes);
            }
            _ => self.make_for_both(frame, bytes),
        }
    }

    /// A key goes down: it goes to the output that it is judged for, and where it goes is written down. A key that is struck again while it is down goes where it went. A key that is the pick takes it from the key that had it
    /// (`pick_lets_go`).
    fn note_on(&mut self, index: usize, from: usize, frame: u64, bytes: [u8; 3]) {
        let (channel, note) = (usize::from(bytes[0] & 0x0F), bytes[1] & 0x7F);
        let bit = 1u128 << note;
        if self.held[from].pick[channel] & bit != 0 {
            self.pick_velocity[channel][usize::from(note)] = bytes[2];
            self.make(frame, bytes, Output::Pick);
        } else if self.held[from].rest[channel] & bit != 0 {
            self.make(frame, bytes, Output::Rest);
        } else if self.made.len() < MAX_EVENTS_PER_BLOCK - RESERVED_FOR_ENDS {
            let output = self.judge(index, note);
            self.count_in(note);
            self.made.push(Made { frame, bytes, output });
            match output {
                Output::Pick => {
                    // The key that had the pick lets go of it after this key has begun, and before this key is written down as the pick, so that it is not let go of itself.
                    self.pick_lets_go(frame);
                    self.held[from].pick[channel] |= bit;
                    self.pick_velocity[channel][usize::from(note)] = bytes[2];
                }
                Output::Rest => self.held[from].rest[channel] |= bit,
            }
        }
    }

    /// The keys that have the pick let go of it, at `frame`, because a key takes it: each ends on the pick, and begins on the rest as it began (its channel, and how hard it was last struck) and is a key of the rest from now on: its
    /// note-off, and its poly aftertouch, go there. (The pick has one key at the most, so that this is one key or none; it is a loop, so that no key can be left on the pick.) A key that has no room to begin on the rest (a block
    /// that is full) is held nowhere, and its note-off is dropped, as the note-off of a note that was not begun is.
    fn pick_lets_go(&mut self, frame: u64) {
        for from in 0..HEARD_BY_SPLITTERS {
            for channel in 0..16usize {
                let mut notes = std::mem::take(&mut self.held[from].pick[channel]);
                while notes != 0 {
                    let note = notes.trailing_zeros() as u8;
                    notes &= notes - 1;
                    self.make_an_end(frame, [0x80 | channel as u8, note, 0], Output::Pick);
                    if self.made.len() < MAX_EVENTS_PER_BLOCK - RESERVED_FOR_ENDS {
                        self.held[from].rest[channel] |= 1u128 << note;
                        let velocity = self.pick_velocity[channel][usize::from(note)];
                        self.made.push(Made { frame, bytes: [0x90 | channel as u8, note, velocity], output: Output::Rest });
                    } else {
                        self.count_out(note);
                    }
                }
            }
        }
    }

    /// A key comes up: its note-off goes where its note-on went. One that was not begun here (the source was not heard then) has no output to go to, and is dropped.
    fn note_off(&mut self, from: usize, frame: u64, bytes: [u8; 3]) {
        let (channel, note) = (usize::from(bytes[0] & 0x0F), bytes[1] & 0x7F);
        let bit = 1u128 << note;
        let output = if self.held[from].pick[channel] & bit != 0 {
            self.held[from].pick[channel] &= !bit;
            Output::Pick
        } else if self.held[from].rest[channel] & bit != 0 {
            self.held[from].rest[channel] &= !bit;
            Output::Rest
        } else {
            return;
        };
        self.count_out(note);
        self.make_an_end(frame, bytes, output);
    }

    /// Which output a note that is pressed goes to: the pick if it is lower than every key held and than the notes that come later in the block (the highest: higher), the rest if it is not.
    fn judge(&self, index: usize, note: u8) -> Output {
        let pitch = i16::from(note);
        let alone = match self.settings.pick {
            Pick::Lowest => pitch < self.lowest_held() && pitch <= self.lowest_after.get(index).copied().unwrap_or(NO_LOWEST),
            Pick::Highest => pitch > self.highest_held() && pitch >= self.highest_after.get(index).copied().unwrap_or(NO_HIGHEST),
        };
        if alone { Output::Pick } else { Output::Rest }
    }

    /// The lowest pitch that a key holds, 128 if none does.
    fn lowest_held(&self) -> i16 {
        self.pitches.trailing_zeros() as i16
    }

    /// The highest pitch that a key holds, -1 if none does.
    fn highest_held(&self) -> i16 {
        127 - self.pitches.leading_zeros() as i16
    }

    fn count_in(&mut self, note: u8) {
        self.counts[usize::from(note)] += 1;
        self.pitches |= 1u128 << note;
    }

    fn count_out(&mut self, note: u8) {
        let count = &mut self.counts[usize::from(note)];
        *count = count.saturating_sub(1);
        if *count == 0 {
            self.pitches &= !(1u128 << note);
        }
    }

    /// The notes of a channel of a source are over (all notes off, all sound off): the splitter forgets them, and the controller that said so goes to both outputs, where the lanes and the plugins let go of them.
    fn forget_channel(&mut self, from: usize, channel: usize) {
        for notes in [std::mem::take(&mut self.held[from].pick[channel]), std::mem::take(&mut self.held[from].rest[channel])] {
            let mut left = notes;
            while left != 0 {
                self.count_out(left.trailing_zeros() as u8);
                left &= left - 1;
            }
        }
    }

    /// The note `note` of a channel of a source is over, wherever it went (a panic ended it).
    fn forget_note(&mut self, from: usize, channel: usize, note: u8) {
        let bit = 1u128 << note;
        let held = &mut self.held[from];
        if (held.pick[channel] | held.rest[channel]) & bit != 0 {
            held.pick[channel] &= !bit;
            held.rest[channel] &= !bit;
            self.count_out(note);
        }
    }

    /// A panic (`Source::BROADCAST`: the notes that are held end, the pedal goes up, all notes off and all sound off) is heard by every lane and plugin as it is, and is not passed on: it ends what the splitter holds, of every source.
    fn heard_a_panic(&mut self, bytes: [u8; 3]) {
        let channel = usize::from(bytes[0] & 0x0F);
        for from in 0..HEARD_BY_SPLITTERS {
            match bytes[0] & 0xF0 {
                0x80 => self.forget_note(from, channel, bytes[1] & 0x7F),
                0x90 if bytes[2] == 0 => self.forget_note(from, channel, bytes[1] & 0x7F),
                0xB0 => match bytes[1] {
                    120 | 123 => self.forget_channel(from, channel),
                    64 if bytes[2] < 64 => self.held[from].pedal &= !(1 << channel),
                    _ => {}
                },
                0xE0 if is_centre(bytes) => self.held[from].bent &= !(1 << channel),
                _ => {}
            }
        }
    }

    /// Let go of what the sources that are no longer heard, or are gone, hold: for each, the note-offs of its notes (to the output that each went to), then the pedal going up and the wheel going back to its centre, on both
    /// outputs, all at the start of the block.
    fn let_go(&mut self) {
        if self.letting_go.is_empty() {
            return;
        }
        for source in self.letting_go.iter() {
            let from = usize::from(source.number());
            for channel in 0..16usize {
                for output in [Output::Pick, Output::Rest] {
                    let notes = match output {
                        Output::Pick => std::mem::take(&mut self.held[from].pick[channel]),
                        Output::Rest => std::mem::take(&mut self.held[from].rest[channel]),
                    };
                    let mut left = notes;
                    while left != 0 {
                        let note = left.trailing_zeros() as u8;
                        left &= left - 1;
                        self.count_out(note);
                        self.make_an_end(0, [0x80 | channel as u8, note, 0], output);
                    }
                }
            }
            for channel in 0..16usize {
                let bit = 1u16 << channel;
                if self.held[from].pedal & bit != 0 {
                    self.held[from].pedal &= !bit;
                    self.make_an_end_for_both(0, [0xB0 | channel as u8, 64, 0]);
                }
                if self.held[from].bent & bit != 0 {
                    self.held[from].bent &= !bit;
                    self.make_an_end_for_both(0, [0xE0 | channel as u8, 0x00, 0x40]);
                }
            }
        }
        self.letting_go = SourceSet::NONE;
    }

    /// An event that is not an end, for an output: not made when the block's room, less what is kept for the ends of notes, is used up.
    fn make(&mut self, frame: u64, bytes: [u8; 3], output: Output) {
        if self.made.len() < MAX_EVENTS_PER_BLOCK - RESERVED_FOR_ENDS {
            self.made.push(Made { frame, bytes, output });
        }
    }

    fn make_for_both(&mut self, frame: u64, bytes: [u8; 3]) {
        self.make(frame, bytes, Output::Pick);
        self.make(frame, bytes, Output::Rest);
    }

    /// The end of a note, a pedal that comes up or a wheel that goes back: it may use all the room.
    fn make_an_end(&mut self, frame: u64, bytes: [u8; 3], output: Output) {
        if self.made.len() < MAX_EVENTS_PER_BLOCK {
            self.made.push(Made { frame, bytes, output });
        }
    }

    fn make_an_end_for_both(&mut self, frame: u64, bytes: [u8; 3]) {
        self.make_an_end(frame, bytes, Output::Pick);
        self.make_an_end(frame, bytes, Output::Rest);
    }

    /// Which output has the note of a source, a channel and a pitch, if it is held.
    #[cfg(test)]
    fn holds(&self, source: Source, channel: usize, note: u8) -> Option<Output> {
        let bit = 1u128 << note;
        let held = &self.held[usize::from(source.number())];
        if held.pick[channel] & bit != 0 {
            Some(Output::Pick)
        } else if held.rest[channel] & bit != 0 {
            Some(Output::Rest)
        } else {
            None
        }
    }

    /// How many keys are held, of every source.
    #[cfg(test)]
    fn keys_held(&self) -> usize {
        self.held.iter().map(|held| held.pick.iter().chain(held.rest.iter()).map(|notes| notes.count_ones() as usize).sum::<usize>()).sum()
    }
}

/// Whether a pitch bend message is the wheel at its centre.
fn is_centre(bytes: [u8; 3]) -> bool {
    u16::from(bytes[1]) | u16::from(bytes[2]) << 7 == 8192
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc_guard::allocations_in;

    const KEYS: Source = Source::KEYS;
    const DEVICE: Source = Source::device(0);
    const OTHER_DEVICE: Source = Source::device(1);

    fn on(note: u8) -> [u8; 3] {
        [0x90, note, 100]
    }

    fn off(note: u8) -> [u8; 3] {
        [0x80, note, 0]
    }

    /// One block of events (the source, the frame and the bytes of each, in the order of their frames) run through the splitter: what it made, as the output, the frame and the bytes.
    fn run(splitter: &mut Splitter, block: &[(Source, u64, [u8; 3])]) -> Vec<(Output, u64, [u8; 3])> {
        let list: Vec<(u64, [u8; 3])> = block.iter().map(|&(_, frame, bytes)| (frame, bytes)).collect();
        let sources: Vec<Source> = block.iter().map(|&(source, _, _)| source).collect();
        splitter.process(Events::new(&list, &sources));
        splitter.events().iter().map(|made| (made.output, made.frame, made.bytes)).collect()
    }

    fn keys(events: &[(u64, [u8; 3])]) -> Vec<(Source, u64, [u8; 3])> {
        events.iter().map(|&(frame, bytes)| (KEYS, frame, bytes)).collect()
    }

    use Output::{Pick, Rest};
    // (`Pick` is an output here, as the events say; the enum of what is picked is `Pick_`.)
    use super::Pick as Pick_;

    fn only(sources: &[Source]) -> SourceSet {
        sources.iter().fold(SourceSet::NONE, |set, &source| set.with(source))
    }

    #[test]
    fn a_note_that_is_pressed_alone_is_the_pick_and_ends_where_it_began() {
        let mut splitter = Splitter::new(0);
        assert_eq!(run(&mut splitter, &keys(&[(3, on(60))])), [(Pick, 3, on(60))]);
        assert_eq!(splitter.holds(KEYS, 0, 60), Some(Pick));
        assert_eq!(run(&mut splitter, &keys(&[(9, off(60))])), [(Pick, 9, off(60))]);
        assert_eq!((splitter.holds(KEYS, 0, 60), splitter.keys_held()), (None, 0));
    }

    #[test]
    fn the_outputs_are_the_sources_of_the_splitter_and_it_starts_as_it_is_made() {
        let splitter = Splitter::new(3);
        assert_eq!((splitter.source(Pick), splitter.source(Rest)), (Source::splitter(3, Pick), Source::splitter(3, Rest)));
        assert_eq!((splitter.source(Pick).number(), splitter.source(Rest).number()), (70, 71));
        assert_eq!(splitter.settings, Settings { pick: Pick_::Lowest, inputs: SourceSet::ALL });
        assert!(splitter.events().is_empty());
    }

    #[test]
    fn a_chord_of_one_block_puts_its_lowest_note_on_the_pick_whatever_the_order_the_notes_came_in() {
        // C3, E3 and G3, at frames 10, 12 and 14, in every order.
        let notes = [48u8, 52, 55];
        for order in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
            let mut splitter = Splitter::new(0);
            let block: Vec<(Source, u64, [u8; 3])> = order.iter().enumerate().map(|(at, &which)| (KEYS, 10 + 2 * at as u64, on(notes[which]))).collect();
            let made = run(&mut splitter, &block);
            let expected: Vec<(Output, u64, [u8; 3])> = block.iter().map(|&(_, frame, bytes)| (if bytes[1] == 48 { Pick } else { Rest }, frame, bytes)).collect();
            assert_eq!(made, expected, "in the order {order:?}: the C is the pick at its own frame and the others are the rest at theirs");
        }
    }

    #[test]
    fn the_highest_note_is_the_pick_when_it_is_what_is_picked() {
        let notes = [48u8, 52, 55];
        for order in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
            let mut splitter = Splitter::new(0);
            splitter.set_settings(Settings { pick: Pick_::Highest, ..Settings::default() });
            let block: Vec<(Source, u64, [u8; 3])> = order.iter().enumerate().map(|(at, &which)| (KEYS, 10 + 2 * at as u64, on(notes[which]))).collect();
            let made = run(&mut splitter, &block);
            let expected: Vec<(Output, u64, [u8; 3])> = block.iter().map(|&(_, frame, bytes)| (if bytes[1] == 55 { Pick } else { Rest }, frame, bytes)).collect();
            assert_eq!(made, expected, "in the order {order:?}: the G is the pick");
        }
    }

    #[test]
    fn a_note_stays_where_it_went_until_it_ends() {
        let mut splitter = Splitter::new(0);
        assert_eq!(run(&mut splitter, &keys(&[(0, on(48))])), [(Pick, 0, on(48))]);
        assert_eq!(run(&mut splitter, &keys(&[(0, on(52))])), [(Rest, 0, on(52))], "higher than the key that is held");
        // The bass key goes up while the E is held: the E does not become the bass, and says nothing.
        assert_eq!(run(&mut splitter, &keys(&[(0, off(48))])), [(Pick, 0, off(48))]);
        assert_eq!((splitter.holds(KEYS, 0, 52), splitter.keys_held()), (Some(Rest), 1));
        // A key above the E is the rest (the E is the lowest that is held, and it is not lower); a key below everything is the pick.
        assert_eq!(run(&mut splitter, &keys(&[(0, on(53))])), [(Rest, 0, on(53))]);
        assert_eq!(run(&mut splitter, &keys(&[(0, on(40))])), [(Pick, 0, on(40))]);
        assert_eq!(run(&mut splitter, &keys(&[(0, off(52)), (1, off(53)), (2, off(40))])), [(Rest, 0, off(52)), (Rest, 1, off(53)), (Pick, 2, off(40))], "each ends where it began");
        assert_eq!(splitter.keys_held(), 0);
    }

    /// What each output has sounding (by pitch, channel 0), from what the splitter made: a note-on adds one, a note-off takes one away.
    fn sound(sounding: &mut [[i32; 128]; 2], made: &[(Output, u64, [u8; 3])]) {
        for &(output, _, bytes) in made {
            match (bytes[0] & 0xF0, bytes[2]) {
                (0x90, velocity) if velocity > 0 => sounding[output as usize][usize::from(bytes[1])] += 1,
                (0x80, _) | (0x90, _) => sounding[output as usize][usize::from(bytes[1])] -= 1,
                _ => {}
            }
        }
    }

    /// The pitches that an output has sounding, lowest first.
    fn sounding_on(sounding: &[[i32; 128]; 2], output: Output) -> Vec<u8> {
        (0..128u8).filter(|&note| sounding[output as usize][usize::from(note)] > 0).collect()
    }

    #[test]
    fn a_lower_key_that_is_pressed_later_takes_the_pick_and_the_key_that_had_it_lets_go_of_it_and_goes_to_the_rest() {
        let mut splitter = Splitter::new(0);
        // The G is on channel 3, struck at 77.
        run(&mut splitter, &keys(&[(0, [0x92, 55, 77])]));
        let made = run(&mut splitter, &keys(&[(4, on(48))]));
        assert_eq!(
            made,
            [(Pick, 4, on(48)), (Pick, 4, [0x82, 55, 0]), (Rest, 4, [0x92, 55, 77])],
            "the C is the pick; the G ends there after the C has begun (a synth that slides from one note to the next slides), and begins on the rest as it did: on its channel and as hard"
        );
        assert_eq!((splitter.holds(KEYS, 2, 55), splitter.holds(KEYS, 0, 48), splitter.keys_held()), (Some(Rest), Some(Pick), 2), "the pick has one key, and the G is a key of the rest from now on");
        assert_eq!(run(&mut splitter, &keys(&[(0, [0x82, 55, 0]), (1, off(48))])), [(Rest, 0, [0x82, 55, 0]), (Pick, 1, off(48))], "each ends where it is now");
        assert_eq!(splitter.keys_held(), 0);
    }

    #[test]
    fn a_chord_that_is_held_and_a_lower_note_pressed_after_it_leave_the_chord_to_the_rest_and_the_note_alone_on_the_pick() {
        let mut splitter = Splitter::new(0);
        let mut sounding = [[0i32; 128]; 2];
        // C5, E5 and G5 in one block: the C is the pick.
        let made = run(&mut splitter, &keys(&[(0, on(72)), (1, on(76)), (2, on(79))]));
        assert_eq!(made, [(Pick, 0, on(72)), (Rest, 1, on(76)), (Rest, 2, on(79))]);
        sound(&mut sounding, &made);
        // C3, pressed after, is the lowest: it is the pick, and the C5 lets go of the pick and goes to the rest with the E and the G.
        let made = run(&mut splitter, &keys(&[(10, on(48))]));
        assert_eq!(made, [(Pick, 10, on(48)), (Pick, 10, off(72)), (Rest, 10, on(72))]);
        sound(&mut sounding, &made);
        assert_eq!((sounding_on(&sounding, Pick), sounding_on(&sounding, Rest)), (vec![48], vec![72, 76, 79]), "the bass is the C3 alone, and the chord is the whole chord");
        // The C3 comes up: the chord is as it was and the pick has nothing (no key moves up into the bass), and the next low key is the pick again.
        let made = run(&mut splitter, &keys(&[(0, off(48))]));
        assert_eq!(made, [(Pick, 0, off(48))]);
        sound(&mut sounding, &made);
        assert_eq!((sounding_on(&sounding, Pick), sounding_on(&sounding, Rest)), (vec![], vec![72, 76, 79]));
        assert_eq!(run(&mut splitter, &keys(&[(0, on(50))])), [(Pick, 0, on(50))]);
    }

    #[test]
    fn the_highest_key_that_is_pressed_later_takes_the_pick_when_the_highest_is_picked_and_a_lower_key_is_the_rest() {
        let mut splitter = Splitter::new(0);
        splitter.set_settings(Settings { pick: Pick_::Highest, ..Settings::default() });
        run(&mut splitter, &keys(&[(0, on(55))]));
        assert_eq!(run(&mut splitter, &keys(&[(3, on(64))])), [(Pick, 3, on(64)), (Pick, 3, off(55)), (Rest, 3, on(55))], "the E is higher than the G that is held: it is the pick, and the G is the rest");
        assert_eq!(run(&mut splitter, &keys(&[(0, on(60))])), [(Rest, 0, on(60))], "a key that is not above the pick is the rest, and nothing moves");
        assert_eq!(splitter.holds(KEYS, 0, 64), Some(Pick));
    }

    #[test]
    fn a_key_that_is_not_lower_than_what_is_held_does_not_take_the_pick_and_nothing_moves() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &keys(&[(0, on(48))]));
        assert_eq!(run(&mut splitter, &keys(&[(0, on(52)), (1, on(48 + 12))])), [(Rest, 0, on(52)), (Rest, 1, on(60))]);
        // The same key, struck again, is not a lower key either.
        assert_eq!(run(&mut splitter, &keys(&[(0, on(48))])), [(Pick, 0, on(48))]);
        assert_eq!((splitter.holds(KEYS, 0, 48), splitter.keys_held()), (Some(Pick), 3));
    }

    #[test]
    fn a_chord_that_straddles_blocks_ends_with_its_lowest_note_on_the_pick_and_the_others_on_the_rest_in_whatever_order_it_came() {
        let notes = [72u8, 76, 79];
        for order in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
            let mut splitter = Splitter::new(0);
            let mut sounding = [[0i32; 128]; 2];
            // Each note in a block of its own: what is judged is the keys that are held, and a note that is first and is not the lowest is the pick for as long as it takes the others to come.
            for (at, &which) in order.iter().enumerate() {
                let made = run(&mut splitter, &keys(&[(at as u64 * 3, on(notes[which]))]));
                sound(&mut sounding, &made);
            }
            assert_eq!((sounding_on(&sounding, Pick), sounding_on(&sounding, Rest)), (vec![72], vec![76, 79]), "in the order {order:?}");
            // Everything is let go of where it is: nothing hangs on either output.
            let ends: Vec<(u64, [u8; 3])> = notes.iter().map(|&note| (0, off(note))).collect();
            let made = run(&mut splitter, &keys(&ends));
            sound(&mut sounding, &made);
            assert!(sounding.iter().flatten().all(|&n| n == 0), "in the order {order:?}");
            assert_eq!(splitter.keys_held(), 0);
        }
    }

    #[test]
    fn the_pick_that_lets_go_goes_to_the_rest_as_hard_as_it_was_struck_the_last_time() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &keys(&[(0, [0x90, 55, 60])]));
        assert_eq!(run(&mut splitter, &keys(&[(0, [0x90, 55, 90])])), [(Pick, 0, [0x90, 55, 90])], "struck again while it is down: it goes where it went");
        assert_eq!(run(&mut splitter, &keys(&[(0, on(48))])), [(Pick, 0, on(48)), (Pick, 0, off(55)), (Rest, 0, [0x90, 55, 90])]);
    }

    #[test]
    fn the_pick_of_another_source_and_channel_lets_go_as_the_key_it_was() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &[(DEVICE, 0, [0x93, 64, 33])]);
        let made = run(&mut splitter, &[(KEYS, 2, on(50))]);
        assert_eq!(made, [(Pick, 2, on(50)), (Pick, 2, [0x83, 64, 0]), (Rest, 2, [0x93, 64, 33])], "on the channel it was on, and as hard");
        assert_eq!((splitter.holds(DEVICE, 3, 64), splitter.holds(KEYS, 0, 50)), (Some(Rest), Some(Pick)), "the device's key is the rest's now, under its own source");
        // The device's key comes up, and the note-off goes to the rest, where the key is.
        assert_eq!(run(&mut splitter, &[(DEVICE, 0, [0x83, 64, 0])]), [(Rest, 0, [0x83, 64, 0])]);
        // A device that goes lets go of a key that moved, on the output it is on now.
        run(&mut splitter, &[(DEVICE, 0, [0x91, 40, 70])]);
        assert_eq!(splitter.holds(DEVICE, 1, 40), Some(Pick));
        run(&mut splitter, &[(KEYS, 1, [0x90, 30, 70])]);
        assert_eq!(splitter.holds(DEVICE, 1, 40), Some(Rest));
        splitter.release_input(DEVICE);
        assert_eq!(run(&mut splitter, &[]), [(Rest, 0, [0x81, 40, 0])]);
    }

    #[test]
    fn a_poly_aftertouch_goes_to_the_pick_before_its_key_lets_go_of_it_and_to_the_rest_after() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &keys(&[(0, on(55))]));
        assert_eq!(run(&mut splitter, &keys(&[(0, [0xA0, 55, 20])])), [(Pick, 0, [0xA0, 55, 20])]);
        run(&mut splitter, &keys(&[(0, on(48))]));
        assert_eq!(run(&mut splitter, &keys(&[(0, [0xA0, 55, 21]), (1, [0xA0, 48, 22])])), [(Rest, 0, [0xA0, 55, 21]), (Pick, 1, [0xA0, 48, 22])]);
    }

    #[test]
    fn a_pick_that_lets_go_has_its_end_made_whatever_the_room_and_begins_on_the_rest_only_where_a_note_has_room() {
        let room = MAX_EVENTS_PER_BLOCK - RESERVED_FOR_ENDS;
        // `before` events are made before the lower key: it begins (it has room while `before` is below `room`), the end of the key that had the pick is made after it (an end has room to the end of the block),
        // and that key begins on the rest if there is room for a note after both: `before + 2` below `room`.
        for before in [room - 3, room - 2, room - 1] {
            let mut splitter = Splitter::new(0);
            // The C4 is the pick and the C5 the rest, which the block's poly aftertouch is of (one event each).
            run(&mut splitter, &keys(&[(0, on(60)), (1, on(72))]));
            let mut block: Vec<(Source, u64, [u8; 3])> = (0..before).map(|at| (KEYS, 0, [0xA0, 72, (at % 100) as u8])).collect();
            block.push((KEYS, 1, on(48)));
            let made = run(&mut splitter, &block);
            let has_room = before + 2 < room;
            assert_eq!(made.len(), before + 2 + usize::from(has_room), "{before} events before the lower key");
            assert_eq!(&made[before..before + 2], [(Pick, 1, on(48)), (Pick, 1, off(60))], "the key that takes the pick, and the end of the key that had it: the end is made whatever the room ({before})");
            if has_room {
                assert_eq!(made[before + 2], (Rest, 1, on(60)), "the key that let go begins on the rest ({before})");
                assert_eq!((splitter.holds(KEYS, 0, 60), splitter.keys_held(), splitter.pitches), (Some(Rest), 3, 1u128 << 48 | 1u128 << 60 | 1u128 << 72), "{before}");
                assert_eq!(run(&mut splitter, &keys(&[(0, off(60))])), [(Rest, 0, off(60))], "and ends there ({before})");
            } else {
                assert_eq!((splitter.holds(KEYS, 0, 60), splitter.holds(KEYS, 0, 48), splitter.holds(KEYS, 0, 72), splitter.keys_held()), (None, Some(Pick), Some(Rest), 2), "the C4 is held nowhere ({before})");
                assert_eq!(splitter.pitches, 1u128 << 48 | 1u128 << 72, "and its pitch is not counted ({before})");
                assert!(run(&mut splitter, &keys(&[(0, off(60))])).is_empty(), "so its note-off is dropped, as the note-off of a note that was not begun is ({before})");
            }
        }
    }

    #[test]
    fn a_chord_that_straddles_two_blocks_gives_the_pick_its_first_note_for_as_long_as_it_takes_the_next_block_to_come() {
        let mut splitter = Splitter::new(0);
        assert_eq!(run(&mut splitter, &keys(&[(250, on(55))])), [(Pick, 250, on(55))], "the G is the lowest that anyone has pressed so far");
        assert_eq!(run(&mut splitter, &keys(&[(2, on(48))])), [(Pick, 2, on(48)), (Pick, 2, off(55)), (Rest, 2, on(55))], "and the C, in the next block, is lower: the G is the rest's from there on");
    }

    #[test]
    fn a_note_is_compared_with_the_notes_that_come_later_in_its_block_and_not_with_those_after_it() {
        let mut splitter = Splitter::new(0);
        // The D is the first of its block, and the C that comes in the block after it is nothing to it.
        assert_eq!(run(&mut splitter, &keys(&[(0, on(50))])), [(Pick, 0, on(50))]);
        let mut splitter = Splitter::new(0);
        // With the C in the same block, a note that is pressed before it is the rest.
        assert_eq!(run(&mut splitter, &keys(&[(0, on(50)), (5, on(48))])), [(Rest, 0, on(50)), (Pick, 5, on(48))]);
        let mut splitter = Splitter::new(0);
        // A note that is pressed and let go of before the next is looked at as it is pressed: both are notes of the block.
        assert_eq!(run(&mut splitter, &keys(&[(0, on(50)), (1, off(50)), (2, on(48))])), [(Rest, 0, on(50)), (Rest, 1, off(50)), (Pick, 2, on(48))]);
    }

    #[test]
    fn a_tie_goes_to_the_earlier_note_and_two_sources_that_hold_the_same_key_are_two_keys() {
        let mut splitter = Splitter::new(0);
        let made = run(&mut splitter, &[(KEYS, 0, on(60)), (DEVICE, 0, on(60))]);
        assert_eq!(made, [(Pick, 0, on(60)), (Rest, 0, on(60))], "the keys' C is the pick, and the device's, which is not lower, is the rest");
        assert_eq!((splitter.holds(KEYS, 0, 60), splitter.holds(DEVICE, 0, 60)), (Some(Pick), Some(Rest)));
        assert_eq!(splitter.keys_held(), 2);
        // Each ends where it began, and the other is held while one is.
        assert_eq!(run(&mut splitter, &[(KEYS, 0, off(60))]), [(Pick, 0, off(60))]);
        assert_eq!(splitter.holds(DEVICE, 0, 60), Some(Rest));
        // The C that is held (the device's) is what a new key is compared with.
        assert_eq!(run(&mut splitter, &[(KEYS, 0, on(60))]), [(Rest, 0, on(60))]);
        assert_eq!(run(&mut splitter, &[(DEVICE, 0, off(60))]), [(Rest, 0, off(60))]);
        assert_eq!(run(&mut splitter, &[(KEYS, 0, off(60))]), [(Rest, 0, off(60))]);
        assert_eq!(splitter.keys_held(), 0);
    }

    #[test]
    fn a_key_that_is_struck_again_while_it_is_down_goes_where_it_went_and_is_still_one_key() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &keys(&[(0, on(48)), (0, on(52))]));
        assert_eq!(run(&mut splitter, &keys(&[(0, on(52)), (1, on(48))])), [(Rest, 0, on(52)), (Pick, 1, on(48))]);
        assert_eq!(splitter.keys_held(), 2);
        assert_eq!(run(&mut splitter, &keys(&[(0, off(48)), (0, off(52))])), [(Pick, 0, off(48)), (Rest, 0, off(52))], "one note-off ends a key that was struck twice");
        assert_eq!(splitter.keys_held(), 0);
    }

    #[test]
    fn the_note_off_of_a_note_that_it_did_not_begin_is_dropped_and_a_note_on_with_no_velocity_is_a_note_off() {
        let mut splitter = Splitter::new(0);
        assert!(run(&mut splitter, &keys(&[(0, off(60))])).is_empty(), "the lanes were never given its note-on");
        run(&mut splitter, &keys(&[(0, on(60))]));
        assert_eq!(run(&mut splitter, &keys(&[(4, [0x90, 60, 0])])), [(Pick, 4, [0x90, 60, 0])], "a note-on with no velocity ends the note, and goes on as it was said");
        assert_eq!(splitter.keys_held(), 0);
        assert!(run(&mut splitter, &keys(&[(0, [0x90, 60, 0])])).is_empty());
    }

    #[test]
    fn the_notes_of_every_channel_are_judged_together_and_the_channel_goes_on_with_them() {
        let mut splitter = Splitter::new(0);
        let made = run(&mut splitter, &keys(&[(0, [0x92, 60, 90]), (1, [0x93, 55, 80]), (2, [0x91, 64, 70])]));
        assert_eq!(made, [(Rest, 0, [0x92, 60, 90]), (Pick, 1, [0x93, 55, 80]), (Rest, 2, [0x91, 64, 70])], "a note is the pick if it is the lowest, whatever channel it is on");
        assert_eq!((splitter.holds(KEYS, 3, 55), splitter.holds(KEYS, 2, 60), splitter.holds(KEYS, 0, 55)), (Some(Pick), Some(Rest), None), "and a note is its channel's");
        assert_eq!(run(&mut splitter, &keys(&[(0, [0x83, 55, 0]), (0, [0x82, 60, 0])])), [(Pick, 0, [0x83, 55, 0]), (Rest, 0, [0x82, 60, 0])]);
    }

    #[test]
    fn what_is_not_a_note_goes_to_both_outputs_at_its_frame() {
        let mut splitter = Splitter::new(0);
        let messages = [[0xB0, 1, 64], [0xE0, 0, 96], [0xD0, 50, 0], [0xC0, 7, 0], [0xF8, 0, 0]];
        for (at, message) in messages.into_iter().enumerate() {
            assert_eq!(run(&mut splitter, &keys(&[(at as u64 * 3, message)])), [(Pick, at as u64 * 3, message), (Rest, at as u64 * 3, message)], "{message:02x?}");
        }
    }

    #[test]
    fn a_poly_aftertouch_goes_to_the_output_that_has_its_note_and_a_note_that_is_not_held_has_none() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &keys(&[(0, on(48)), (0, on(60))]));
        let made = run(&mut splitter, &keys(&[(1, [0xA0, 60, 30]), (2, [0xA0, 48, 40]), (3, [0xA0, 70, 50])]));
        assert_eq!(made, [(Rest, 1, [0xA0, 60, 30]), (Pick, 2, [0xA0, 48, 40])], "to the output of each note, and nowhere for the note that is not held");
    }

    #[test]
    fn the_pedal_and_the_wheel_are_kept_for_the_source_that_moved_them() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &keys(&[(0, [0xB0, 64, 127]), (0, [0xE0, 0, 96]), (0, [0xB1, 64, 100])]));
        assert_eq!((splitter.held[0].pedal, splitter.held[0].bent), (0b11, 0b1));
        run(&mut splitter, &keys(&[(0, [0xB0, 64, 0]), (0, [0xE0, 0x00, 0x40])]));
        assert_eq!((splitter.held[0].pedal, splitter.held[0].bent), (0b10, 0), "the pedal of channel 1 is still down; a wheel at its centre is not away from it");
        run(&mut splitter, &[(DEVICE, 0, [0xB0, 64, 127])]);
        assert_eq!(splitter.held[1].pedal, 1, "and the device's is its own");
        assert_eq!(splitter.held[0].pedal, 0b10);
    }

    #[test]
    fn a_source_that_is_not_heard_is_not_heard_and_a_box_is_when_it_is_in_the_set() {
        let mut splitter = Splitter::new(0);
        splitter.set_settings(Settings { inputs: only(&[DEVICE, Source::pattern(2)]), ..Settings::default() });
        let made = run(&mut splitter, &[(KEYS, 0, on(60)), (DEVICE, 1, on(62)), (Source::pattern(2), 2, on(61)), (Source::pattern(3), 3, on(59)), (OTHER_DEVICE, 4, [0xB0, 1, 9])]);
        assert_eq!(made, [(Rest, 1, on(62)), (Pick, 2, on(61))], "the device's D and the box's C sharp, which is lower and later: the C sharp is the pick; the keys' C, the other box and the other device are not heard");
    }

    #[test]
    fn a_splitter_does_not_hear_a_splitter_and_what_it_hears_is_what_is_in_its_set() {
        let mut splitter = Splitter::new(0);
        let another = Source::splitter(1, Rest);
        splitter.set_settings(Settings { inputs: only(&[KEYS, another, Source::pattern(0)]), ..Settings::default() });
        assert_eq!(splitter.settings.inputs, only(&[KEYS, Source::pattern(0)]), "no splitter, so that no loop can be made");
        assert!(run(&mut splitter, &[(another, 0, on(60))]).is_empty());
    }

    #[test]
    fn a_source_that_is_no_longer_heard_has_what_it_holds_let_go_of_on_both_outputs_at_the_start_of_the_next_block() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &keys(&[(0, on(48)), (0, on(52)), (0, [0x91, 55, 80]), (0, [0xB0, 64, 127]), (0, [0xE1, 0, 100])]));
        assert_eq!(splitter.keys_held(), 3);
        splitter.set_settings(Settings { inputs: only(&[DEVICE]), ..Settings::default() });
        let made = run(&mut splitter, &[(DEVICE, 5, on(70))]);
        assert_eq!(
            made,
            [
                (Pick, 0, off(48)),
                (Rest, 0, off(52)),
                (Rest, 0, [0x81, 55, 0]),
                (Pick, 0, [0xB0, 64, 0]),
                (Rest, 0, [0xB0, 64, 0]),
                (Pick, 0, [0xE1, 0x00, 0x40]),
                (Rest, 0, [0xE1, 0x00, 0x40]),
                (Pick, 5, on(70)),
            ],
            "the notes to the outputs that had them, then the pedal and the wheel to both, and then the block"
        );
        assert_eq!((splitter.held[0].pedal, splitter.held[0].bent, splitter.keys_held()), (0, 0, 1), "and the keys have nothing of it");
        // The note that it began is judged against what is held now: only the device's D.
        assert_eq!(run(&mut splitter, &[(DEVICE, 0, on(72))]), [(Rest, 0, on(72))]);
    }

    #[test]
    fn a_source_that_is_heard_again_before_it_is_let_go_of_keeps_what_it_holds() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &keys(&[(0, on(48))]));
        splitter.set_settings(Settings { inputs: only(&[DEVICE]), ..Settings::default() });
        splitter.set_settings(Settings::default());
        assert!(run(&mut splitter, &[]).is_empty(), "nothing was let go of");
        assert_eq!(run(&mut splitter, &keys(&[(0, off(48))])), [(Pick, 0, off(48))]);
    }

    #[test]
    fn a_source_that_has_gone_has_what_it_holds_let_go_of_and_the_others_keep_theirs() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &[(KEYS, 0, on(48)), (DEVICE, 0, on(52)), (OTHER_DEVICE, 0, on(40))]);
        assert_eq!((splitter.holds(KEYS, 0, 48), splitter.holds(DEVICE, 0, 52), splitter.holds(OTHER_DEVICE, 0, 40)), (Some(Rest), Some(Rest), Some(Pick)));
        splitter.release_input(DEVICE);
        assert_eq!(run(&mut splitter, &[]), [(Rest, 0, off(52))]);
        splitter.release_input(Source::BROADCAST);
        splitter.release_input(Source::splitter(0, Pick));
        assert!(run(&mut splitter, &[]).is_empty(), "no source of the splitter's own, and no panic, is a source to let go of");
        assert_eq!(splitter.keys_held(), 2);
        // The device's key came up after it went: it is not held, so the note-off is dropped.
        assert!(run(&mut splitter, &[(DEVICE, 0, off(52))]).is_empty());
    }

    #[test]
    fn a_panic_ends_what_the_splitter_holds_of_every_source_and_is_not_passed_on() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &[(KEYS, 0, on(48)), (DEVICE, 0, on(52)), (KEYS, 0, [0xB0, 64, 127]), (KEYS, 0, [0xE0, 0, 96])]);
        let panic = [
            (Source::BROADCAST, 0, [0xB0, 64, 0]),
            (Source::BROADCAST, 0, off(48)),
            (Source::BROADCAST, 0, off(52)),
            (Source::BROADCAST, 0, [0xB0, 123, 0]),
            (Source::BROADCAST, 0, [0xE0, 0x00, 0x40]),
        ];
        assert!(run(&mut splitter, &panic).is_empty(), "the lanes and the plugins hear it as it is");
        assert_eq!((splitter.keys_held(), splitter.held[0].pedal, splitter.held[0].bent), (0, 0, 0));
        // The notes are over, so a new key is judged against nothing.
        assert_eq!(run(&mut splitter, &keys(&[(0, on(70))])), [(Pick, 0, on(70))]);
    }

    #[test]
    fn all_notes_off_from_a_source_ends_the_notes_of_its_channel_and_goes_to_both_outputs() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &keys(&[(0, on(48)), (0, on(52)), (0, [0x91, 40, 90])]));
        assert_eq!(run(&mut splitter, &keys(&[(0, [0xB0, 123, 0])])), [(Pick, 0, [0xB0, 123, 0]), (Rest, 0, [0xB0, 123, 0])]);
        assert_eq!((splitter.holds(KEYS, 0, 48), splitter.holds(KEYS, 0, 52), splitter.holds(KEYS, 1, 40)), (None, None, Some(Pick)), "the notes of channel 1 only");
        assert_eq!(run(&mut splitter, &keys(&[(0, [0xB1, 120, 0])])), [(Pick, 0, [0xB1, 120, 0]), (Rest, 0, [0xB1, 120, 0])]);
        assert_eq!(splitter.keys_held(), 0);
        assert!(run(&mut splitter, &keys(&[(0, off(48))])).is_empty(), "a note-off for a note that was ended is dropped");
    }

    #[test]
    fn what_is_picked_can_change_and_a_note_that_is_held_stays_where_it_is() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &keys(&[(0, on(48))]));
        splitter.set_settings(Settings { pick: Pick_::Highest, ..Settings::default() });
        assert_eq!(splitter.holds(KEYS, 0, 48), Some(Pick), "it does not move");
        assert_eq!(run(&mut splitter, &keys(&[(0, on(40))])), [(Rest, 0, on(40))], "and a note is judged by the new rule: the C is held, and the lower note is not the highest");
        assert_eq!(run(&mut splitter, &keys(&[(0, on(60))])), [(Pick, 0, on(60)), (Pick, 0, off(48)), (Rest, 0, on(48))], "a note that is the highest takes the pick from the C, which had it from before: the C is the rest's now");
    }

    #[test]
    fn the_frames_are_the_events_own_and_a_block_with_nothing_in_it_makes_nothing() {
        let mut splitter = Splitter::new(0);
        assert!(run(&mut splitter, &[]).is_empty());
        let made = run(&mut splitter, &keys(&[(0, on(60)), (7, [0xB0, 1, 5]), (255, on(48))]));
        assert_eq!(made, [(Rest, 0, on(60)), (Pick, 7, [0xB0, 1, 5]), (Rest, 7, [0xB0, 1, 5]), (Pick, 255, on(48))]);
        assert!(run(&mut splitter, &[]).is_empty(), "what it made is for one block");
    }

    #[test]
    fn a_block_that_has_more_than_there_is_room_for_drops_notes_and_never_leaves_one_on() {
        let mut splitter = Splitter::new(0);
        // Three sources, all the pitches on all the channels at once: far more than a block has room for.
        let mut begin = Vec::new();
        for source in [KEYS, DEVICE, OTHER_DEVICE] {
            for channel in 0..16u8 {
                for note in 0..128u8 {
                    begin.push((source, 0u64, [0x90 | channel, note, 90]));
                }
            }
        }
        let made = run(&mut splitter, &begin);
        let begun = made.iter().filter(|&&(_, _, bytes)| bytes[0] & 0xF0 == 0x90).count();
        assert!(begun > 0 && begun <= MAX_EVENTS_PER_BLOCK - RESERVED_FOR_ENDS, "{begun}");
        assert_eq!(splitter.keys_held(), begun, "a note that was dropped is not held");
        let ends: Vec<(Source, u64, [u8; 3])> = begin.iter().map(|&(source, frame, bytes)| (source, frame, [bytes[0] & 0x0F | 0x80, bytes[1], 0])).collect();
        let ended = run(&mut splitter, &ends).len();
        assert_eq!(ended, begun, "every note that began ends, and none that was dropped does");
        assert_eq!(splitter.keys_held(), 0);
    }

    #[test]
    fn a_block_with_more_events_than_it_looks_over_still_judges_each_note_against_what_is_held() {
        // The first note is the last event that is looked over, and the second is the first that is not: neither is compared with the other, whatever is picked.
        for (pick, first, second) in [(Pick_::Lowest, 60u8, 50u8), (Pick_::Highest, 50, 60)] {
            let mut splitter = Splitter::new(0);
            splitter.set_settings(Settings { pick, inputs: only(&[KEYS]) });
            // (The events before the notes are a device's, which is not heard: they make nothing, and they are looked over.)
            let mut block: Vec<(Source, u64, [u8; 3])> = (0..MAX_LOOKED_OVER - 1).map(|at| (DEVICE, 0, [0xB0, 1, (at % 100) as u8])).collect();
            block.push((KEYS, 1, on(first)));
            block.push((KEYS, 2, on(second)));
            let made = run(&mut splitter, &block);
            let notes: Vec<_> = made.iter().filter(|&&(_, _, bytes)| bytes[0] == 0x90).collect();
            assert_eq!(
                notes,
                [&(Pick, 1, on(first)), &(Pick, 2, on(second)), &(Rest, 2, on(first))],
                "past what is looked over a note is judged as it comes, against the keys that are held: both are picks, picking the {pick:?}, and the second takes the pick from the first, which goes to the rest"
            );
        }
    }

    #[test]
    fn a_pitch_over_127_is_the_pitch_without_its_high_bit_and_is_no_panic() {
        let mut splitter = Splitter::new(0);
        assert_eq!(run(&mut splitter, &keys(&[(0, [0x90, 0xC8, 100])])), [(Pick, 0, [0x90, 0xC8, 100])], "200 is 72, as it was said");
        assert_eq!(splitter.holds(KEYS, 0, 72), Some(Pick));
        assert_eq!(run(&mut splitter, &keys(&[(1, [0xA0, 0xC8, 5]), (2, [0x80, 0xC8, 0])])), [(Pick, 1, [0xA0, 0xC8, 5]), (Pick, 2, [0x80, 0xC8, 0])]);
        assert_eq!(splitter.keys_held(), 0);
        // (A later note of the block is judged as the pitch it is: 0xC8 is 72, and the 60 that follows is lower.)
        assert_eq!(run(&mut splitter, &keys(&[(0, [0x90, 0xC8, 100]), (1, on(60))])), [(Rest, 0, [0x90, 0xC8, 100]), (Pick, 1, on(60))]);
    }

    #[test]
    fn the_lowest_and_the_highest_pitches_there_are_are_judged_like_the_others() {
        let mut splitter = Splitter::new(0);
        assert_eq!(run(&mut splitter, &keys(&[(0, on(127)), (1, on(5)), (2, on(0))])), [(Rest, 0, on(127)), (Rest, 1, on(5)), (Pick, 2, on(0))], "the C at the bottom of the keys is lower than the others");
        let mut splitter = Splitter::new(0);
        splitter.set_settings(Settings { pick: Pick_::Highest, ..Settings::default() });
        assert_eq!(run(&mut splitter, &keys(&[(0, on(0)), (1, on(122)), (2, on(127))])), [(Rest, 0, on(0)), (Rest, 1, on(122)), (Pick, 2, on(127))], "and the G at the top is higher than the others");
        // The key at the very top goes up, and the next key is compared with what is held now: the 122 and the 0.
        assert_eq!(run(&mut splitter, &keys(&[(0, off(127)), (1, on(126))])), [(Pick, 0, off(127)), (Pick, 1, on(126))], "the 126 is higher than the 122, which is the highest that is held now");
    }

    #[test]
    fn the_highest_note_ties_as_the_lowest_does() {
        let mut splitter = Splitter::new(0);
        splitter.set_settings(Settings { pick: Pick_::Highest, ..Settings::default() });
        assert_eq!(run(&mut splitter, &[(KEYS, 0, on(60)), (DEVICE, 0, on(60))]), [(Pick, 0, on(60)), (Rest, 0, on(60))], "the earlier of two that are equal is the pick, and the other, which is not higher than what is held, the rest");
    }

    #[test]
    fn the_pedal_is_down_from_64_up_and_a_wheel_is_away_unless_it_is_at_8192() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &keys(&[(0, [0xB0, 64, 63])]));
        assert_eq!(splitter.held[0].pedal, 0, "63 is up");
        run(&mut splitter, &keys(&[(0, [0xB0, 64, 64])]));
        assert_eq!(splitter.held[0].pedal, 1, "64 is down");
        run(&mut splitter, &keys(&[(0, [0xE0, 0x01, 0x40])]));
        assert_eq!(splitter.held[0].bent, 1, "8193 is away from the centre");
        run(&mut splitter, &keys(&[(0, [0xE0, 0x00, 0x40])]));
        assert_eq!(splitter.held[0].bent, 0, "8192 is the centre");
        run(&mut splitter, &keys(&[(0, [0xE0, 0x7F, 0x3F])]));
        assert_eq!(splitter.held[0].bent, 1, "8191 is away from it");
    }

    #[test]
    fn a_panic_ends_only_what_it_says_a_note_that_begins_is_none_and_the_pedal_and_the_wheel_go_back_only_when_it_says_so() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &[(KEYS, 0, on(48)), (KEYS, 0, [0xB0, 64, 127]), (KEYS, 0, [0xE0, 0, 96]), (DEVICE, 0, [0x91, 52, 100])]);
        let nothing = [
            (Source::BROADCAST, 0, [0x90, 48, 100]),
            (Source::BROADCAST, 0, [0xB0, 64, 64]),
            (Source::BROADCAST, 0, [0xE0, 0, 96]),
            (Source::BROADCAST, 0, [0xB0, 7, 0]),
            (Source::BROADCAST, 0, [0xB1, 123, 0]),
            (Source::BROADCAST, 0, off(52)),
        ];
        assert!(run(&mut splitter, &nothing).is_empty());
        assert_eq!((splitter.holds(KEYS, 0, 48), splitter.held[0].pedal, splitter.held[0].bent), (Some(Pick), 1, 1), "a note that begins, a pedal at 64 and a wheel that is away from its centre, and the controllers of another channel, are not a panic");
        assert_eq!(splitter.holds(DEVICE, 1, 52), None, "the all notes off of channel 2 ends the device's note on channel 2, and nothing else");
        run(&mut splitter, &[(Source::BROADCAST, 0, [0xB0, 64, 63]), (Source::BROADCAST, 0, [0xE0, 0x00, 0x40])]);
        assert_eq!((splitter.held[0].pedal, splitter.held[0].bent), (0, 0), "a pedal below 64 and a wheel at its centre are");
        run(&mut splitter, &[(Source::BROADCAST, 0, [0xB0, 120, 0])]);
        assert_eq!(splitter.keys_held(), 0, "all sound off ends the notes of its channel as all notes off does");
        run(&mut splitter, &keys(&[(0, on(50))]));
        run(&mut splitter, &[(Source::BROADCAST, 0, [0x90, 50, 0])]);
        assert_eq!(splitter.keys_held(), 0, "and a note-on with no velocity ends a note, as it always does");
    }

    #[test]
    fn a_note_off_in_a_panic_ends_the_note_wherever_it_went() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &keys(&[(0, on(48)), (0, on(52))]));
        assert_eq!((splitter.holds(KEYS, 0, 48), splitter.holds(KEYS, 0, 52)), (Some(Pick), Some(Rest)));
        assert!(run(&mut splitter, &[(Source::BROADCAST, 0, off(52))]).is_empty());
        assert_eq!((splitter.holds(KEYS, 0, 48), splitter.holds(KEYS, 0, 52)), (Some(Pick), None), "the rest's note ended, and the pick's did not");
        run(&mut splitter, &[(Source::BROADCAST, 0, off(48))]);
        assert_eq!(splitter.keys_held(), 0, "and the pick's ended in its turn");
    }

    #[test]
    fn the_last_input_and_the_last_box_are_heard_like_the_others_and_let_go_of_like_them() {
        let mut splitter = Splitter::new(0);
        let (last_input, last_box) = (Source::LAST_INPUT, Source::pattern(15));
        splitter.set_settings(Settings { inputs: only(&[last_input, last_box]), ..Settings::default() });
        assert_eq!(splitter.settings.inputs, only(&[last_input, last_box]), "both are sources that a splitter can hear");
        assert_eq!(run(&mut splitter, &[(last_input, 0, on(60)), (last_box, 1, on(55))]), [(Rest, 0, on(60)), (Pick, 1, on(55))]);
        splitter.release_input(last_box);
        assert_eq!(run(&mut splitter, &[]), [(Pick, 0, off(55))]);
        splitter.release_input(last_input);
        assert_eq!(run(&mut splitter, &[]), [(Rest, 0, off(60))]);
        assert_eq!(splitter.keys_held(), 0);
    }

    #[test]
    fn what_the_channels_of_the_last_numbers_hold_is_let_go_of_as_the_others_are() {
        let mut splitter = Splitter::new(0);
        run(&mut splitter, &keys(&[(0, [0x9F, 48, 100]), (0, [0x9F, 52, 100]), (0, [0xBF, 64, 127]), (0, [0xEF, 0, 100])]));
        splitter.release_input(KEYS);
        assert_eq!(
            run(&mut splitter, &[]),
            [(Pick, 0, [0x8F, 48, 0]), (Rest, 0, [0x8F, 52, 0]), (Pick, 0, [0xBF, 64, 0]), (Rest, 0, [0xBF, 64, 0]), (Pick, 0, [0xEF, 0x00, 0x40]), (Rest, 0, [0xEF, 0x00, 0x40])],
            "the sixteenth channel is let go of too"
        );
    }

    #[test]
    fn a_block_of_controllers_has_room_for_all_but_what_is_kept_for_the_ends_of_notes() {
        let mut splitter = Splitter::new(0);
        let block: Vec<(Source, u64, [u8; 3])> = (0..3000).map(|at| (KEYS, 0, [0xB0, 1, (at % 100) as u8])).chain([(KEYS, 1, on(60))]).collect();
        let made = run(&mut splitter, &block);
        assert_eq!(made.len(), MAX_EVENTS_PER_BLOCK - RESERVED_FOR_ENDS, "the controllers go to both outputs until the room is used, and the note, which is not an end, has none");
        assert_eq!(splitter.keys_held(), 0, "a note that there is no room for is not held");
    }

    #[test]
    fn the_ends_of_notes_have_room_that_nothing_else_has() {
        let mut splitter = Splitter::new(0);
        // 300 keys held (three channels of a hundred pitches).
        let down: Vec<(Source, u64, [u8; 3])> = (0..3u8).flat_map(|channel| (0..100u8).map(move |note| (KEYS, 0, [0x90 | channel, note, 90]))).collect();
        assert_eq!(run(&mut splitter, &down).len(), 300);
        // A block that fills the room with controllers and then lets go of all of them: the last 256 of the room are the ends'.
        let mut block: Vec<(Source, u64, [u8; 3])> = (0..1920).map(|at| (KEYS, 0, [0xB0, 1, (at % 100) as u8])).collect();
        block.extend((0..3u8).flat_map(|channel| (0..100u8).map(move |note| (KEYS, 1, [0x80 | channel, note, 0]))));
        let made = run(&mut splitter, &block);
        assert_eq!(made.len(), MAX_EVENTS_PER_BLOCK, "the controllers stop at what is kept, and the ends go on to the end of the room");
        assert_eq!(made.iter().filter(|&&(_, _, bytes)| bytes[0] & 0xF0 == 0x80).count(), RESERVED_FOR_ENDS);
    }

    // ---- the randomised test: a reference model that is as simple as it can be ----------------------------------------------------------

    /// The same rule as the splitter's, with plain lists: the keys that are held (the source, the channel, the pitch, the output and how hard it was last struck), and what comes later is looked at by going down the rest of the block.
    struct Reference {
        pick: Pick_,
        inputs: SourceSet,
        held: Vec<(Source, u8, u8, Output, u8)>,
    }

    impl Reference {
        fn block(&mut self, block: &[(Source, u64, [u8; 3])]) -> Vec<(Output, u64, [u8; 3])> {
            let mut made = Vec::new();
            for (at, &(source, frame, bytes)) in block.iter().enumerate() {
                if source.is_broadcast() {
                    let channel = bytes[0] & 0x0F;
                    match (bytes[0] & 0xF0, bytes[1]) {
                        (0x80, note) | (0x90, note) if bytes[0] & 0xF0 == 0x80 || bytes[2] == 0 => self.held.retain(|&(_, c, n, _, _)| !(c == channel && n == note)),
                        (0xB0, 120) | (0xB0, 123) => self.held.retain(|&(_, c, _, _, _)| c != channel),
                        _ => {}
                    }
                    continue;
                }
                if !self.inputs.contains(source) {
                    continue;
                }
                let (channel, note) = (bytes[0] & 0x0F, bytes[1] & 0x7F);
                let where_it_is = self.held.iter().find(|&&(s, c, n, _, _)| s == source && c == channel && n == note).map(|&(_, _, _, output, _)| output);
                match bytes[0] & 0xF0 {
                    0x90 if bytes[2] > 0 => {
                        let output = where_it_is.unwrap_or_else(|| {
                            let later = block[at + 1..].iter().filter(|&&(s, _, b)| !s.is_broadcast() && self.inputs.contains(s) && b[0] & 0xF0 == 0x90 && b[2] > 0).map(|&(_, _, b)| b[1] & 0x7F);
                            let picked = match self.pick {
                                Pick_::Lowest => self.held.iter().all(|&(_, _, n, _, _)| note < n) && later.clone().all(|n| note <= n),
                                Pick_::Highest => self.held.iter().all(|&(_, _, n, _, _)| note > n) && later.clone().all(|n| note >= n),
                            };
                            if picked { Pick } else { Rest }
                        });
                        made.push((output, frame, bytes));
                        if where_it_is.is_some() {
                            // Struck again while it is down: it is one key, and how hard it was struck is the last time.
                            for key in self.held.iter_mut().filter(|key| (key.0, key.1, key.2) == (source, channel, note)) {
                                key.4 = bytes[2];
                            }
                        } else {
                            if output == Pick {
                                // The keys that have the pick let go of it, after this key begins: each ends on the pick and begins on the rest as it did, in the order of the sources, the channels and the pitches.
                                let mut letting_go: Vec<usize> = (0..self.held.len()).filter(|&at| self.held[at].3 == Pick).collect();
                                letting_go.sort_by_key(|&at| (self.held[at].0.number(), self.held[at].1, self.held[at].2));
                                for at in letting_go {
                                    let (_, c, n, _, velocity) = self.held[at];
                                    self.held[at].3 = Rest;
                                    made.extend([(Pick, frame, [0x80 | c, n, 0]), (Rest, frame, [0x90 | c, n, velocity])]);
                                }
                            }
                            self.held.push((source, channel, note, output, bytes[2]));
                        }
                    }
                    0x80 | 0x90 => {
                        if let Some(output) = where_it_is {
                            self.held.retain(|&(s, c, n, _, _)| !(s == source && c == channel && n == note));
                            made.push((output, frame, bytes));
                        }
                    }
                    0xA0 => {
                        if let Some(output) = where_it_is {
                            made.push((output, frame, bytes));
                        }
                    }
                    0xB0 if bytes[1] == 120 || bytes[1] == 123 => {
                        self.held.retain(|&(s, c, _, _, _)| !(s == source && c == channel));
                        made.extend([(Pick, frame, bytes), (Rest, frame, bytes)]);
                    }
                    _ => made.extend([(Pick, frame, bytes), (Rest, frame, bytes)]),
                }
            }
            made
        }
    }

    /// A small deterministic generator (xorshift): the same blocks every run.
    struct Dice(u64);

    impl Dice {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    #[test]
    fn a_splitter_does_what_a_list_of_the_keys_that_are_held_does_in_ten_thousand_random_blocks_and_nothing_is_ever_left_on() {
        for pick in [Pick_::Lowest, Pick_::Highest] {
            let mut dice = Dice(0x9E37_79B9_7F4A_7C15);
            let mut splitter = Splitter::new(1);
            let inputs = only(&[KEYS, DEVICE, Source::pattern(0)]);
            splitter.set_settings(Settings { pick, inputs });
            let mut reference = Reference { pick, inputs, held: Vec::new() };
            let heard = [KEYS, DEVICE, Source::pattern(0), OTHER_DEVICE, Source::BROADCAST];
            // What each output has sounding, by channel and pitch, from what the splitter says: it must never go below 0, and be 0 at the end.
            let mut sounding = [[[0i32; 128]; 16]; 2];
            for block in 0..10_000 {
                let count = dice.below(9);
                let mut frame = 0;
                let events: Vec<(Source, u64, [u8; 3])> = (0..count)
                    .map(|_| {
                        frame += dice.below(40);
                        let source = heard[dice.below(5) as usize];
                        let channel = dice.below(3) as u8;
                        let note = 48 + dice.below(10) as u8;
                        let bytes = match dice.below(14) {
                            0..=5 => [0x90 | channel, note, 1 + dice.below(127) as u8],
                            6..=9 => [0x80 | channel, note, 0],
                            10 => [0x90 | channel, note, 0],
                            11 => [0xA0 | channel, note, dice.below(128) as u8],
                            12 => [[0xB0, 0xE0, 0xD0][dice.below(3) as usize] | channel, [64, 1, 123, 120][dice.below(4) as usize], dice.below(128) as u8],
                            _ => [0xB0 | channel, 123, 0],
                        };
                        (source, frame, bytes)
                    })
                    .collect();
                let made = run(&mut splitter, &events);
                assert_eq!(made, reference.block(&events), "block {block}, picking the {pick:?}: {events:02x?}");
                for &(output, _, bytes) in &made {
                    let (channel, note) = (usize::from(bytes[0] & 0x0F), usize::from(bytes[1] & 0x7F));
                    match (bytes[0] & 0xF0, bytes[2]) {
                        (0x90, velocity) if velocity > 0 => sounding[output as usize][channel][note] += 1,
                        (0x80, _) | (0x90, _) => sounding[output as usize][channel][note] -= 1,
                        _ => {}
                    }
                    // (A note that is struck again while it is down is one key and has two note-ons: the count may be 2, and a panic or an all-notes-off ends what it ends without a note-off.)
                }
                // A key is on exactly one output, and the splitter and the list agree on which.
                for &(source, channel, note, output, _) in &reference.held {
                    assert_eq!(splitter.holds(source, usize::from(channel), note), Some(output), "block {block}");
                }
                assert_eq!(splitter.keys_held(), reference.held.len(), "block {block}");
            }
            // Everything ends: a note-off for every key that is held, and nothing is left sounding on either output except what an all-notes-off ended (which the outputs' controllers say).
            let ends: Vec<(Source, u64, [u8; 3])> = reference.held.iter().map(|&(source, channel, note, _, _)| (source, 0, [0x80 | channel, note, 0])).collect();
            run(&mut splitter, &ends);
            assert_eq!(splitter.keys_held(), 0);
            assert!(splitter.pitches == 0 && splitter.counts.iter().all(|&count| count == 0), "the counts of the pitches that are held are 0 when no key is");
            assert!(sounding.iter().flatten().flatten().all(|&n| n >= 0), "no output was told that a note ended more often than it began");
        }
    }

    #[test]
    fn every_note_that_an_output_begins_ends_on_that_output_when_nothing_ends_it_but_its_own_note_off_and_the_pick_has_one_note_at_the_most() {
        for pick in [Pick_::Lowest, Pick_::Highest] {
            let mut dice = Dice(0xD1B5_4A32_D192_ED03);
            let mut splitter = Splitter::new(2);
            let mut down: Vec<(Source, u8, u8)> = Vec::new();
            // What each output has sounding, by channel and pitch, from what the splitter says.
            let mut sounding = [[[0i32; 128]; 16]; 2];
            splitter.set_settings(Settings { pick, ..Settings::default() });
            for block in 0..5_000 {
                let mut frame = 0;
                let mut events = Vec::new();
                for _ in 0..dice.below(7) {
                    frame += dice.below(30);
                    let (source, channel, note) = ([KEYS, DEVICE][dice.below(2) as usize], dice.below(3) as u8, 40 + dice.below(24) as u8);
                    // A key that is down is let go of, and one that is up is pressed: no key is struck twice.
                    if let Some(at) = down.iter().position(|&key| key == (source, channel, note)) {
                        down.remove(at);
                        events.push((source, frame, [0x80 | channel, note, 0]));
                    } else {
                        down.push((source, channel, note));
                        events.push((source, frame, [0x90 | channel, note, 100]));
                    }
                }
                for (output, _, bytes) in run(&mut splitter, &events) {
                    let (channel, note) = (usize::from(bytes[0] & 0x0F), usize::from(bytes[1]));
                    match bytes[0] & 0xF0 {
                        0x90 => sounding[output as usize][channel][note] += 1,
                        _ => sounding[output as usize][channel][note] -= 1,
                    }
                    assert!(sounding[output as usize][channel][note] >= 0, "block {block}: a note ended on an output that had not begun it");
                }
                // A key of a source is on one output only, so that the two sources that can hold a pitch on a channel have at most two notes between the outputs.
                for (channel, (on_pick, on_rest)) in sounding[0].iter().zip(&sounding[1]).enumerate() {
                    for (note, (picked, rest)) in on_pick.iter().zip(on_rest).enumerate() {
                        assert!(picked + rest <= 2, "block {block}: channel {channel}, note {note}");
                    }
                }
                // The pick has one note at the most: a key that takes it has it from the key that had it, and the lanes are told so.
                let on_the_pick: i32 = sounding[Pick as usize].iter().flatten().sum();
                assert!(on_the_pick <= 1, "block {block}: the pick has {on_the_pick} notes, picking the {pick:?}");
            }
            let ends: Vec<(Source, u64, [u8; 3])> = down.iter().map(|&(source, channel, note)| (source, 0, [0x80 | channel, note, 0])).collect();
            for (output, _, bytes) in run(&mut splitter, &ends) {
                sounding[output as usize][usize::from(bytes[0] & 0x0F)][usize::from(bytes[1])] -= 1;
            }
            assert!(sounding.iter().flatten().flatten().all(|&n| n == 0), "every note that an output began ended, on that output, picking the {pick:?}");
            assert_eq!(splitter.keys_held(), 0);
        }
    }

    #[test]
    fn a_splitter_asks_the_allocator_for_nothing_in_a_block_in_which_it_judges_lets_go_and_hears_a_panic() {
        let mut splitter = Splitter::new(0);
        let chord = keys(&[(0, on(55)), (1, on(52)), (2, on(48)), (3, [0xB0, 64, 127]), (4, [0xE0, 0, 96])]);
        let list: Vec<(u64, [u8; 3])> = chord.iter().map(|&(_, frame, bytes)| (frame, bytes)).collect();
        let sources: Vec<Source> = chord.iter().map(|&(source, _, _)| source).collect();
        let panic = [(0u64, [0xB0u8, 123, 0]), (0, [0x80, 48, 0])];
        let panic_sources = [Source::BROADCAST; 2];
        let lower = [(0u64, on(40))];
        let lower_sources = [KEYS];
        let ((), allocations) = allocations_in(|| {
            for _ in 0..100 {
                splitter.process(Events::new(&list, &sources));
                splitter.process(Events::new(&panic, &panic_sources));
                splitter.process(Events::new(&list, &sources));
                // A key that is lower than the pick takes it: the pick lets go.
                splitter.process(Events::new(&lower, &lower_sources));
                splitter.set_settings(Settings { pick: Pick_::Highest, inputs: only(&[DEVICE]) });
                splitter.process(Events::new(&[], &[]));
                splitter.set_settings(Settings::default());
                splitter.release_input(KEYS);
                splitter.process(Events::new(&list, &sources));
            }
        });
        assert_eq!(allocations, 0);
    }
}
