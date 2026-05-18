//! Visual piano keyboard rendering for the Piano tab.
//!
//! Draws a multi-octave piano, highlights notes that belong to the current
//! scale, emphasizes the root, and lights up keys that are currently held.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::held_notes::HeldNotes;
use crate::scale::{ScaleSetting, NOTE_NAMES};

/// Pitch classes (semitone within octave) of white-key positions, in order.
const WHITE_OFFSETS: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];
/// Pitch classes for black keys, with the white-key index they sit *after*.
const BLACK_KEYS: [(usize, u8); 5] = [
    (0, 1),  // C#
    (1, 3),  // D#
    (3, 6),  // F#
    (4, 8),  // G#
    (5, 10), // A#
];

/// Cap on octaves shown. MIDI nominally spans octaves -1..9 (11 total) for the
/// 128 note numbers; in practice nothing outside roughly C0..C8 is musical.
const MAX_OCTAVES: i8 = 11;

/// Decide how many octaves to draw and which white-key column width to use,
/// based on the available cell width.
///
/// Strategy: maximize octaves visible (up to the full MIDI range), then pick
/// the widest white-key cell width that still fits that octave count. Width 3
/// is the practical minimum for note-name labels — narrower keys lose their
/// labels and are only used as a last resort.
fn choose_layout(width: u16) -> (i8, u8) {
    let octs_at = |w: u16| ((width / (7 * w)) as i8).min(MAX_OCTAVES);

    let o3 = octs_at(3);
    let o4 = octs_at(4);
    let o5 = octs_at(5);

    // If a width fits the full MIDI range, prefer the widest such width.
    if o5 >= MAX_OCTAVES { return (MAX_OCTAVES, 5); }
    if o4 >= MAX_OCTAVES { return (MAX_OCTAVES, 4); }
    if o3 >= MAX_OCTAVES { return (MAX_OCTAVES, 3); }
    // Otherwise, prefer width 3 with whatever octaves fit (down to 4 octaves).
    if o3 >= 4 { return (o3, 3); }
    // Very narrow: width 2.
    let o2 = octs_at(2);
    if o2 >= 4 { return (o2, 2); }
    // Tiny terminal — width 1, at least 2 octaves.
    let o1 = octs_at(1);
    if o1 >= 2 { return (o1, 1); }
    (2, 1)
}

/// Vertical-cell cap for the keyboard, scaled with white-key width so the
/// aspect ratio stays piano-like. A 1-cell-wide key shouldn't be 18 cells tall.
fn max_kb_height(white_w: u8) -> u16 {
    match white_w {
        5 => 22,
        4 => 18,
        3 => 15,
        2 => 11,
        _ => 8,
    }
}

// Color palette. The keyboard reads as a natural piano (white/black bodies)
// with one of three accent colors painted as a thin stripe at the bottom of
// the key:
//   - GOLD: root pitch class
//   - TEAL: other scale tones
//   - CORAL: held (overrides everything, fills the whole key)
const WHITE_BODY: Color = Color::Rgb(240, 240, 235);
const BLACK_BODY: Color = Color::Rgb(25, 25, 30);
/// Held in-scale, white keys: a subtle tone shift toward gray, like a piano
/// key dipping when pressed. Stripe and label color stay the same.
const WHITE_BODY_HELD: Color = Color::Rgb(205, 205, 200);
/// Held in-scale, black keys: a small lift toward gray.
const BLACK_BODY_HELD: Color = Color::Rgb(85, 85, 95);
const WHITE_TEXT: Color = Color::Rgb(80, 80, 80);
const BLACK_TEXT: Color = Color::Rgb(170, 170, 175);
const SEAM: Color = Color::Rgb(150, 150, 150);
const STRIPE_TEAL: Color = Color::Rgb(110, 195, 215);
const STRIPE_GOLD: Color = Color::Rgb(240, 185, 70);
/// Held + off-scale: pure red. "Wrong note" hint in Highlight mode. In Locked
/// mode this color never appears because off-scale notes can't be held.
const HELD_RED: Color = Color::Rgb(200, 40, 50);

/// Stripe color for a key, or None if it should stay natural (off-scale, not held).
fn stripe_color(in_scale: bool, is_root: bool) -> Option<Color> {
    if is_root {
        Some(STRIPE_GOLD)
    } else if in_scale {
        Some(STRIPE_TEAL)
    } else {
        None
    }
}

/// Draw the visual piano keyboard inside `area`.
pub fn render_keyboard(
    area: Rect,
    buf: &mut Buffer,
    held: &HeldNotes,
    scale: &ScaleSetting,
    center_octave: i8,
) {
    if area.width < 14 || area.height < 4 {
        return;
    }

    let (octaves_visible, white_w) = choose_layout(area.width);
    let octave_w = 7 * white_w as u16;
    let total_w = octave_w * octaves_visible as u16;

    // Cap the keyboard height to maintain piano-like proportions, and anchor
    // it slightly above center so the keyboard reads as a single object instead
    // of being swallowed by the area.
    let kb_h = area.height.min(max_kb_height(white_w));
    let extra = area.height.saturating_sub(kb_h);
    let kb_y0 = area.y + extra / 3;
    let kb_y_end = kb_y0 + kb_h;

    let x_offset = area.x + (area.width.saturating_sub(total_w)) / 2;
    // Anchor the view so center_octave sits in the middle when possible,
    // but clamp so the visible range stays within roughly -1..=9 (the MIDI
    // range plus a bit of slack). The render loops already filter octaves
    // outside this range.
    let max_start = (9 - octaves_visible + 1).max(-1);
    let start_octave = (center_octave - octaves_visible / 2).clamp(-1, max_start);

    // Vertical split inside kb_h:
    //   - upper zone (black keys + dimmed top of whites)
    //   - separator row (a thin "ledge" of '▔' on whites only)
    //   - lower zone (main visible part of whites)
    //   - last row: octave labels
    let label_row = kb_y_end - 1;
    let upper_rows = (kb_h - 1) * 55 / 100;
    let upper_end = kb_y0 + upper_rows;
    let lower_end = label_row;

    let black_w = match white_w {
        1 => 1u16,
        2 => 1,
        3 => 2,
        _ => (white_w as u16).saturating_sub(1),
    };
    let black_half = black_w / 2;

    // --- Pass 1: white keys ---
    for octave_offset in 0..octaves_visible {
        let octave = start_octave + octave_offset;
        if !(-1..=9).contains(&octave) {
            continue;
        }
        for (wi, &pc) in WHITE_OFFSETS.iter().enumerate() {
            let midi_signed = (octave as i16 + 1) * 12 + pc as i16;
            if !(0..=127).contains(&midi_signed) {
                continue;
            }
            let midi = midi_signed as u8;
            let in_scale = scale.contains(midi);
            let is_root = scale.is_root(midi);
            let is_held = held.is_held(midi);

            // Held + in-scale: keep the stripe, just shift the key body to a
            //   slightly darker shade ("pressed" feel).
            // Held + off-scale: full red wash — clear "wrong note" warning.
            // Not held: natural white body, optional teal/gold stripe.
            let body_bg = if is_held && !in_scale {
                HELD_RED
            } else if is_held {
                WHITE_BODY_HELD
            } else {
                WHITE_BODY
            };
            let body_fg = if is_held && !in_scale {
                Color::Black
            } else {
                WHITE_TEXT
            };
            let stripe_bg = if is_held && !in_scale {
                HELD_RED
            } else {
                stripe_color(in_scale, is_root).unwrap_or(body_bg)
            };
            let label_fg = if is_held || is_root || in_scale {
                Color::Black
            } else {
                WHITE_TEXT
            };

            let key_x0 = x_offset + (octave_offset as u16) * octave_w + (wi as u16) * (white_w as u16);
            let key_x1 = key_x0 + white_w as u16;

            // Fill the body (everything except the bottom label/stripe row).
            let body_style = Style::default().bg(body_bg).fg(body_fg);
            for y in kb_y0..(lower_end.saturating_sub(1)) {
                for x in key_x0..key_x1 {
                    if x < area.x + area.width {
                        buf[(x, y)].set_char(' ').set_style(body_style);
                    }
                }
            }

            // Stripe row at the bottom of the key (same row as the label).
            let stripe_style = Style::default().bg(stripe_bg).fg(label_fg);
            if lower_end > kb_y0 {
                for x in key_x0..key_x1 {
                    if x < area.x + area.width {
                        buf[(x, lower_end - 1)].set_char(' ').set_style(stripe_style);
                    }
                }
            }

            // Faint vertical seam at the left edge of every white key except
            // the very first one of the keyboard. This includes the B→C
            // boundary at the octave seam.
            let is_first_key = octave_offset == 0 && wi == 0;
            if !is_first_key && white_w >= 2 {
                let sep_x = key_x0;
                for y in kb_y0..lower_end {
                    if sep_x < area.x + area.width {
                        let row_bg = if y == lower_end - 1 { stripe_bg } else { body_bg };
                        let sep_style = Style::default().fg(SEAM).bg(row_bg);
                        buf[(sep_x, y)].set_char('│').set_style(sep_style);
                    }
                }
            }

            // Note-name label on the stripe row.
            if white_w >= 2 && lower_end > kb_y0 {
                let name = NOTE_NAMES[pc as usize];
                let label_w = name.len() as u16;
                if white_w as u16 >= label_w {
                    let mid_x = key_x0 + (white_w as u16 - label_w) / 2;
                    let bold = is_root || is_held;
                    let label_style = Style::default()
                        .bg(stripe_bg)
                        .fg(label_fg)
                        .add_modifier(if bold { Modifier::BOLD } else { Modifier::empty() });
                    for (i, ch) in name.chars().enumerate() {
                        let x = mid_x + i as u16;
                        if x < key_x1 && x < area.x + area.width {
                            buf[(x, lower_end - 1)].set_char(ch).set_style(label_style);
                        }
                    }
                }
            }
        }
    }

    // --- Pass 2: black keys (overlay) ---
    for octave_offset in 0..octaves_visible {
        let octave = start_octave + octave_offset;
        for (after_white_idx, pc) in BLACK_KEYS.iter().copied() {
            let midi_signed = (octave as i16 + 1) * 12 + pc as i16;
            if !(0..=127).contains(&midi_signed) {
                continue;
            }
            let midi = midi_signed as u8;
            let in_scale = scale.contains(midi);
            let is_root = scale.is_root(midi);
            let is_held = held.is_held(midi);

            let body_bg = if is_held && !in_scale {
                HELD_RED
            } else if is_held {
                BLACK_BODY_HELD
            } else {
                BLACK_BODY
            };
            let body_fg = if is_held && !in_scale {
                Color::Black
            } else {
                BLACK_TEXT
            };
            let stripe_bg = if is_held && !in_scale {
                HELD_RED
            } else {
                stripe_color(in_scale, is_root).unwrap_or(body_bg)
            };
            let body_style = Style::default().bg(body_bg).fg(body_fg);

            let boundary = x_offset + (octave_offset as u16) * octave_w
                + ((after_white_idx + 1) as u16) * (white_w as u16);
            let black_x0 = boundary.saturating_sub(black_half);
            let black_x1 = black_x0 + black_w;

            // Fill the black-key body (everything except the bottom stripe row).
            let stripe_y = upper_end.saturating_sub(1);
            for y in kb_y0..stripe_y {
                for x in black_x0..black_x1 {
                    if x < area.x + area.width {
                        buf[(x, y)].set_char(' ').set_style(body_style);
                    }
                }
            }
            // Stripe row at the bottom of the black key. Pure body color if
            // off-scale (so it stays plain black), otherwise the accent.
            let label_fg = if is_held || is_root || in_scale {
                Color::Black
            } else {
                BLACK_TEXT
            };
            let stripe_style = Style::default().bg(stripe_bg).fg(label_fg);
            for x in black_x0..black_x1 {
                if x < area.x + area.width {
                    buf[(x, stripe_y)].set_char(' ').set_style(stripe_style);
                }
            }

            // Black-key label on the stripe. Always drawn (mirrors white-key
            // behavior) — off-scale labels are light-grey on the dark body.
            if black_w >= 2 {
                let name = NOTE_NAMES[pc as usize];
                let label_w = name.len() as u16;
                if black_w >= label_w {
                    let mid_x = black_x0 + (black_w - label_w) / 2;
                    let bold = is_root || is_held;
                    let label_style = Style::default()
                        .bg(stripe_bg)
                        .fg(label_fg)
                        .add_modifier(if bold { Modifier::BOLD } else { Modifier::empty() });
                    for (i, ch) in name.chars().enumerate() {
                        let x = mid_x + i as u16;
                        if x < black_x1 && x < area.x + area.width {
                            buf[(x, stripe_y)].set_char(ch).set_style(label_style);
                        }
                    }
                }
            }
        }
    }

    // --- Pass 3: octave labels under each C ---
    for octave_offset in 0..octaves_visible {
        let octave = start_octave + octave_offset;
        let label = format!("C{octave}");
        let key_x0 = x_offset + (octave_offset as u16) * octave_w;
        let style = Style::default().fg(Color::Rgb(180, 180, 180));
        for (i, ch) in label.chars().enumerate() {
            let x = key_x0 + i as u16;
            if x < area.x + area.width && x < key_x0 + white_w as u16 {
                buf[(x, label_row)].set_char(ch).set_style(style);
            }
        }
    }
}

/// Build the list of (display, root, scale_idx) entries for the scale picker
/// popup — 12 roots × every scale type.
pub fn scale_picker_entries() -> Vec<(String, u8, usize)> {
    use crate::scale::SCALES;
    let mut out = Vec::with_capacity(12 * SCALES.len());
    for root in 0u8..12 {
        for (idx, scale) in SCALES.iter().enumerate() {
            let display = format!("{} {}", NOTE_NAMES[root as usize], scale.name);
            out.push((display, root, idx));
        }
    }
    out
}
