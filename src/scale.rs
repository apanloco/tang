//! Musical scales and tonality for the Piano tab.
//!
//! A `ScaleSetting` is a (root, scale) pair, e.g. C Major or F# Dorian.
//! Used by the Piano tab to highlight scale tones and the root note.

pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

pub const SCALE_DEGREES: [&str; 13] = [
    "1", "b2", "2", "b3", "3", "4", "b5", "5", "b6", "6", "b7", "7", "8",
];

#[derive(Clone, Copy, Debug)]
pub struct Scale {
    pub name: &'static str,
    pub short: &'static str,
    pub intervals: &'static [u8],
}

pub const SCALES: &[Scale] = &[
    Scale { name: "Major",            short: "major",      intervals: &[0, 2, 4, 5, 7, 9, 11] },
    Scale { name: "Natural Minor",    short: "minor",      intervals: &[0, 2, 3, 5, 7, 8, 10] },
    Scale { name: "Harmonic Minor",   short: "harm minor", intervals: &[0, 2, 3, 5, 7, 8, 11] },
    Scale { name: "Melodic Minor",    short: "mel minor",  intervals: &[0, 2, 3, 5, 7, 9, 11] },
    Scale { name: "Dorian",           short: "dorian",     intervals: &[0, 2, 3, 5, 7, 9, 10] },
    Scale { name: "Phrygian",         short: "phrygian",   intervals: &[0, 1, 3, 5, 7, 8, 10] },
    Scale { name: "Lydian",           short: "lydian",     intervals: &[0, 2, 4, 6, 7, 9, 11] },
    Scale { name: "Mixolydian",       short: "mixolydian", intervals: &[0, 2, 4, 5, 7, 9, 10] },
    Scale { name: "Locrian",          short: "locrian",    intervals: &[0, 1, 3, 5, 6, 8, 10] },
    Scale { name: "Major Pentatonic", short: "maj pent",   intervals: &[0, 2, 4, 7, 9] },
    Scale { name: "Minor Pentatonic", short: "min pent",   intervals: &[0, 3, 5, 7, 10] },
    Scale { name: "Blues",            short: "blues",      intervals: &[0, 3, 5, 6, 7, 10] },
    Scale { name: "Whole Tone",       short: "whole tone", intervals: &[0, 2, 4, 6, 8, 10] },
    Scale { name: "Diminished",       short: "diminished", intervals: &[0, 2, 3, 5, 6, 8, 9, 11] },
    Scale { name: "Chromatic",        short: "chromatic",  intervals: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11] },
];

/// The tone of a scale that is nearest to `offset`, a number of semitones from the scale's root (any real number, in any octave): `intervals` are the scale's, from 0, within an octave. Halfway
/// between two tones the lower one is chosen. The oscillator's quantizer goes through it, so that a detune that a modulator moves is heard at a tone of a scale counted from the key that is held.
pub fn nearest_tone(intervals: &[u8], offset: f32) -> f32 {
    let octave = (offset / 12.0).floor();
    let within = offset - 12.0 * octave;
    let mut nearest = 0.0f32;
    let mut distance = f32::MAX;
    // (The root of the next octave is a tone too, and a tone that is not nearer than the one before it is not chosen: of two as near, the lower one.)
    for tone in intervals.iter().map(|&interval| f32::from(interval)).chain(std::iter::once(12.0)) {
        if (within - tone).abs() < distance {
            nearest = tone;
            distance = (within - tone).abs();
        }
    }
    12.0 * octave + nearest
}

/// A specific tonality: root pitch class (0..12, C=0) and a scale shape.
/// Default is C Major (root=0, scale_idx=0).
#[derive(Clone, Copy, Debug, Default)]
pub struct ScaleSetting {
    pub root: u8,
    pub scale_idx: usize,
}

impl ScaleSetting {
    /// True if `midi_note`'s pitch class is in the scale.
    pub fn contains(&self, midi_note: u8) -> bool {
        let scale = &SCALES[self.scale_idx];
        let pc = midi_note % 12;
        let offset = (pc as i16 - self.root as i16).rem_euclid(12) as u8;
        scale.intervals.contains(&offset)
    }

    /// True if `midi_note` has the same pitch class as the root.
    pub fn is_root(&self, midi_note: u8) -> bool {
        midi_note % 12 == self.root
    }

    /// Scale degree of a note (e.g. "1", "b3", "5") if it's in the scale.
    pub fn degree(&self, midi_note: u8) -> Option<&'static str> {
        let scale = &SCALES[self.scale_idx];
        let pc = midi_note % 12;
        let offset = (pc as i16 - self.root as i16).rem_euclid(12) as u8;
        // Map offset → degree label. We use the chromatic degree labels.
        if scale.intervals.contains(&offset) {
            Some(SCALE_DEGREES[offset as usize])
        } else {
            None
        }
    }

    /// Map a note to its position in this scale: a scale index (degrees count
    /// across octaves, anchored at the root) plus a chromatic offset in
    /// semitones for off-scale notes. The offset is measured from the nearest
    /// scale tone at or below the note, so `note_at(index_of(n)) == n` always.
    pub fn index_of(&self, note: u8) -> (i32, u8) {
        let intervals = SCALES[self.scale_idx].intervals;
        let rel = note as i32 - self.root as i32;
        let octave = rel.div_euclid(12);
        let pc = rel.rem_euclid(12) as u8;
        // intervals[0] is always 0, so a position always exists.
        let pos = intervals.iter().rposition(|&iv| iv <= pc).unwrap_or(0);
        (
            octave * intervals.len() as i32 + pos as i32,
            pc - intervals[pos],
        )
    }

    /// Inverse of `index_of`: the MIDI note (possibly out of 0..=127) at a
    /// scale index, shifted up by a chromatic offset.
    pub fn note_at(&self, index: i32, offset: u8) -> i32 {
        let intervals = SCALES[self.scale_idx].intervals;
        let len = intervals.len() as i32;
        let octave = index.div_euclid(len);
        let pos = index.rem_euclid(len) as usize;
        self.root as i32 + octave * 12 + intervals[pos] as i32 + offset as i32
    }

    /// Transpose `note` by scale degrees: the degree distance from `from` to
    /// `to`, instead of their semitone distance. Off-scale `from`/`to` snap to
    /// the scale tone below; an off-scale `note` keeps its chromatic offset
    /// from the scale tone below (passing tones survive). `from == to` always
    /// returns `note` unchanged. The result is clamped to MIDI range.
    pub fn transpose_in_key(&self, note: u8, from: u8, to: u8) -> u8 {
        let shift = self.index_of(to).0 - self.index_of(from).0;
        let (index, offset) = self.index_of(note);
        self.note_at(index + shift, offset).clamp(0, 127) as u8
    }

    /// Pretty display name, e.g. "C Major", "F# Dorian".
    pub fn display(&self) -> String {
        format!(
            "{} {}",
            NOTE_NAMES[self.root as usize],
            SCALES[self.scale_idx].name
        )
    }

    /// Compact form for serialization, e.g. "C major".
    pub fn short(&self) -> String {
        format!("{} {}", self.root_name(), self.kind())
    }

    /// The name of the root, e.g. "C", "F#".
    pub fn root_name(&self) -> &'static str {
        NOTE_NAMES[self.root as usize]
    }

    /// The short name of the kind of scale, as `short` and the session file say it: "major", "harm minor".
    pub fn kind(&self) -> &'static str {
        SCALES[self.scale_idx].short
    }

    /// The pitch classes of the scale (0 C … 11 B), lowest first: the notes it has in every octave.
    pub fn pitch_classes(&self) -> Vec<u8> {
        let mut classes: Vec<u8> = SCALES[self.scale_idx].intervals.iter().map(|interval| (self.root + interval) % 12).collect();
        classes.sort_unstable();
        classes
    }

    /// Parse a "root scale" string. Accepts sharps ("C#") and flats ("Db").
    /// The scale portion matches either the short or long form, case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        // Try the 5 sharped names first so "C#" doesn't mis-match as "C".
        const SHARPS: &[(&str, u8)] = &[
            ("C#", 1), ("D#", 3), ("F#", 6), ("G#", 8), ("A#", 10),
        ];
        const FLATS: &[(&str, u8)] = &[
            ("Db", 1), ("Eb", 3), ("Gb", 6), ("Ab", 8), ("Bb", 10),
        ];
        const NATURALS: &[(&str, u8)] = &[
            ("C", 0), ("D", 2), ("E", 4), ("F", 5), ("G", 7), ("A", 9), ("B", 11),
        ];
        let try_match = |table: &[(&str, u8)]| -> Option<(u8, &str)> {
            for (name, root) in table {
                // (`get`, not a slice: text that is not ASCII can have a letter that the name's length ends in the middle of.)
                if s.get(..name.len()).is_some_and(|head| head.eq_ignore_ascii_case(name)) {
                    return Some((*root, &s[name.len()..]));
                }
            }
            None
        };
        let (root, rest) = try_match(SHARPS)
            .or_else(|| try_match(FLATS))
            .or_else(|| try_match(NATURALS))?;
        let rest = rest.trim();
        for (idx, scale) in SCALES.iter().enumerate() {
            if rest.eq_ignore_ascii_case(scale.short) || rest.eq_ignore_ascii_case(scale.name) {
                return Some(Self { root, scale_idx: idx });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intervals_of(short: &str) -> &'static [u8] {
        SCALES.iter().find(|scale| scale.short == short).unwrap_or_else(|| panic!("no scale called {short}")).intervals
    }

    #[test]
    fn the_nearest_tone_is_the_tone_of_the_scale_that_is_nearest_in_any_octave_and_the_lower_one_halfway() {
        // (A number of semitones from the root, and the tone of the scale that it goes to.)
        let minor_pentatonic = intervals_of("min pent");
        for (offset, tone) in [
            (0.0, 0.0), (1.4, 0.0), (1.5, 0.0), (1.6, 3.0), (3.9, 3.0), (4.0, 3.0), (4.1, 5.0), (6.0, 5.0), (6.2, 7.0), (8.5, 7.0), (8.6, 10.0), (11.0, 10.0), (11.2, 12.0), (12.0, 12.0), (14.9, 15.0),
            (-0.4, 0.0), (-1.6, -2.0), (-2.0, -2.0), (-2.5, -2.0), (-12.0, -12.0), (24.0, 24.0), (-24.0, -24.0),
        ] {
            assert_eq!(nearest_tone(minor_pentatonic, offset), tone, "{offset} in the minor pentatonic");
        }
        // A scale with every semitone is rounding (halfway, down), and a major scale has its two whole steps and its half steps.
        let chromatic = intervals_of("chromatic");
        for (offset, tone) in [(0.4, 0.0), (0.5, 0.0), (0.6, 1.0), (1.5, 1.0), (-0.5, -1.0), (-0.6, -1.0), (23.7, 24.0)] {
            assert_eq!(nearest_tone(chromatic, offset), tone, "{offset} in the chromatic scale");
        }
        let major = intervals_of("major");
        for (offset, tone) in [(3.0, 2.0), (3.4, 4.0), (6.0, 5.0), (6.1, 7.0), (10.0, 9.0), (10.1, 11.0), (11.6, 12.0)] {
            assert_eq!(nearest_tone(major, offset), tone, "{offset} in the major scale");
        }
    }

    #[test]
    fn the_nearest_tone_of_every_scale_is_a_tone_of_it_and_at_most_half_its_widest_gap_away_and_goes_nowhere_it_is_put_again() {
        for scale in SCALES {
            // The widest step between two tones of the scale, the last of them to the root of the next octave included.
            let widest = scale.intervals.windows(2).map(|w| w[1] - w[0]).chain(std::iter::once(12 - scale.intervals[scale.intervals.len() - 1])).max().unwrap();
            for tenth in -300..=300 {
                let offset = tenth as f32 / 10.0;
                let tone = nearest_tone(scale.intervals, offset);
                assert!(scale.intervals.contains(&(tone.rem_euclid(12.0) as u8)) && tone.fract() == 0.0, "{} at {offset}: {tone}", scale.short);
                assert!((tone - offset).abs() <= f32::from(widest) / 2.0 + 1e-4, "{} at {offset}: {tone} is farther than half of {widest}", scale.short);
                assert_eq!(nearest_tone(scale.intervals, tone), tone, "{} at {offset}", scale.short);
            }
        }
    }

    #[test]
    fn parse_round_trip() {
        let s = ScaleSetting { root: 0, scale_idx: 0 };
        assert_eq!(ScaleSetting::parse(&s.short()).map(|p| (p.root, p.scale_idx)), Some((0, 0)));
        let s = ScaleSetting { root: 6, scale_idx: 4 };
        let parsed = ScaleSetting::parse(&s.short()).unwrap();
        assert_eq!(parsed.root, 6);
        assert_eq!(parsed.scale_idx, 4);
    }

    #[test]
    fn parse_flats() {
        let p = ScaleSetting::parse("Bb minor").unwrap();
        assert_eq!(p.root, 10);
        assert_eq!(SCALES[p.scale_idx].short, "minor");
    }

    #[test]
    fn parse_long_form() {
        let p = ScaleSetting::parse("F# Natural Minor").unwrap();
        assert_eq!(p.root, 6);
        assert_eq!(SCALES[p.scale_idx].short, "minor");
    }

    #[test]
    fn parse_refuses_text_that_is_not_a_scale_whatever_its_letters_are() {
        // (A root is matched by its first bytes; where they end inside a letter of more than one byte, there is no root.)
        for text in ["", "C", "Cé", "é", "C#é major", "日本語", "Db é", "A\u{301} major", "C major é", "banana", "H major"] {
            assert!(ScaleSetting::parse(text).is_none(), "{text:?}");
        }
    }

    #[test]
    fn contains_and_root() {
        let s = ScaleSetting { root: 0, scale_idx: 0 }; // C major
        assert!(s.contains(60)); // C
        assert!(s.contains(62)); // D
        assert!(!s.contains(61)); // C#
        assert!(s.is_root(60));
        assert!(s.is_root(72));
        assert!(!s.is_root(61));
    }

    #[test]
    fn degree() {
        let s = ScaleSetting { root: 0, scale_idx: 0 }; // C major
        assert_eq!(s.degree(60), Some("1"));   // C
        assert_eq!(s.degree(64), Some("3"));   // E
        assert_eq!(s.degree(67), Some("5"));   // G
        assert_eq!(s.degree(61), None);        // C#
    }

    #[test]
    fn index_round_trip() {
        let s = ScaleSetting { root: 9, scale_idx: 1 }; // A minor
        for note in 0..=127u8 {
            let (index, offset) = s.index_of(note);
            assert_eq!(s.note_at(index, offset), note as i32);
        }
    }

    #[test]
    fn in_key_triad_qualities() {
        let s = ScaleSetting { root: 0, scale_idx: 0 }; // C major
        // C major triad on each degree of C major yields the diatonic triads.
        let triad = [60u8, 64, 67]; // C4 E4 G4
        let on = |trigger: u8| -> Vec<u8> {
            triad.iter().map(|&n| s.transpose_in_key(n, 60, trigger)).collect()
        };
        assert_eq!(on(60), vec![60, 64, 67]); // C:  C E G (identity)
        assert_eq!(on(62), vec![62, 65, 69]); // D:  D F A   (minor)
        assert_eq!(on(64), vec![64, 67, 71]); // E:  E G B   (minor)
        assert_eq!(on(65), vec![65, 69, 72]); // F:  F A C   (major)
        assert_eq!(on(67), vec![67, 71, 74]); // G:  G B D   (major)
        assert_eq!(on(69), vec![69, 72, 76]); // A:  A C E   (minor)
        assert_eq!(on(71), vec![71, 74, 77]); // B:  B D F   (diminished)
        assert_eq!(on(72), vec![72, 76, 79]); // C5: octave up
        assert_eq!(on(48), vec![48, 52, 55]); // C3: octave down
    }

    #[test]
    fn in_key_off_scale_notes_keep_offset() {
        let s = ScaleSetting { root: 0, scale_idx: 0 }; // C major
        // Eb4 (63) is off-scale: D + 1 semitone. Shift up one degree (C->D):
        // D becomes E, so Eb maps to E + 1 = F (65).
        assert_eq!(s.transpose_in_key(63, 60, 62), 65);
        // Off-scale trigger snaps down: C#4 trigger behaves like C4.
        assert_eq!(s.transpose_in_key(64, 60, 61), 64);
    }

    #[test]
    fn in_key_chromatic_scale_is_semitone_shift() {
        let s = ScaleSetting { root: 0, scale_idx: 14 }; // Chromatic
        assert_eq!(SCALES[14].short, "chromatic");
        for &(note, from, to) in &[(60u8, 60u8, 63u8), (64, 60, 61), (67, 62, 59)] {
            let expected = (note as i32 + to as i32 - from as i32).clamp(0, 127) as u8;
            assert_eq!(s.transpose_in_key(note, from, to), expected);
        }
    }

    #[test]
    fn the_pitch_classes_of_a_scale_are_its_notes_in_one_octave_lowest_first() {
        let classes = |text: &str| ScaleSetting::parse(text).unwrap().pitch_classes();
        assert_eq!(classes("C major"), [0, 2, 4, 5, 7, 9, 11]);
        // D dorian has the notes of C major, from another root.
        assert_eq!(classes("D dorian"), [0, 2, 4, 5, 7, 9, 11]);
        // F# dorian (F# G# A B C# D# E) goes round the octave: lowest first is not from its root.
        assert_eq!(classes("F# dorian"), [1, 3, 4, 6, 8, 9, 11]);
        assert_eq!(classes("A min pent"), [0, 2, 4, 7, 9]);
        assert_eq!(classes("G# chromatic"), (0..12).collect::<Vec<u8>>());
    }

    #[test]
    fn the_pitch_classes_are_the_notes_that_contains_says_are_in_the_scale_for_every_root_and_scale() {
        for root in 0..12u8 {
            for scale_idx in 0..SCALES.len() {
                let scale = ScaleSetting { root, scale_idx };
                let classes = scale.pitch_classes();
                assert!(classes.windows(2).all(|pair| pair[0] < pair[1]), "{}: lowest first, none twice", scale.short());
                assert!(classes.contains(&root), "{}: its root is in it", scale.short());
                for note in 0..=127u8 {
                    assert_eq!(scale.contains(note), classes.contains(&(note % 12)), "{} at {note}", scale.short());
                }
            }
        }
    }

    #[test]
    fn a_scale_is_told_by_its_root_and_its_kind_and_both_make_the_text_that_is_saved() {
        let scale = ScaleSetting::parse("Bb Natural Minor").unwrap();
        assert_eq!((scale.root_name(), scale.kind()), ("A#", "minor"));
        assert_eq!(scale.short(), "A# minor");
        for root in 0..12u8 {
            for scale_idx in 0..SCALES.len() {
                let scale = ScaleSetting { root, scale_idx };
                assert_eq!(format!("{} {}", scale.root_name(), scale.kind()), scale.short());
                let back = ScaleSetting::parse(&scale.short()).unwrap();
                assert_eq!((back.root, back.scale_idx), (root, scale_idx), "{}", scale.short());
            }
        }
    }

    #[test]
    fn in_key_pentatonic() {
        let s = ScaleSetting { root: 0, scale_idx: 9 }; // C major pentatonic
        assert_eq!(SCALES[9].short, "maj pent");
        // C D E G A — shifting C4 up one degree from trigger C->D gives D,
        // E->G (next degree), G->A, A->C5.
        assert_eq!(s.transpose_in_key(60, 60, 62), 62);
        assert_eq!(s.transpose_in_key(64, 60, 62), 67);
        assert_eq!(s.transpose_in_key(67, 60, 62), 69);
        assert_eq!(s.transpose_in_key(69, 60, 62), 72);
    }
}
