//! Chord detection from held notes, with scale-aware ranking.
//!
//! Given a set of held MIDI note numbers and a `ScaleSetting`, returns a list
//! of `ChordMatch`es sorted by how likely they are in context: chords whose
//! root is the scale's tonic come first, then chords whose root is in the
//! scale, then diatonic chords, then common types over rare ones.

use crate::scale::{ScaleSetting, NOTE_NAMES, SCALES};

/// One row in the chord template library: a name suffix and the intervals
/// (semitones from the root, sorted, deduplicated) that define the shape.
struct ChordTemplate {
    suffix: &'static str,
    intervals: &'static [u8],
    /// Higher = more common / preferred in ties.
    priority: u8,
    /// Bucket for Roman-numeral quality (major / minor / dim / aug / other).
    quality: ChordQuality,
    /// Suffix appended to the Roman numeral when this chord is diatonic.
    roman_suffix: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChordQuality {
    Major,
    Minor,
    Dim,
    Aug,
    Other,
}

const TEMPLATES: &[ChordTemplate] = &[
    // ---- Triads ----
    ChordTemplate { suffix: "",      intervals: &[0, 4, 7],     priority: 10, quality: ChordQuality::Major, roman_suffix: "" },
    ChordTemplate { suffix: "m",     intervals: &[0, 3, 7],     priority: 10, quality: ChordQuality::Minor, roman_suffix: "" },
    ChordTemplate { suffix: "dim",   intervals: &[0, 3, 6],     priority: 7,  quality: ChordQuality::Dim,   roman_suffix: "°" },
    ChordTemplate { suffix: "aug",   intervals: &[0, 4, 8],     priority: 6,  quality: ChordQuality::Aug,   roman_suffix: "+" },
    ChordTemplate { suffix: "sus2",  intervals: &[0, 2, 7],     priority: 6,  quality: ChordQuality::Other, roman_suffix: "sus2" },
    ChordTemplate { suffix: "sus4",  intervals: &[0, 5, 7],     priority: 6,  quality: ChordQuality::Other, roman_suffix: "sus4" },

    // ---- 7ths ----
    ChordTemplate { suffix: "maj7",  intervals: &[0, 4, 7, 11], priority: 9,  quality: ChordQuality::Major, roman_suffix: "M7" },
    ChordTemplate { suffix: "m7",    intervals: &[0, 3, 7, 10], priority: 9,  quality: ChordQuality::Minor, roman_suffix: "7" },
    ChordTemplate { suffix: "7",     intervals: &[0, 4, 7, 10], priority: 9,  quality: ChordQuality::Major, roman_suffix: "7" },
    ChordTemplate { suffix: "m7♭5", intervals: &[0, 3, 6, 10], priority: 7,  quality: ChordQuality::Dim,   roman_suffix: "ø7" },
    ChordTemplate { suffix: "dim7",  intervals: &[0, 3, 6, 9],  priority: 6,  quality: ChordQuality::Dim,   roman_suffix: "°7" },
    ChordTemplate { suffix: "mMaj7", intervals: &[0, 3, 7, 11], priority: 5,  quality: ChordQuality::Minor, roman_suffix: "(M7)" },

    // ---- 6ths ----
    ChordTemplate { suffix: "6",     intervals: &[0, 4, 7, 9],  priority: 7,  quality: ChordQuality::Major, roman_suffix: "6" },
    ChordTemplate { suffix: "m6",    intervals: &[0, 3, 7, 9],  priority: 7,  quality: ChordQuality::Minor, roman_suffix: "6" },

    // ---- Add tones ----
    ChordTemplate { suffix: "add9",  intervals: &[0, 2, 4, 7],  priority: 6,  quality: ChordQuality::Major, roman_suffix: "add9" },
    ChordTemplate { suffix: "madd9", intervals: &[0, 2, 3, 7],  priority: 5,  quality: ChordQuality::Minor, roman_suffix: "add9" },

    // ---- 9ths ----
    ChordTemplate { suffix: "9",     intervals: &[0, 2, 4, 7, 10], priority: 6, quality: ChordQuality::Major, roman_suffix: "9" },
    ChordTemplate { suffix: "maj9",  intervals: &[0, 2, 4, 7, 11], priority: 6, quality: ChordQuality::Major, roman_suffix: "M9" },
    ChordTemplate { suffix: "m9",    intervals: &[0, 2, 3, 7, 10], priority: 6, quality: ChordQuality::Minor, roman_suffix: "9" },

    // ---- 2-note ----
    ChordTemplate { suffix: "5",     intervals: &[0, 7],        priority: 4,  quality: ChordQuality::Other, roman_suffix: "5" },
];

pub struct ChordMatch {
    pub root: u8,
    pub suffix: &'static str,
    /// Roman numeral if root is in the current scale, otherwise None.
    pub roman: Option<String>,
}

impl ChordMatch {
    pub fn display(&self) -> String {
        format!("{}{}", NOTE_NAMES[self.root as usize], self.suffix)
    }
}

/// Detect chord matches from currently-held MIDI notes, ranked by scale fit.
/// Returns at most a few matches — typically 1 for an unambiguous chord, 2-3
/// when notes admit multiple interpretations. Empty if nothing musical is held.
pub fn detect(held_notes: &[u8], scale: &ScaleSetting) -> Vec<ChordMatch> {
    if held_notes.len() < 2 {
        return Vec::new();
    }

    // Collapse to unique pitch classes.
    let mut pcs: Vec<u8> = held_notes.iter().map(|&n| n % 12).collect();
    pcs.sort();
    pcs.dedup();
    if pcs.len() < 2 {
        return Vec::new();
    }

    struct Scored {
        m: ChordMatch,
        score: i32,
    }
    let mut scored: Vec<Scored> = Vec::new();

    for &root in &pcs {
        // Intervals from this root, sorted+deduplicated.
        let mut intervals: Vec<u8> = pcs
            .iter()
            .map(|&pc| ((pc as i16 - root as i16).rem_euclid(12)) as u8)
            .collect();
        intervals.sort();
        intervals.dedup();

        for tpl in TEMPLATES {
            if intervals == tpl.intervals {
                let roman = roman_numeral(root, tpl, scale);
                let diatonic = tpl
                    .intervals
                    .iter()
                    .all(|&iv| scale.contains((root as u16 + iv as u16) as u8 % 12 + 60));
                // (scale.contains uses pc internally; +60 keeps it in valid MIDI range)

                let mut score = tpl.priority as i32;
                if scale.is_root(root + 60) { score += 8; } // tonic chord — top
                if scale.contains(root + 60) { score += 4; }
                if diatonic { score += 5; }

                scored.push(Scored {
                    m: ChordMatch {
                        root,
                        suffix: tpl.suffix,
                        roman,
                    },
                    score,
                });
            }
        }
    }

    scored.sort_by_key(|s| std::cmp::Reverse(s.score));
    scored.into_iter().map(|s| s.m).collect()
}

/// For a 2-note set, return a nice "C-E (M3)" style interval label.
pub fn two_note_interval(held_notes: &[u8]) -> Option<String> {
    let mut pcs: Vec<u8> = held_notes.iter().map(|&n| n % 12).collect();
    pcs.sort();
    pcs.dedup();
    if pcs.len() != 2 {
        return None;
    }
    let mut notes: Vec<u8> = held_notes.to_vec();
    notes.sort();
    let low = notes[0];
    let high = *notes.last().unwrap();
    let semis = (high - low) % 12;
    let name = match semis {
        0 => "P1",
        1 => "m2",
        2 => "M2",
        3 => "m3",
        4 => "M3",
        5 => "P4",
        6 => "TT",
        7 => "P5",
        8 => "m6",
        9 => "M6",
        10 => "m7",
        11 => "M7",
        _ => return None,
    };
    Some(format!(
        "{}-{} ({})",
        NOTE_NAMES[(low % 12) as usize],
        NOTE_NAMES[(high % 12) as usize],
        name,
    ))
}

/// Roman numeral for a chord whose root is in the current scale. Returns None
/// if the root sits outside the scale.
fn roman_numeral(root_pc: u8, tpl: &ChordTemplate, scale: &ScaleSetting) -> Option<String> {
    let s = &SCALES[scale.scale_idx];
    let offset = (root_pc as i16 - scale.root as i16).rem_euclid(12) as u8;
    let degree_idx = s.intervals.iter().position(|&iv| iv == offset)?;
    if degree_idx >= 7 {
        return None;
    }
    const UPPER: &[&str] = &["I", "II", "III", "IV", "V", "VI", "VII"];
    const LOWER: &[&str] = &["i", "ii", "iii", "iv", "v", "vi", "vii"];
    let base = match tpl.quality {
        ChordQuality::Minor | ChordQuality::Dim => LOWER[degree_idx],
        _ => UPPER[degree_idx],
    };
    Some(format!("{}{}", base, tpl.roman_suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(n: u8) -> u8 { n }

    #[test]
    fn detects_c_major() {
        let scale = ScaleSetting::default();
        let held = vec![note(60), note(64), note(67)]; // C E G
        let matches = detect(&held, &scale);
        assert!(!matches.is_empty());
        assert_eq!(matches[0].display(), "C");
        assert_eq!(matches[0].roman.as_deref(), Some("I"));
    }

    #[test]
    fn detects_a_minor_in_c_major() {
        let scale = ScaleSetting::default(); // C major
        let held = vec![note(57), note(60), note(64)]; // A C E
        let matches = detect(&held, &scale);
        assert!(matches.iter().any(|m| m.display() == "Am"));
        let am = matches.iter().find(|m| m.display() == "Am").unwrap();
        assert_eq!(am.roman.as_deref(), Some("vi"));
    }

    #[test]
    fn detects_g7_in_c_major() {
        let scale = ScaleSetting::default();
        let held = vec![note(55), note(59), note(62), note(65)]; // G B D F
        let matches = detect(&held, &scale);
        let g7 = matches.iter().find(|m| m.display() == "G7").unwrap();
        assert_eq!(g7.roman.as_deref(), Some("V7"));
    }

    #[test]
    fn interval_two_notes() {
        let s = two_note_interval(&[60, 64]).unwrap();
        assert!(s.contains("M3"));
    }
}
