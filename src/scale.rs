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
        format!(
            "{} {}",
            NOTE_NAMES[self.root as usize],
            SCALES[self.scale_idx].short
        )
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
                if s.len() >= name.len() && s[..name.len()].eq_ignore_ascii_case(name) {
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
}
