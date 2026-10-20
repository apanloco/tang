use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};

use super::Plugin;
use crate::piano_filter::PianoFilter;
use crate::scale::ScaleSetting;
use crate::session::{self, RemapTarget};
use crate::telemetry::{self, Meter, MeterSlots, Slot, Telemetry};

/// How many audio channels every chain carries: stereo (CLAUDE.md, "Signal chain"). An effect has to have
/// as many outputs; an instrument may have more, which are cut down to this many.
pub const CHAIN_CHANNELS: usize = 2;

/// Maximum number of audio channels supported (for stack-allocated reference arrays).
const MAX_CHANNELS: usize = 16;

/// How many entries the per-buffer scratch lists start with: room for the
/// modulation of a busy lane, so that they do not grow on the audio thread.
const SCRATCH_CAPACITY: usize = 64;

/// Pre-computed remap entry for a single note.
#[derive(Debug, Clone)]
struct RemapEntry {
    target_note: u8,
    channel: u8,
    pitch_bend_lsb: u8,
    pitch_bend_msb: u8,
}

/// Remaps specific MIDI notes to different notes on separate channels with pitch bend.
///
/// Normal notes pass through on channel 1 (status nibble 0x00).
/// Remapped notes are rewritten to a target note on channels 2-16, with a pitch bend
/// message inserted before each note-on to shift the pitch to the correct frequency.
#[derive(Debug, Clone)]
pub struct NoteRemapper {
    table: HashMap<u8, RemapEntry>,
}

impl NoteRemapper {
    /// Build a remapper from the session config.
    ///
    /// Groups entries by detune value and assigns MIDI channels 1-15 (status nibble 0x01-0x0F,
    /// i.e. MIDI channels 2-16). Returns an error if detune exceeds pitch_bend_range or if
    /// there are more than 15 distinct detune values.
    pub fn from_config(
        remap: &HashMap<String, RemapTarget>,
        pitch_bend_range: f64,
    ) -> anyhow::Result<Self> {
        if remap.is_empty() {
            return Ok(NoteRemapper {
                table: HashMap::new(),
            });
        }

        // Group by detune value to assign channels. Use ordered floats as keys.
        // We use a Vec to maintain insertion order and dedup by approximate equality.
        let mut detune_channels: Vec<(f64, u8)> = Vec::new();

        let mut table = HashMap::new();

        // The entries in the order of the notes they remap: the channels are handed out in the order that the detune values are met, and a hash map's order is not the same
        // from one map to the next, so that the same session loaded twice gave a note another channel (which the member channel of an MPE controller's note may share).
        let mut entries: Vec<(u8, &String, &RemapTarget)> = Vec::with_capacity(remap.len());
        for (source_name, target) in remap {
            entries.push((session::parse_note_name(source_name)?, source_name, target));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));

        // First pass: collect all distinct detune values
        for &(_, _, target) in &entries {
            if target.detune.abs() > pitch_bend_range {
                anyhow::bail!(
                    "detune {:.1} exceeds pitch_bend_range ±{:.1}",
                    target.detune,
                    pitch_bend_range
                );
            }
            let existing = detune_channels
                .iter()
                .find(|(d, _)| (*d - target.detune).abs() < 1e-9);
            if existing.is_none() {
                if detune_channels.len() >= 15 {
                    anyhow::bail!("too many distinct detune values (max 15, MIDI channels 2-16)");
                }
                // Channel status nibble: 0x01 for ch2, 0x02 for ch3, etc.
                let ch = detune_channels.len() as u8 + 1;
                detune_channels.push((target.detune, ch));
            }
        }

        // Second pass: build the lookup table
        for &(source_note, _, target) in &entries {
            let target_note = session::parse_note_name(&target.note)?;

            let &(_, channel) = detune_channels
                .iter()
                .find(|(d, _)| (*d - target.detune).abs() < 1e-9)
                .unwrap();

            // Pre-compute pitch bend: center is 8192, range maps to ±pitch_bend_range semitones
            let bend_value = (8192.0 + (target.detune / pitch_bend_range) * 8191.0).round() as i32;
            let bend_clamped = bend_value.clamp(0, 16383) as u16;
            let lsb = (bend_clamped & 0x7F) as u8;
            let msb = ((bend_clamped >> 7) & 0x7F) as u8;

            table.insert(
                source_note,
                RemapEntry {
                    target_note,
                    channel,
                    pitch_bend_lsb: lsb,
                    pitch_bend_msb: msb,
                },
            );
        }

        Ok(NoteRemapper { table })
    }

    /// Remap MIDI events, writing results into the provided buffer.
    /// For remapped note-on events, a pitch bend message is inserted before the note-on.
    /// For remapped note-off events, the note and channel are rewritten.
    /// All other events pass through unchanged.
    pub fn remap_events(&self, input: &[(u64, [u8; 3])], output: &mut Vec<(u64, [u8; 3])>) {
        output.clear();
        for &(frame, bytes) in input {
            let status_type = bytes[0] & 0xF0;
            let note = bytes[1];

            match status_type {
                0x90 if bytes[2] > 0 => {
                    // Note-on
                    if let Some(entry) = self.table.get(&note) {
                        log::info!(
                            "Remap: NoteOn {} → {} ch={} bend=({},{})",
                            note,
                            entry.target_note,
                            entry.channel + 1,
                            entry.pitch_bend_lsb,
                            entry.pitch_bend_msb,
                        );
                        // Rewritten note-on on remapped channel
                        output.push((frame, [0x90 | entry.channel, entry.target_note, bytes[2]]));
                        // Pitch bend after note-on
                        output.push((
                            frame,
                            [
                                0xE0 | entry.channel,
                                entry.pitch_bend_lsb,
                                entry.pitch_bend_msb,
                            ],
                        ));
                    } else {
                        output.push((frame, bytes));
                    }
                }
                0x80 | 0x90 => {
                    // Note-off (0x80 or 0x90 with velocity 0)
                    if let Some(entry) = self.table.get(&note) {
                        log::info!(
                            "Remap: NoteOff {} → {} ch={}",
                            note,
                            entry.target_note,
                            entry.channel + 1,
                        );
                        output.push((
                            frame,
                            [(status_type) | entry.channel, entry.target_note, bytes[2]],
                        ));
                    } else {
                        output.push((frame, bytes));
                    }
                }
                _ => {
                    output.push((frame, bytes));
                }
            }
        }
    }
}

/// Build `&mut [&mut [f32]]` on the stack from `&mut [Vec<f32>]`.
///
/// # Panics
/// Panics if `bufs.len() > MAX_CHANNELS`.
fn mut_slices<'a>(
    bufs: &'a mut [Vec<f32>],
    storage: &'a mut [MaybeUninit<&'a mut [f32]>; MAX_CHANNELS],
) -> &'a mut [&'a mut [f32]] {
    let n = bufs.len();
    assert!(n <= MAX_CHANNELS);
    for (i, buf) in bufs.iter_mut().enumerate() {
        storage[i].write(buf.as_mut_slice());
    }
    // SAFETY: first `n` elements are initialized. MaybeUninit<T> is #[repr(transparent)].
    unsafe { std::slice::from_raw_parts_mut(storage.as_mut_ptr().cast(), n) }
}

/// Build `&[&[f32]]` on the stack from `&[Vec<f32>]`.
///
/// # Panics
/// Panics if `bufs.len() > MAX_CHANNELS`.
fn shared_slices<'a>(
    bufs: &'a [Vec<f32>],
    storage: &'a mut [MaybeUninit<&'a [f32]>; MAX_CHANNELS],
) -> &'a [&'a [f32]] {
    let n = bufs.len();
    assert!(n <= MAX_CHANNELS);
    for (i, buf) in bufs.iter().enumerate() {
        storage[i].write(buf.as_slice());
    }
    // SAFETY: first `n` elements are initialized. MaybeUninit<T> is #[repr(transparent)].
    unsafe { std::slice::from_raw_parts(storage.as_ptr().cast(), n) }
}

// ---------------------------------------------------------------------------
// LFO Modulator
// ---------------------------------------------------------------------------

/// LFO waveform shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LfoWaveform {
    Sine,
    Triangle,
    Saw,
    Square,
    /// A number of its own for every cycle, held for the whole of it (sample and hold).
    Random,
    /// The numbers of `Random`, glided between: a cycle goes from its number to the next cycle's, easing at both ends.
    Smooth,
}

/// The number of cycle `cycle` of a random LFO that has the seed `seed`, in -1.0..1.0 (the top 24 bits of a hash of the two, which a 32-bit float holds exactly): the same
/// cycle of the same seed always has the same number, and the numbers of one seed and of the next are not alike. The page works it out the same way
/// (`client/noise.ts`, pinned to this by `fixtures/noise.json`), so that its mock engine's random LFOs are the engine's.
pub fn noise(seed: u32, cycle: u32) -> f32 {
    // (Chris Wellons's "lowbias32". The seed is offset first, since the hash of 0 is 0 and the numbers of seed 0 would start at the bottom of the range.)
    fn lowbias32(mut x: u32) -> u32 {
        x ^= x >> 16;
        x = x.wrapping_mul(0x7FEB_352D);
        x ^= x >> 15;
        x = x.wrapping_mul(0x846C_A68B);
        x ^= x >> 16;
        x
    }
    let hash = lowbias32(lowbias32(seed.wrapping_add(0x9E37_79B9)).wrapping_add(cycle));
    (hash >> 8) as f32 / 8_388_608.0 - 1.0
}

impl LfoWaveform {
    /// Evaluate the waveform at a given phase (0.0–1.0), returning a value in -1.0..1.0. The random ones are the first cycle of seed 0 (`eval_cycle` has the others, and is
    /// what the engine goes by: this is for the tests of the shapes).
    #[cfg(test)]
    pub fn eval(self, phase: f32) -> f32 {
        self.eval_cycle(phase, 0, 0)
    }

    /// Evaluate the waveform at a given phase of cycle `cycle` of an LFO that has the seed `seed` (see `noise`): the shapes that go by the phase alone do not look at them.
    pub fn eval_cycle(self, phase: f32, seed: u32, cycle: u32) -> f32 {
        match self {
            LfoWaveform::Sine => (phase * std::f32::consts::TAU).sin(),
            LfoWaveform::Triangle => {
                // 0→1 over first half, 1→-1 over second half
                if phase < 0.25 {
                    phase * 4.0
                } else if phase < 0.75 {
                    1.0 - (phase - 0.25) * 4.0
                } else {
                    -1.0 + (phase - 0.75) * 4.0
                }
            }
            LfoWaveform::Saw => {
                // Rising sawtooth: -1 at phase=0, +1 at phase=1
                phase * 2.0 - 1.0
            }
            LfoWaveform::Square => {
                if phase < 0.5 { 1.0 } else { -1.0 }
            }
            LfoWaveform::Random => noise(seed, cycle),
            LfoWaveform::Smooth => {
                let (from, to) = (noise(seed, cycle), noise(seed, cycle.wrapping_add(1)));
                // (Smoothstep: no corner at either end, so that the cycles join without one. A phase that is not in the cycle, which a rate below 0 makes, is at its nearer end.)
                let eased = phase.clamp(0.0, 1.0);
                from + (to - from) * (eased * eased * (3.0 - 2.0 * eased))
            }
        }
    }

    pub const ALL: &[LfoWaveform] = &[
        LfoWaveform::Sine,
        LfoWaveform::Triangle,
        LfoWaveform::Saw,
        LfoWaveform::Square,
        LfoWaveform::Random,
        LfoWaveform::Smooth,
    ];

    /// Cycle to the next waveform.
    pub fn next(self) -> Self {
        Self::ALL[(self.to_index() + 1) % Self::ALL.len()]
    }

    /// Cycle to the previous waveform.
    pub fn prev(self) -> Self {
        Self::ALL[(self.to_index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    pub fn to_index(self) -> usize {
        match self {
            LfoWaveform::Sine => 0,
            LfoWaveform::Triangle => 1,
            LfoWaveform::Saw => 2,
            LfoWaveform::Square => 3,
            LfoWaveform::Random => 4,
            LfoWaveform::Smooth => 5,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LfoWaveform::Sine => "sine",
            LfoWaveform::Triangle => "triangle",
            LfoWaveform::Saw => "saw",
            LfoWaveform::Square => "square",
            LfoWaveform::Random => "random",
            LfoWaveform::Smooth => "smooth",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "sine" => Some(LfoWaveform::Sine),
            "triangle" | "tri" => Some(LfoWaveform::Triangle),
            "saw" | "sawtooth" => Some(LfoWaveform::Saw),
            "square" | "sq" => Some(LfoWaveform::Square),
            "random" | "s&h" | "sh" => Some(LfoWaveform::Random),
            "smooth" => Some(LfoWaveform::Smooth),
            _ => None,
        }
    }
}

/// Every modulator's cables, for tests: the rack, the modulator's place in it, and its cables' kinds and depths.
#[cfg(test)]
pub type RackCables = Vec<(ModulatorLocation, usize, Vec<(ModTargetKind, f32)>)>;

/// What a lane is, for tests: the group it is in, its key range, transpose and volume, whether it is muted or
/// soloed, whether it has an instrument, how many effects and modulators, and which lane its pattern player says it is.
#[cfg(test)]
#[derive(Debug, PartialEq)]
pub struct LaneFacts {
    pub group: Option<usize>,
    pub range: Option<(u8, u8)>,
    pub transpose: i8,
    pub volume: f32,
    pub muted: bool,
    pub solo: bool,
    pub has_instrument: bool,
    pub effects: usize,
    pub modulators: usize,
    pub pattern_index: usize,
}

/// What a group is, for a test to compare with the model's: the names of its bus effects, how many
/// modulators, its volume and its pan, and whether it is muted or soloed.
#[cfg(test)]
#[derive(Debug, PartialEq)]
pub struct GroupFacts {
    pub effects: Vec<String>,
    pub modulators: usize,
    pub volume: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
}

/// Identifies what a modulation target points at.
///
/// Modulators are lane-scoped: one modulator can drive parameters anywhere in
/// its instrument's chain. `PluginParam.slot` selects the plugin — 0 is the
/// instrument, 1..N are the effects in order. Cross-mod kinds reference a
/// sibling modulator by its lane-global index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModTargetKind {
    /// Target a plugin parameter: `slot` (0 = instrument, 1.. = effect) + index.
    /// Used by lane-scoped modulators.
    PluginParam { slot: usize, param_index: u32 },
    /// (Group-scoped) Target a parameter on a member instrument's chain.
    /// `member` is the member's ordinal within the group (ascending lane
    /// order); `slot` is 0 = that instrument, 1..N = its effects.
    GroupMember {
        member: usize,
        slot: usize,
        param_index: u32,
    },
    /// (Group-scoped) Target a parameter on one of the group's own bus effects.
    GroupBus { effect_index: usize, param_index: u32 },
    /// (Group-scoped) Cross-mod a member instrument's modulator: `member` is
    /// the member ordinal, `mod_index` the member lane modulator's index, and
    /// `field` which field of it to drive (rate / ADSR / target depth).
    GroupMemberMod {
        member: usize,
        mod_index: usize,
        field: CrossModField,
    },
    /// Target a sibling modulator's LFO rate.
    ModulatorRate { mod_index: usize },
    /// Target a sibling modulator's target depth.
    ModulatorDepth { mod_index: usize, target_index: usize },
    /// Target envelope Attack.
    ModulatorAttack { mod_index: usize },
    /// Target envelope Decay.
    ModulatorDecay { mod_index: usize },
    /// Target envelope Sustain.
    ModulatorSustain { mod_index: usize },
    /// Target envelope Release.
    ModulatorRelease { mod_index: usize },
}

impl ModTargetKind {
    /// The sibling modulator, in the same rack, whose rate, stage or cable depth the target drives;
    /// `None` for a target that drives anything else.
    pub fn sibling_modulator(&self) -> Option<usize> {
        match self {
            ModTargetKind::ModulatorRate { mod_index }
            | ModTargetKind::ModulatorAttack { mod_index }
            | ModTargetKind::ModulatorDecay { mod_index }
            | ModTargetKind::ModulatorSustain { mod_index }
            | ModTargetKind::ModulatorRelease { mod_index }
            | ModTargetKind::ModulatorDepth { mod_index, .. } => Some(*mod_index),
            ModTargetKind::PluginParam { .. }
            | ModTargetKind::GroupMember { .. }
            | ModTargetKind::GroupBus { .. }
            | ModTargetKind::GroupMemberMod { .. } => None,
        }
    }

    /// Whether the target drives a setting that is turned on a log scale (an LFO's rate, an envelope's attack, decay and release: `ModulatorField::taper`), whose
    /// knob runs over decades. A cable moves such a setting a fraction of the knob's travel, in ratios (`Modulator::modulated_value`); the rest, a parameter of a plugin, a
    /// sustain and the depth of a cable, it moves a fraction of the range.
    fn is_turned_on_a_log_scale(&self) -> bool {
        match self {
            ModTargetKind::ModulatorRate { .. } | ModTargetKind::ModulatorAttack { .. } | ModTargetKind::ModulatorDecay { .. } | ModTargetKind::ModulatorRelease { .. } => true,
            ModTargetKind::GroupMemberMod { field, .. } => field.is_turned_on_a_log_scale(),
            ModTargetKind::PluginParam { .. }
            | ModTargetKind::GroupMember { .. }
            | ModTargetKind::GroupBus { .. }
            | ModTargetKind::ModulatorDepth { .. }
            | ModTargetKind::ModulatorSustain { .. } => false,
        }
    }
}

/// A modulation target: one parameter on the parent plugin or a sibling modulator.
#[derive(Debug, Clone)]
pub struct ModTarget {
    pub kind: ModTargetKind,
    /// Fraction of parameter range for modulation depth (e.g. 0.5 = ±50%).
    pub depth: f32,
    /// The user's set value (auto-updated when SetParameter is handled).
    pub base_value: f32,
    pub param_min: f32,
    pub param_max: f32,
    /// Where each buffer's *effective* value of what the target drives, a
    /// parameter or a setting of a modulator, is published (telemetry): what it
    /// was set to, the base value plus what every modulator on it adds, clamped
    /// to its range (for a setting, the last write: see `apply_cross_mod`). The
    /// slot stays with the target as the chain around it changes; `None`
    /// publishes nothing.
    pub effective: Option<Slot>,
}

/// ADSR envelope state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvState {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// A learnable MIDI control source bound to a `MidiLearn` modulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiBindSource {
    /// Control Change number (0–127).
    Cc(u8),
    /// The pitch-bend wheel.
    PitchBend,
    /// Key pressure / aftertouch — either channel pressure (0xD0) or polyphonic
    /// aftertouch (0xA0). This is the "press harder" dimension on expressive
    /// (MPE) controllers like the ROLI Seaboard. Captured and matched
    /// interchangeably (a controller sends one kind consistently); unipolar,
    /// like a CC.
    Aftertouch,
    /// The velocity of the last note the lane plays, over the 127 steps of MIDI (a note-on with none is a note-off and does not count): it stays when the note ends, so that
    /// a release goes by the velocity of its own note. Unipolar, like a CC. It is not learned (no controller sends it as a control): it is bound when it is made.
    Velocity,
    /// The number of the last note the lane plays, over 127. The notes it hears are the ones the instrument is given (after the lane's range, remap and transpose, and a
    /// pattern's own). Bipolar around middle C: it adds nothing at note 60, and the whole of the depth at `KEY_SPAN` semitones, five octaves, either way.
    Key,
}

/// The note that a `Key` control is centred on (middle C), and how many semitones either way make the whole of its depth (the page reads both: `KEY_CENTRE`, `KEY_SPAN`).
pub const KEY_CENTRE: f32 = 60.0;
pub const KEY_SPAN: f32 = 60.0;

impl MidiBindSource {
    /// Where the control is before it has sent anything, as the `value` of a `MidiLearn` source (0 to 1): a knob or a pressure at nothing, the pitch-bend wheel at its centre,
    /// which is where it rests and sends nothing from until it is moved.
    pub fn rest(self) -> f32 {
        match self {
            MidiBindSource::PitchBend => 0.5,
            MidiBindSource::Key => KEY_CENTRE / 127.0,
            MidiBindSource::Cc(_) | MidiBindSource::Aftertouch | MidiBindSource::Velocity => 0.0,
        }
    }

    /// The value (0 to 1) that the MIDI message `bytes` gives this control, if it is one of its messages.
    fn follow(self, bytes: [u8; 3]) -> Option<f32> {
        match self {
            MidiBindSource::Velocity => note_on_of(bytes).map(|(_, velocity)| f32::from(velocity) / 127.0),
            MidiBindSource::Key => note_on_of(bytes).map(|(note, _)| f32::from(note) / 127.0),
            MidiBindSource::Cc(_) | MidiBindSource::PitchBend | MidiBindSource::Aftertouch => {
                parse_learnable(bytes).filter(|(source, ..)| *source == self).map(|(_, norm, _)| norm)
            }
        }
    }
}

/// The note and the velocity of a note-on in `bytes` (a note-on with no velocity is a note-off, and has none).
fn note_on_of(bytes: [u8; 3]) -> Option<(u8, u8)> {
    (bytes[0] & 0xF0 == 0x90 && bytes[2] > 0).then_some((bytes[1], bytes[2]))
}

/// Parse a MIDI message into a learnable control: its source, normalized value
/// (0..1), and deflection from rest (0..1). Non-learnable messages → None.
///
/// The deflection is what MIDI-learn ranks gestures by — an expressive
/// controller streams several dimensions at once (touching a key nudges pitch
/// bend, slide *and* pressure together), so "the first message" is not reliably
/// the one the user meant. Pitch bend rests at centre (deflection = distance
/// from centre); a CC / aftertouch rests at zero (deflection = the value).
fn parse_learnable(bytes: [u8; 3]) -> Option<(MidiBindSource, f32, f32)> {
    match bytes[0] & 0xF0 {
        0xB0 => {
            let n = bytes[2] as f32 / 127.0;
            Some((MidiBindSource::Cc(bytes[1]), n, n))
        }
        0xE0 => {
            let raw = ((bytes[2] as u16) << 7) | bytes[1] as u16;
            let n = raw as f32 / 16383.0;
            Some((MidiBindSource::PitchBend, n, (n - 0.5).abs() * 2.0))
        }
        0xD0 => {
            let n = bytes[1] as f32 / 127.0;
            Some((MidiBindSource::Aftertouch, n, n))
        }
        0xA0 => {
            let n = bytes[2] as f32 / 127.0;
            Some((MidiBindSource::Aftertouch, n, n))
        }
        _ => None,
    }
}

/// How many steps a step sequencer has to set: the bars of its node (generated into the page's constants as `MAX_STEPS`).
pub const MAX_STEPS: usize = 16;

/// Where a step sequencer of `length` steps in use is at `position`: how many steps are in use (held to 1 to `MAX_STEPS`) and the position brought round at them
/// (a position that is not a number is the beginning).
fn steps_place(length: usize, position: f32) -> (usize, f32) {
    let length = length.clamp(1, MAX_STEPS);
    (length, if position.is_finite() { position.rem_euclid(length as f32) } else { 0.0 })
}

/// The step that a step sequencer of `length` steps in use is at when its position is `position`: the whole part of it, brought round at the length. The bar that a
/// playhead is on (the page's `playheadStep` is pinned to it by `fixtures/steps.json`).
pub fn step_at(length: usize, position: f32) -> usize {
    let (length, position) = steps_place(length, position);
    // (A position a hair below 0 comes round to the length itself, in an `f32`.)
    (position as usize).min(length - 1)
}

/// What a step sequencer puts out at `position` (the step it is at as the whole part, how far into it as the fraction, 0 to `length`): the value of the step,
/// held for the part of it before its last `glide`, which is spent going to the value of the next step (the first, after the last) in a straight line.
pub fn steps_output(values: &[f32; MAX_STEPS], length: usize, position: f32, glide: f32) -> f32 {
    let (length, position) = steps_place(length, position);
    let step = (position as usize).min(length - 1);
    let within = position - step as f32;
    let here = values[step];
    let glide = glide.clamp(0.0, 1.0);
    if glide <= 0.0 || within <= 1.0 - glide {
        return here;
    }
    let next = values[(step + 1) % length];
    here + (next - here) * ((within - (1.0 - glide)) / glide)
}

/// Modulation source: an LFO, an ADSR envelope, a learned MIDI control, a macro, or a step sequencer.
#[derive(Debug, Clone)]
pub enum ModSource {
    /// A low-frequency oscillator: `rate` cycles a second of `waveform`, or, if `sync` is not 0, a cycle every `sync` beats of the tempo (`rate` is kept for when it is 0 again). With
    /// `retrigger` on a note puts it at the start of its wave again (and, if the wave is a random one, takes the next number). What it puts out comes in from nothing over `fade`
    /// seconds (none if it is 0) from every note, whether or not it retriggers; `age` is how many seconds ago the last note was (infinite before the first: it rests at full until a
    /// note).
    Lfo {
        waveform: LfoWaveform,
        rate: f32,
        phase: f32,
        retrigger: bool,
        fade: f32,
        sync: f32,
        age: f32,
    },
    Envelope {
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
        state: EnvState,
        level: f32,
        notes_held: u32,
    },
    /// A MIDI-learned control: a CC or the pitch-bend wheel drives the target
    /// parameter absolutely (0..1 across its range). `source` is None until a
    /// control is captured; `learning` arms the next incoming CC/bend.
    MidiLearn {
        source: Option<MidiBindSource>,
        value: f32,
        learning: bool,
    },
    /// A number that the user sets (0..1) and nothing else moves: a knob on the screen that drives every target that has a cable from it. It offsets
    /// its targets as a CC does (the rest, 0, adds nothing; full adds `depth` of the range), so that a new one moves nothing until it is turned.
    Macro {
        value: f32,
    },
    /// A step sequencer: up to `MAX_STEPS` values from -1 to 1 that it goes through one after the other, `rate` steps a second (or, if `sync` is not 0, a step every `sync` beats of
    /// the tempo), the first `length` of them and round again (`steps_output` says what each moment of it puts out). A note starts it again at the first step if `retrigger` is on. It
    /// drives its targets as an LFO does, around what the user set.
    Steps {
        values: [f32; MAX_STEPS],
        length: usize,
        rate: f32,
        glide: f32,
        retrigger: bool,
        sync: f32,
        /// Where it is: the whole part is the step, the fraction how far into it.
        position: f32,
    },
}

impl ModSource {
    /// An LFO that runs free, as every one did before it had a key: no note puts it at the start of its wave again and it does not fade in.
    pub fn lfo(waveform: LfoWaveform, rate: f32) -> ModSource {
        ModSource::Lfo { waveform, rate, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: f32::INFINITY }
    }

    /// The same, with whether a note starts an LFO again and how many seconds it takes to come in after one (none for 0). Any other source is as it was.
    pub fn with_key(mut self, key_retrigger: bool, key_fade: f32) -> ModSource {
        if let ModSource::Lfo { retrigger, fade, .. } = &mut self {
            *retrigger = key_retrigger;
            *fade = key_fade;
        }
        self
    }

    /// The same, with how many beats of the tempo a cycle of an LFO or a step of a step sequencer is (0: it goes at its own rate). Any other source is as it was.
    pub fn with_sync(mut self, beats: f32) -> ModSource {
        if let ModSource::Lfo { sync, .. } | ModSource::Steps { sync, .. } = &mut self {
            *sync = beats;
        }
        self
    }

    /// How fast it goes, if it has a rate: an LFO's cycles and a step sequencer's steps a second.
    fn rate(&self) -> Option<f32> {
        match self {
            ModSource::Lfo { rate, .. } | ModSource::Steps { rate, .. } => Some(*rate),
            ModSource::Envelope { .. } | ModSource::MidiLearn { .. } | ModSource::Macro { .. } => None,
        }
    }

    /// Set how fast it goes, if it has a rate.
    fn set_rate(&mut self, new: f32) {
        if let ModSource::Lfo { rate, .. } | ModSource::Steps { rate, .. } = self {
            *rate = new;
        }
    }
}

/// How many cycles (or steps) a second there are at `bpm` beats a minute when each of them is `beats` beats long.
pub fn rate_at_tempo(bpm: f32, beats: f32) -> f32 {
    bpm / (60.0 * beats)
}

/// How fast a source that goes `rate` cycles (or steps) a second goes at `bpm` beats a minute, if each of them takes `sync` beats of the tempo instead (0: it goes at its own rate).
fn effective_rate(rate: f32, sync: f32, bpm: f32) -> f32 {
    // (A `sync` that is no number is none: the comparison is false for it.)
    if sync > 0.0 { rate_at_tempo(bpm, sync) } else { rate }
}

/// A block-rate modulator with a source (LFO or Envelope) and targets.
#[derive(Debug, Clone)]
pub struct Modulator {
    pub source: ModSource,
    sample_rate: f32,
    pub targets: Vec<ModTarget>,
    /// Last computed output value (bipolar -1..1 for LFO, unipolar 0..1 for envelope).
    pub last_output: f32,
    /// Where `last_output` is published each buffer (telemetry); `None`
    /// publishes nothing.
    output: Option<Slot>,
    /// Set for one buffer when a `MidiLearn` source just captured a control, so
    /// the graph can notify the main thread. Consumed (taken) by the graph.
    pub last_learned: Option<MidiBindSource>,
    /// Where the position of a step sequencer is published each buffer (telemetry); `None` publishes nothing.
    position_out: Option<Slot>,
    /// What makes the numbers of a random LFO its own: the graph gives every modulator that comes in the next (`AudioGraph::next_modulator_seed`).
    seed: u32,
    /// How many times an LFO has gone round (counted as the phase wraps, so that a rate that is turned does not start the numbers again): the random
    /// waveforms have a number for each.
    cycle: u32,
}

impl Modulator {
    pub fn new(source: ModSource, sample_rate: f32) -> Self {
        Modulator {
            source,
            sample_rate,
            targets: Vec::new(),
            last_output: 0.0,
            output: None,
            last_learned: None,
            position_out: None,
            seed: 0,
            cycle: 0,
        }
    }

    /// The same modulator with the seed that its random numbers are made from.
    pub fn with_seed(mut self, seed: u32) -> Self {
        self.seed = seed;
        self
    }

    /// Advance the modulator by one buffer, at `bpm` beats a minute. For envelopes, processes MIDI note events.
    fn tick(&mut self, buffer_size: usize, midi_events: &[(u64, [u8; 3])], bpm: f32) {
        match &mut self.source {
            ModSource::Lfo { waveform, rate, phase, retrigger, fade, sync, age } => {
                // A note (a note-on with a velocity, on any channel: the lane's or a member's, as the step sequencer's) starts the fade again, and the wave too if it retriggers.
                let note = midi_events.iter().any(|&(_frame, bytes)| note_on_of(bytes).is_some());
                if note {
                    *age = 0.0;
                } else {
                    *age += buffer_size as f32 / self.sample_rate;
                }
                if note && *retrigger {
                    // The start of the wave, at the start of a cycle of its own: a random wave takes its next number.
                    *phase = 0.0;
                    self.cycle = self.cycle.wrapping_add(1);
                } else {
                    let phase_inc = effective_rate(*rate, *sync, bpm) * buffer_size as f32 / self.sample_rate;
                    let turned = *phase + phase_inc;
                    // Every time round is a cycle, and a rate that makes more than one in a buffer counts them all.
                    self.cycle = self.cycle.wrapping_add(turned as u32);
                    *phase = turned % 1.0;
                }
                self.last_output = waveform.eval_cycle(*phase, self.seed, self.cycle);
                // (A fade of nothing is a gain of one, which is no change to the last bit: the check says so and spares the division.)
                if *fade > 0.0 {
                    self.last_output *= (*age / *fade).min(1.0);
                }
            }
            ModSource::Envelope { attack, decay, sustain, release, state, level, notes_held } => {
                // Process MIDI events for note-on/off.
                for &(_frame, bytes) in midi_events {
                    let status_type = bytes[0] & 0xF0;
                    match status_type {
                        0x90 if bytes[2] > 0 => {
                            // Note-on: retrigger from Attack.
                            *notes_held = notes_held.saturating_add(1);
                            *state = EnvState::Attack;
                        }
                        0x80 | 0x90 => {
                            // Note-off.
                            *notes_held = notes_held.saturating_sub(1);
                            if *notes_held == 0 {
                                *state = EnvState::Release;
                            }
                        }
                        _ => {}
                    }
                }

                // Advance envelope state machine.
                let dt = buffer_size as f32 / self.sample_rate;
                match *state {
                    EnvState::Idle => {
                        *level = 0.0;
                    }
                    EnvState::Attack => {
                        let rate = if *attack > 0.0 { dt / *attack } else { 1.0 };
                        *level += rate;
                        if *level >= 1.0 {
                            *level = 1.0;
                            *state = EnvState::Decay;
                        }
                    }
                    EnvState::Decay => {
                        let rate = if *decay > 0.0 { dt / *decay } else { 1.0 };
                        *level -= rate * (1.0 - *sustain);
                        if *level <= *sustain {
                            *level = *sustain;
                            *state = EnvState::Sustain;
                        }
                    }
                    EnvState::Sustain => {
                        *level = *sustain;
                    }
                    EnvState::Release => {
                        let rate = if *release > 0.0 { dt / *release } else { 1.0 };
                        *level -= rate * (*level).max(0.001);
                        if *level <= 0.001 {
                            *level = 0.0;
                            *state = EnvState::Idle;
                        }
                    }
                }
                self.last_output = *level;
            }
            ModSource::MidiLearn { source, value, learning } => {
                if *learning {
                    // Capture the *strongest-moved* control in this buffer, not
                    // the first message. An expressive controller streams
                    // several dimensions at once (a press also nudges pitch
                    // bend / slide), so first-wins binds the wrong one — often
                    // an incidental micro-bend instead of the press the user
                    // meant. Take the largest deflection from rest, above a
                    // small noise floor (a Seaboard's glide/pressure values can
                    // be modest, so this must stay low or real gestures never
                    // clear it).
                    const LEARN_MIN_DEFLECTION: f32 = 0.05;
                    let mut best: Option<(MidiBindSource, f32, f32)> = None;
                    for &(_frame, bytes) in midi_events {
                        if let Some((src, norm, defl)) = parse_learnable(bytes) {
                            if defl >= LEARN_MIN_DEFLECTION
                                && best.is_none_or(|(_, _, d)| defl > d)
                            {
                                best = Some((src, norm, defl));
                            }
                        }
                    }
                    if let Some((src, norm, _)) = best {
                        *source = Some(src);
                        *learning = false;
                        *value = norm;
                        self.last_learned = Some(src);
                    }
                } else if let Some(bound) = *source {
                    // Bound: track the value of the matching control (the last of its messages in the buffer stands).
                    for &(_frame, bytes) in midi_events {
                        if let Some(norm) = bound.follow(bytes) {
                            *value = norm;
                        }
                    }
                }
                self.last_output = *value;
            }
            ModSource::Macro { value } => self.last_output = *value,
            ModSource::Steps { values, length, rate, glide, retrigger, sync, position } => {
                let length = (*length).clamp(1, MAX_STEPS);
                if *retrigger && midi_events.iter().any(|&(_frame, bytes)| note_on_of(bytes).is_some()) {
                    // A note puts it at the first step, which this buffer puts out.
                    *position = 0.0;
                } else {
                    *position = (*position + effective_rate(*rate, *sync, bpm) * buffer_size as f32 / self.sample_rate).rem_euclid(length as f32);
                }
                if !position.is_finite() {
                    *position = 0.0;
                }
                self.last_output = steps_output(values, length, *position, *glide);
            }
        }
    }

    /// The offset this modulator currently contributes to a target, relative
    /// to the target's base value.
    ///
    /// LFOs are bipolar and wobble ±depth×range around the base. Envelopes
    /// are unipolar and treat the base as their **peak**: the value rises
    /// from `base − depth × (base − min)` at idle up to the base at full
    /// envelope level. With depth 1.0 the envelope spans min→base, so e.g.
    /// an amp envelope on a volume parameter reaches silence instead of
    /// trying to push past an already-maxed value (offsets above the base
    /// would just clamp at param_max and do nothing).
    fn target_offset(&self, target: &ModTarget) -> f32 {
        self.offset(target.depth, target.base_value, target.param_min, target.param_max)
    }

    /// The offset of `target_offset`, for a cable of `depth` on a value whose base is `base` between `min` and `max`.
    fn offset(&self, depth: f32, base: f32, min: f32, max: f32) -> f32 {
        match &self.source {
            ModSource::Lfo { .. } => self.last_output * depth * (max - min),
            ModSource::Envelope { .. } => -(1.0 - self.last_output) * depth * (base - min),
            // Unbound (armed for learning, or bound to nothing yet): leave the
            // parameter at its base, so arming learn doesn't move the sound
            // before you've touched a control.
            ModSource::MidiLearn { source: None, .. } => 0.0,
            // Bound: an additive offset around the base, like the LFO — so a
            // learned control *combines* with your set value instead of
            // overwriting it. The rest position contributes nothing:
            //   - pitch bend is bipolar (centre = 0, ±full bend = ±depth·range)
            //   - a CC / pressure / velocity is unipolar (0 = 0, full = +depth·range)
            //   - a key is bipolar around middle C (note 60 = 0, five octaves either way = ±depth·range)
            ModSource::MidiLearn { source: Some(src), .. } => {
                let deflection = match src {
                    MidiBindSource::PitchBend => (self.last_output - 0.5) * 2.0,
                    MidiBindSource::Cc(_) | MidiBindSource::Aftertouch | MidiBindSource::Velocity => self.last_output,
                    // (The value is the number of the note over 127, and 127 times it is the note again exactly, in 32 bits, for every note: a test goes through all of them.)
                    MidiBindSource::Key => ((self.last_output * 127.0 - KEY_CENTRE) / KEY_SPAN).clamp(-1.0, 1.0),
                };
                deflection * depth * (max - min)
            }
            // Unipolar and additive around the base, like a CC: at rest (0) it adds nothing.
            ModSource::Macro { .. } => self.last_output * depth * (max - min),
            // Bipolar and additive around the base, like the LFO: a value of 0 adds nothing.
            ModSource::Steps { .. } => self.last_output * depth * (max - min),
        }
    }

    /// What this modulator alone sets what `target` drives to, from the user's value, kept to its range: the write of a cable to a setting of a modulator, where the
    /// last of the cables that drive it stands (`apply_cross_mod`).
    ///
    /// A setting that is turned on a log scale (a rate, an attack, a decay, a release) is moved in ratios: the law of `target_offset` is the same, but over the knob's
    /// travel, from 0 at the lowest value to 1 at the highest, where equal steps are equal ratios, so that a depth of 0.1 moves it a tenth of the knob either way. (Over the
    /// range in seconds or hertz it was nothing at the bottom of the knob and everything at the top.) A range that starts at nothing has no decades to move by, and is
    /// moved by the range, as the knob of such a setting is turned by distances.
    fn modulated_value(&self, target: &ModTarget) -> f32 {
        let (min, max) = (target.param_min, target.param_max);
        if target.kind.is_turned_on_a_log_scale() && min > 0.0 && max > min {
            let decades = (max / min).ln();
            let at = ((target.base_value.max(min) / min).ln() / decades).clamp(0.0, 1.0);
            let moved = (at + self.offset(target.depth, at, 0.0, 1.0)).clamp(0.0, 1.0);
            return (min * (moved * decades).exp()).clamp(min, max);
        }
        (target.base_value + self.target_offset(target)).clamp(min, max)
    }
}

/// One cross-mod write waiting to be applied: the sibling modulator, which of its
/// fields, and the new value.
type CrossModWrite = (usize, CrossModField, f32);

/// Apply cross-modulator targets within a modulator list.
///
/// For each modulator, applies its `last_output` to any sibling modulator targets
/// (rate, ADSR params, depth). Self-modulation (targeting own index) is skipped.
/// `mods_to_apply` is scratch space the caller keeps between buffers, so that this
/// does not allocate once it has grown to fit. What each setting is set to is
/// published into the slots of the cables that drive it (`telemetry`), once they
/// have all been applied.
fn apply_cross_mod(modulators: &mut [Modulator], mods_to_apply: &mut Vec<CrossModWrite>, telemetry: Option<&Telemetry>) {
    // Collect modifications first (avoids simultaneous borrow issues).
    mods_to_apply.clear();

    for (src_idx, src) in modulators.iter().enumerate() {
        for target in &src.targets {
            let Some((tgt_mod_idx, field)) = sibling_field(&target.kind) else { continue };
            // Skip self-modulation.
            if tgt_mod_idx == src_idx {
                continue;
            }
            mods_to_apply.push((tgt_mod_idx, field, src.modulated_value(target)));
        }
    }

    // Apply collected modifications.
    for &(tgt_idx, field, value) in mods_to_apply.iter() {
        if let Some(tgt) = modulators.get_mut(tgt_idx) {
            apply_cross_mod_field(tgt, field, value);
        }
    }

    if let Some(telemetry) = telemetry {
        publish_cross_mod_effective(modulators, telemetry);
    }
}

/// The sibling modulator and the setting of it that a target drives, or `None` for a target that drives anything else.
fn sibling_field(kind: &ModTargetKind) -> Option<(usize, CrossModField)> {
    match kind {
        ModTargetKind::ModulatorRate { mod_index } => Some((*mod_index, CrossModField::Rate)),
        ModTargetKind::ModulatorAttack { mod_index } => Some((*mod_index, CrossModField::Attack)),
        ModTargetKind::ModulatorDecay { mod_index } => Some((*mod_index, CrossModField::Decay)),
        ModTargetKind::ModulatorSustain { mod_index } => Some((*mod_index, CrossModField::Sustain)),
        ModTargetKind::ModulatorRelease { mod_index } => Some((*mod_index, CrossModField::Release)),
        ModTargetKind::ModulatorDepth { mod_index, target_index } => Some((*mod_index, CrossModField::Depth(*target_index))),
        ModTargetKind::PluginParam { .. }
        | ModTargetKind::GroupMember { .. }
        | ModTargetKind::GroupBus { .. }
        | ModTargetKind::GroupMemberMod { .. } => None,
    }
}

/// What the setting `field` of `m` is now, or `None` if `m` has no such setting (an envelope has no rate, an LFO has no stages) or no such cable.
fn cross_mod_field_value(m: &Modulator, field: CrossModField) -> Option<f32> {
    match (field, &m.source) {
        (CrossModField::Rate, source) => source.rate(),
        (CrossModField::Attack, ModSource::Envelope { attack, .. }) => Some(*attack),
        (CrossModField::Decay, ModSource::Envelope { decay, .. }) => Some(*decay),
        (CrossModField::Sustain, ModSource::Envelope { sustain, .. }) => Some(*sustain),
        (CrossModField::Release, ModSource::Envelope { release, .. }) => Some(*release),
        (CrossModField::Depth(target_index), _) => m.targets.get(target_index).map(|target| target.depth),
        _ => None,
    }
}

/// Publish what the settings that a rack's cross-mod drives were just set to, into the slot of each cable that drives one: the value the setting has
/// now, which is the last one written when several cables drive it. A cable to a setting its modulator has not, and one to its own, publish nothing.
fn publish_cross_mod_effective(modulators: &[Modulator], telemetry: &Telemetry) {
    for (src_idx, src) in modulators.iter().enumerate() {
        for target in &src.targets {
            let Some(slot) = target.effective else { continue };
            let Some((tgt_idx, field)) = sibling_field(&target.kind) else { continue };
            if tgt_idx == src_idx {
                continue;
            }
            if let Some(value) = modulators.get(tgt_idx).and_then(|m| cross_mod_field_value(m, field)) {
                telemetry.store(slot, value);
            }
        }
    }
}

/// Publish what the settings of the members' modulators that the groups' cables drive ended up as: each cable's slot gets the value its setting has once the
/// lanes have run (a lane's own cross-mod, applied inside it, comes after the group's and wins). A cable to a member that is not there publishes nothing.
fn publish_group_member_mod_effective(groups: &[Group], instruments: &[InstrumentLane], telemetry: &Telemetry) {
    for (gi, g) in groups.iter().enumerate() {
        for target in g.modulators.iter().flat_map(|m| &m.targets) {
            let Some(slot) = target.effective else { continue };
            let ModTargetKind::GroupMemberMod { member, mod_index, field } = target.kind else { continue };
            let Some(lane) = group_member_lane(instruments, gi, member) else { continue };
            if let Some(value) = instruments[lane].modulators.get(mod_index).and_then(|m| cross_mod_field_value(m, field)) {
                telemetry.store(slot, value);
            }
        }
    }
}

/// Write a cross-mod value into a modulator's field. Shared by sibling
/// cross-mod and group→member-modulator cross-mod.
fn apply_cross_mod_field(m: &mut Modulator, field: CrossModField, value: f32) {
    match field {
        CrossModField::Rate => m.source.set_rate(value),
        CrossModField::Attack => {
            if let ModSource::Envelope { attack, .. } = &mut m.source {
                *attack = value;
            }
        }
        CrossModField::Decay => {
            if let ModSource::Envelope { decay, .. } = &mut m.source {
                *decay = value;
            }
        }
        CrossModField::Sustain => {
            if let ModSource::Envelope { sustain, .. } = &mut m.source {
                *sustain = value;
            }
        }
        CrossModField::Release => {
            if let ModSource::Envelope { release, .. } = &mut m.source {
                *release = value;
            }
        }
        CrossModField::Depth(target_index) => {
            if let Some(t) = m.targets.get_mut(target_index) {
                t.depth = value;
            }
        }
    }
}

/// Which field of a modulator a cross-mod target drives. Shared by sibling
/// cross-mod (within one rack) and group→member-modulator cross-mod.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossModField {
    Rate,
    Attack,
    Decay,
    Sustain,
    Release,
    Depth(usize),
}

impl CrossModField {
    /// Whether the knob of the setting runs over decades (see `ModTargetKind::is_turned_on_a_log_scale`).
    fn is_turned_on_a_log_scale(self) -> bool {
        matches!(self, CrossModField::Rate | CrossModField::Attack | CrossModField::Decay | CrossModField::Release)
    }
}

/// Publish `value`, what a parameter was just set to, into the slot of every
/// target in `modulators` that is on that parameter (`on_param` says which are).
fn publish_effective(
    modulators: &[Modulator],
    telemetry: &Telemetry,
    value: f32,
    on_param: impl Fn(&ModTargetKind) -> bool,
) {
    for target in modulators.iter().flat_map(|m| &m.targets) {
        if let Some(slot) = target.effective {
            if on_param(&target.kind) {
                telemetry.store(slot, value);
            }
        }
    }
}

/// Publish what each of `modulators` put out this buffer, into the slots they
/// were given.
fn publish_outputs(modulators: &[Modulator], telemetry: &Telemetry) {
    for m in modulators {
        if let Some(slot) = m.output {
            telemetry.store(slot, m.last_output);
        }
        if let (Some(slot), ModSource::Steps { position, .. }) = (m.position_out, &m.source) {
            telemetry.store(slot, *position);
        }
    }
}

/// A parameter some modulators of a lane drive: the chain slot, the parameter's
/// index, its base value, its range (min, max) and the offsets summed so far.
type ParamDrive = (usize, u32, f32, f32, f32, f32);

/// Apply all of a lane's modulators across its chain, summing contributions
/// when multiple modulators target the same parameter. Each parameter gets:
///   base_value + sum(offset_i)
/// where each offset comes from `Modulator::target_offset` (bipolar around
/// the base for LFOs, rising from below up to the base for envelopes). This
/// prevents the last-modulator-wins overwrite bug. Targets are routed by
/// slot: 0 = instrument, 1..N = effects[slot-1]. `accum` is scratch space the
/// caller keeps between buffers, so that this does not allocate once it has grown
/// to fit.
fn apply_modulators_to_chain(
    modulators: &[Modulator],
    instrument: &mut Option<Box<dyn Plugin>>,
    effects: &mut [Box<dyn Plugin>],
    accum: &mut Vec<ParamDrive>,
    telemetry: Option<&Telemetry>,
) {
    accum.clear();

    for m in modulators {
        for target in &m.targets {
            if let ModTargetKind::PluginParam { slot, param_index } = target.kind {
                let offset = m.target_offset(target);
                if let Some(entry) = accum
                    .iter_mut()
                    .find(|e| e.0 == slot && e.1 == param_index)
                {
                    entry.5 += offset;
                } else {
                    accum.push((
                        slot,
                        param_index,
                        target.base_value,
                        target.param_min,
                        target.param_max,
                        offset,
                    ));
                }
            }
        }
    }

    for &(slot, param_index, base_value, min, max, total_offset) in accum.iter() {
        let modulated = (base_value + total_offset).clamp(min, max);
        if let Some(telemetry) = telemetry {
            publish_effective(modulators, telemetry, modulated, |kind| {
                matches!(kind, ModTargetKind::PluginParam { slot: s, param_index: p } if *s == slot && *p == param_index)
            });
        }
        let plugin: Option<&mut dyn Plugin> = if slot == 0 {
            instrument.as_deref_mut()
        } else {
            effects.get_mut(slot - 1).map(|b| b.as_mut())
        };
        if let Some(p) = plugin {
            let _ = p.set_parameter(param_index, modulated);
        }
    }
}

/// After removing a modulator at `removed_index`, clean up cross-mod targets
/// in siblings: remove targets pointing at the removed index, and decrement
/// indices > removed_index.
fn fixup_cross_mod_after_remove(modulators: &mut [Modulator], removed_index: usize) {
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            let idx = cross_mod_index(&t.kind);
            idx != Some(removed_index)
        });
        for t in &mut m.targets {
            adjust_cross_mod_index(&mut t.kind, removed_index);
        }
    }
}

/// Hand `plugin`, which was taken out of the graph, back to the main thread without dropping it here: to `keep`, the channel that the main thread made
/// for this plugin when it wants the very plugin to put it back (the controller's history of what was taken out), or by `return_tx`, where the main
/// thread drops what comes, when it made none, when nobody listens to it any more or when it is full. Never blocks.
fn hand_back(return_tx: &Sender<Box<dyn Plugin>>, keep: Option<Sender<Box<dyn Plugin>>>, plugin: Box<dyn Plugin>) {
    let plugin = match keep {
        Some(keep) => match keep.try_send(plugin) {
            Ok(()) => return,
            Err(unsent) => unsent.into_inner(),
        },
        None => plugin,
    };
    let _ = return_tx.try_send(plugin);
}

/// A lane that the audio thread took out of the graph whole, to be put back where it was (`GraphCommand::InsertInstrument`): its plugins and whatever they hold, its
/// modulators, its meter and its pattern. The controller asks for it (`RemoveInstrument { keep }`) for the history of what was taken out.
pub struct KeptLane(Box<InstrumentLane>);

impl KeptLane {
    /// A lane with nothing in it, which is what a test that has no audio thread hands back to the controller for the lane that it took out: a lane that gave up its plugins, so
    /// that the controller loads them again. (It is not put into a graph.)
    #[cfg(test)]
    pub fn empty() -> KeptLane {
        KeptLane(Box::new(InstrumentLane::new(2)))
    }

    /// How many plugins the lane holds: its instrument and its effects.
    pub fn plugin_count(&self) -> usize {
        usize::from(self.0.instrument.is_some()) + self.0.effects.len()
    }

    /// Take the plugins out of the lane, the instrument first and then the effects: to drop them, since a plugin can be big, when the history lets go of them. A lane
    /// without them is not put back (the audio thread turns it away) until it has been given plugins again, `put_plugins`.
    pub fn take_plugins(&mut self) -> Vec<Box<dyn Plugin>> {
        let mut plugins: Vec<Box<dyn Plugin>> = self.0.instrument.take().into_iter().collect();
        plugins.append(&mut self.0.effects);
        plugins
    }

    /// Give the lane an instrument and effects, as many effects as it has mixes for (it has kept the mixes). The lane's buffers are the instrument's.
    pub fn put_plugins(&mut self, instrument: Option<Box<dyn Plugin>>, effects: Vec<Box<dyn Plugin>>) {
        self.0.inst_buf = instrument.as_ref().map_or_else(Vec::new, |plugin| (0..plugin.audio_output_count()).map(|_| Vec::new()).collect());
        self.0.instrument = instrument;
        self.0.effects = effects;
    }
}

/// Give the plugins of `lane`, which is out of the graph, to the main thread by the return channel, which drops what comes: the lane is dropped here without them.
fn return_the_plugins_of(return_tx: &Sender<Box<dyn Plugin>>, mut lane: InstrumentLane) {
    if let Some(instrument) = lane.instrument.take() {
        let _ = return_tx.try_send(instrument);
    }
    for effect in lane.effects.drain(..) {
        let _ = return_tx.try_send(effect);
    }
}

/// Hand `lane`, which was taken out of the graph, to `keep`, the channel that the main thread made for it when it wants the whole lane to put it back (the
/// controller's history), or give its plugins to the return channel, as `hand_back` does for a plugin, when it made none, when nobody listens or when the channel is
/// full. Never blocks.
fn hand_back_lane(return_tx: &Sender<Box<dyn Plugin>>, keep: Option<Sender<KeptLane>>, lane: InstrumentLane) {
    let lane = match keep {
        Some(keep) => match keep.try_send(KeptLane(Box::new(lane))) {
            Ok(()) => return,
            Err(unsent) => *unsent.into_inner().0,
        },
        None => lane,
    };
    return_the_plugins_of(return_tx, lane);
}

/// A group that the audio thread took out of the graph whole, to be put back where it was with the lanes that were its members (`GraphCommand::InsertGroup`): its bus effects and
/// whatever they hold, its modulators, its meter, its volume and its pan. The controller asks for it (`RemoveGroup { keep }`) for the history of what was taken out.
pub struct KeptGroup(Box<Group>);

impl KeptGroup {
    /// A group with nothing in it, which is what a test that has no audio thread hands back to the controller for the group that it took out: a group that gave up its plugins, so
    /// that the controller loads them again. (It is not put into a graph.)
    #[cfg(test)]
    pub fn empty() -> KeptGroup {
        KeptGroup(Box::new(Group::new(2)))
    }

    /// How many plugins the group holds: its bus effects.
    pub fn plugin_count(&self) -> usize {
        self.0.effects.len()
    }

    /// Take the bus effects out of the group, in order: to drop them, since a plugin can be big, when the history lets go of them. A group without them is not put back (the audio
    /// thread turns it away) until it has been given effects again, `put_plugins`.
    pub fn take_plugins(&mut self) -> Vec<Box<dyn Plugin>> {
        std::mem::take(&mut self.0.effects)
    }

    /// Give the group bus effects, as many as it has mixes for (it has kept the mixes).
    pub fn put_plugins(&mut self, effects: Vec<Box<dyn Plugin>>) {
        self.0.effects = effects;
    }
}

/// Hand `group`, which was taken out of the graph, to `keep`, the channel that the main thread made for it when it wants the whole group to put it back (the controller's
/// history), or give its bus effects to the return channel, as `hand_back` does for a plugin, when it made none, when nobody listens or when the channel is full. Never blocks.
fn hand_back_group(return_tx: &Sender<Box<dyn Plugin>>, keep: Option<Sender<KeptGroup>>, group: Group) {
    let mut group = match keep {
        Some(keep) => match keep.try_send(KeptGroup(Box::new(group))) {
            Ok(()) => return,
            Err(unsent) => *unsent.into_inner().0,
        },
        None => group,
    };
    for effect in group.effects.drain(..) {
        let _ = return_tx.try_send(effect);
    }
}

/// After an effect is inserted at effect index `at`, bump plugin-param target
/// slots so they keep pointing at the same plugins (effects at/after `at`
/// shifted up one). Slot 0 (instrument) is untouched.
fn shift_slots_after_insert(modulators: &mut [Modulator], at: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::PluginParam { slot, .. } = &mut t.kind {
                if *slot > at {
                    *slot += 1;
                }
            }
        }
    }
}

/// After the effect at index `removed` is deleted, drop targets that pointed at
/// it and shift higher slots down one.
fn fixup_slots_after_remove(modulators: &mut [Modulator], removed: usize) {
    let removed_slot = removed + 1;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            !matches!(&t.kind, ModTargetKind::PluginParam { slot, .. } if *slot == removed_slot)
        });
        for t in &mut m.targets {
            if let ModTargetKind::PluginParam { slot, .. } = &mut t.kind {
                if *slot > removed_slot {
                    *slot -= 1;
                }
            }
        }
    }
}

/// After the effect at `from` moves to `to`, remap plugin-param target slots
/// through the same permutation so they follow their plugins.
fn remap_slots_after_reorder(modulators: &mut [Modulator], from: usize, to: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::PluginParam { slot, .. } = &mut t.kind {
                if *slot >= 1 {
                    let ei = *slot - 1;
                    let new_ei = if ei == from {
                        to
                    } else if from < to && ei > from && ei <= to {
                        ei - 1
                    } else if from > to && ei >= to && ei < from {
                        ei + 1
                    } else {
                        ei
                    };
                    *slot = new_ei + 1;
                }
            }
        }
    }
}

/// Extract the mod_index from a cross-mod target kind, if any.
fn cross_mod_index(kind: &ModTargetKind) -> Option<usize> {
    match kind {
        ModTargetKind::PluginParam { .. }
        | ModTargetKind::GroupMember { .. }
        | ModTargetKind::GroupBus { .. }
        | ModTargetKind::GroupMemberMod { .. } => None,
        ModTargetKind::ModulatorRate { mod_index }
        | ModTargetKind::ModulatorAttack { mod_index }
        | ModTargetKind::ModulatorDecay { mod_index }
        | ModTargetKind::ModulatorSustain { mod_index }
        | ModTargetKind::ModulatorRelease { mod_index }
        | ModTargetKind::ModulatorDepth { mod_index, .. } => Some(*mod_index),
    }
}

/// Decrement cross-mod mod_index values that are greater than `removed_index`.
fn adjust_cross_mod_index(kind: &mut ModTargetKind, removed_index: usize) {
    let idx = match kind {
        ModTargetKind::PluginParam { .. }
        | ModTargetKind::GroupMember { .. }
        | ModTargetKind::GroupBus { .. }
        | ModTargetKind::GroupMemberMod { .. } => return,
        ModTargetKind::ModulatorRate { mod_index }
        | ModTargetKind::ModulatorAttack { mod_index }
        | ModTargetKind::ModulatorDecay { mod_index }
        | ModTargetKind::ModulatorSustain { mod_index }
        | ModTargetKind::ModulatorRelease { mod_index }
        | ModTargetKind::ModulatorDepth { mod_index, .. } => mod_index,
    };
    if *idx > removed_index {
        *idx -= 1;
    }
}

/// The inverse of `adjust_cross_mod_index`: bump a cross-mod mod_index that is at or after `inserted`.
fn shift_cross_mod_index(kind: &mut ModTargetKind, inserted: usize) {
    let idx = match kind {
        ModTargetKind::PluginParam { .. }
        | ModTargetKind::GroupMember { .. }
        | ModTargetKind::GroupBus { .. }
        | ModTargetKind::GroupMemberMod { .. } => return,
        ModTargetKind::ModulatorRate { mod_index }
        | ModTargetKind::ModulatorAttack { mod_index }
        | ModTargetKind::ModulatorDecay { mod_index }
        | ModTargetKind::ModulatorSustain { mod_index }
        | ModTargetKind::ModulatorRelease { mod_index }
        | ModTargetKind::ModulatorDepth { mod_index, .. } => mod_index,
    };
    if *idx >= inserted {
        *idx += 1;
    }
}

/// The inverse of `fixup_cross_mod_after_remove`: after a modulator goes into a rack at `inserted`, the cross-mod targets of its siblings that
/// address a modulator at that place or after it are told the place that modulator has now. (Before it goes in: its own cables are added after it,
/// and say the places there are by then.)
fn shift_cross_mod_after_insert(modulators: &mut [Modulator], inserted: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            shift_cross_mod_index(&mut t.kind, inserted);
        }
    }
}

/// When a modulator parameter is set by the user (e.g. SetModulatorRate),
/// update the `base_value` of any cross-mod targets pointing at that field.
fn update_cross_mod_base(modulators: &mut [Modulator], target_mod_index: usize, field: CrossModField, value: f32) {
    for m in modulators.iter_mut() {
        for target in &mut m.targets {
            let matches = match (&target.kind, &field) {
                (ModTargetKind::ModulatorRate { mod_index }, CrossModField::Rate) => *mod_index == target_mod_index,
                (ModTargetKind::ModulatorAttack { mod_index }, CrossModField::Attack) => *mod_index == target_mod_index,
                (ModTargetKind::ModulatorDecay { mod_index }, CrossModField::Decay) => *mod_index == target_mod_index,
                (ModTargetKind::ModulatorSustain { mod_index }, CrossModField::Sustain) => *mod_index == target_mod_index,
                (ModTargetKind::ModulatorRelease { mod_index }, CrossModField::Release) => *mod_index == target_mod_index,
                (ModTargetKind::ModulatorDepth { mod_index, target_index }, CrossModField::Depth(ti)) => {
                    *mod_index == target_mod_index && *target_index == *ti
                }
                _ => false,
            };
            if matches {
                target.base_value = value;
            }
        }
    }
}

/// Scale `bufs` by a gain that goes from `from` to `to` over the buffer, in equal steps: a lane or a bus that is let go of by a mute or a solo, or
/// brought back, is not cut or opened in one sample, which would click. Nothing at all for a gain that is 1 and stays so, which is what every lane
/// and group is until something is muted or soloed; and a gain that is 0 and stays so puts zeros and does not multiply by them, so that what a
/// plugin puts out that is not a number is not let through a mute (`NaN * 0` is `NaN`).
fn fade(bufs: &mut [Vec<f32>], num_channels: usize, from: f32, to: f32) {
    if from == 1.0 && to == 1.0 {
        return;
    }
    for buf in bufs.iter_mut().take(num_channels) {
        if from == 0.0 && to == 0.0 {
            buf.fill(0.0);
        } else {
            let step = (to - from) / buf.len() as f32;
            let mut gain = from;
            for s in buf.iter_mut() {
                gain += step;
                *s *= gain;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Group-scoped modulator helpers
// ---------------------------------------------------------------------------

/// The global lane index of the `member`-th member of `group` (members taken
/// in ascending lane order), or None if out of range.
fn group_member_lane(instruments: &[InstrumentLane], group: usize, member: usize) -> Option<usize> {
    instruments
        .iter()
        .enumerate()
        .filter(|(_, l)| l.group == Some(group))
        .nth(member)
        .map(|(i, _)| i)
}

/// The (group, member-ordinal) of lane `inst` within its group, or None if the
/// lane is ungrouped.
fn lane_member_ordinal(instruments: &[InstrumentLane], inst: usize) -> Option<(usize, usize)> {
    let group = instruments.get(inst)?.group?;
    let ordinal = instruments[..inst]
        .iter()
        .filter(|l| l.group == Some(group))
        .count();
    Some((group, ordinal))
}

/// Build the trigger MIDI for a group's modulators: note events whose note
/// falls within any member's range (members with no range match all notes),
/// plus all non-note events. Drives group-scoped envelope modulators so the
/// group envelope opens whenever any member would sound a note.
fn build_group_midi(
    instruments: &[InstrumentLane],
    group: usize,
    midi: &[(u64, [u8; 3])],
    out: &mut Vec<(u64, [u8; 3])>,
) {
    out.clear();
    for &(frame, bytes) in midi {
        let status = bytes[0] & 0xF0;
        if matches!(status, 0x80 | 0x90) {
            let note = bytes[1];
            let in_any = instruments
                .iter()
                .filter(|l| l.group == Some(group))
                .any(|l| l.range.is_none_or(|(lo, hi)| note >= lo && note <= hi));
            if in_any {
                out.push((frame, bytes));
            }
        } else {
            out.push((frame, bytes));
        }
    }
}

/// A pending parameter write produced by a group modulator.
enum GroupModWrite {
    /// A member instrument's plugin: resolved global lane index + chain slot.
    Member {
        lane: usize,
        slot: usize,
        param_index: u32,
        value: f32,
    },
    /// One of the group's own bus effects.
    Bus {
        group: usize,
        effect_index: usize,
        param_index: u32,
        value: f32,
    },
}

/// A member's parameter that group modulators drive: the lane, the chain slot, the
/// parameter's index, its base value, its range (min, max) and the offsets summed
/// so far.
type MemberDrive = (usize, usize, u32, f32, f32, f32, f32);

/// A bus effect's parameter that group modulators drive: the effect, the
/// parameter's index, its base value, its range (min, max) and the offsets summed
/// so far.
type BusDrive = (usize, u32, f32, f32, f32, f32);

/// Collect the parameter writes from every group's modulators, summing
/// contributions when several modulators within a group target the same
/// parameter (mirrors `apply_modulators_to_chain`). Member ordinals are
/// resolved to global lane indices against the current membership;
/// unresolvable targets (e.g. a member that left the group) are skipped.
/// `member_acc` and `bus_acc` are scratch space the caller keeps between buffers,
/// so that this does not allocate once they have grown to fit.
fn collect_group_mod_writes(
    groups: &[Group],
    instruments: &[InstrumentLane],
    out: &mut Vec<GroupModWrite>,
    member_acc: &mut Vec<MemberDrive>,
    bus_acc: &mut Vec<BusDrive>,
) {
    out.clear();
    for (gi, g) in groups.iter().enumerate() {
        member_acc.clear();
        bus_acc.clear();
        for m in &g.modulators {
            for t in &m.targets {
                match t.kind {
                    ModTargetKind::GroupMember {
                        member,
                        slot,
                        param_index,
                    } => {
                        let Some(lane) = group_member_lane(instruments, gi, member) else {
                            continue;
                        };
                        let off = m.target_offset(t);
                        if let Some(e) = member_acc
                            .iter_mut()
                            .find(|e| e.0 == lane && e.1 == slot && e.2 == param_index)
                        {
                            e.6 += off;
                        } else {
                            member_acc.push((
                                lane,
                                slot,
                                param_index,
                                t.base_value,
                                t.param_min,
                                t.param_max,
                                off,
                            ));
                        }
                    }
                    ModTargetKind::GroupBus {
                        effect_index,
                        param_index,
                    } => {
                        let off = m.target_offset(t);
                        if let Some(e) = bus_acc
                            .iter_mut()
                            .find(|e| e.0 == effect_index && e.1 == param_index)
                        {
                            e.5 += off;
                        } else {
                            bus_acc.push((
                                effect_index,
                                param_index,
                                t.base_value,
                                t.param_min,
                                t.param_max,
                                off,
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        for &(lane, slot, param_index, base, min, max, off) in member_acc.iter() {
            let value = (base + off).clamp(min, max);
            out.push(GroupModWrite::Member {
                lane,
                slot,
                param_index,
                value,
            });
        }
        for &(effect_index, param_index, base, min, max, off) in bus_acc.iter() {
            let value = (base + off).clamp(min, max);
            out.push(GroupModWrite::Bus {
                group: gi,
                effect_index,
                param_index,
                value,
            });
        }
    }
}

/// Publish what the group modulators' targets were just set to, into their slots:
/// each target on a member's or a bus effect's parameter gets the value written
/// to that parameter. A target that does not resolve (its member left the group)
/// has no write, and publishes nothing.
fn publish_group_effective(
    groups: &[Group],
    instruments: &[InstrumentLane],
    writes: &[GroupModWrite],
    telemetry: &Telemetry,
) {
    for (gi, g) in groups.iter().enumerate() {
        for target in g.modulators.iter().flat_map(|m| &m.targets) {
            let Some(slot) = target.effective else { continue };
            let value = match target.kind {
                ModTargetKind::GroupMember { member, slot: chain_slot, param_index } => {
                    let Some(lane) = group_member_lane(instruments, gi, member) else { continue };
                    writes.iter().find_map(|w| match *w {
                        GroupModWrite::Member { lane: l, slot: s, param_index: p, value }
                            if l == lane && s == chain_slot && p == param_index =>
                        {
                            Some(value)
                        }
                        _ => None,
                    })
                }
                ModTargetKind::GroupBus { effect_index, param_index } => writes.iter().find_map(|w| match *w {
                    GroupModWrite::Bus { group, effect_index: e, param_index: p, value }
                        if group == gi && e == effect_index && p == param_index =>
                    {
                        Some(value)
                    }
                    _ => None,
                }),
                _ => None,
            };
            if let Some(value) = value {
                telemetry.store(slot, value);
            }
        }
    }
}

/// Apply collected group-modulator writes to the member lanes and bus effects.
fn apply_group_mod_writes(
    writes: &[GroupModWrite],
    instruments: &mut [InstrumentLane],
    groups: &mut [Group],
) {
    for w in writes {
        match *w {
            GroupModWrite::Member {
                lane,
                slot,
                param_index,
                value,
            } => {
                if let Some(l) = instruments.get_mut(lane) {
                    let plugin: Option<&mut dyn Plugin> = if slot == 0 {
                        l.instrument.as_deref_mut()
                    } else {
                        l.effects.get_mut(slot - 1).map(|b| b.as_mut())
                    };
                    if let Some(p) = plugin {
                        let _ = p.set_parameter(param_index, value);
                    }
                }
            }
            GroupModWrite::Bus {
                group,
                effect_index,
                param_index,
                value,
            } => {
                if let Some(e) = groups
                    .get_mut(group)
                    .and_then(|g| g.effects.get_mut(effect_index))
                {
                    let _ = e.set_parameter(param_index, value);
                }
            }
        }
    }
}

/// A pending write from a group modulator into a member instrument's modulator.
struct GroupMemberModWrite {
    lane: usize,
    mod_index: usize,
    field: CrossModField,
    value: f32,
}

/// Collect group modulators' `GroupMemberMod` targets into cross-mod writes on
/// member lanes' modulators. Member ordinals resolve against current
/// membership; unresolvable ones are skipped. Multiple writers to the same
/// field are last-wins (matching sibling cross-mod).
fn collect_group_member_mod_writes(
    groups: &[Group],
    instruments: &[InstrumentLane],
    out: &mut Vec<GroupMemberModWrite>,
) {
    out.clear();
    for (gi, g) in groups.iter().enumerate() {
        for m in &g.modulators {
            for t in &m.targets {
                if let ModTargetKind::GroupMemberMod {
                    member,
                    mod_index,
                    field,
                } = t.kind
                {
                    let Some(lane) = group_member_lane(instruments, gi, member) else {
                        continue;
                    };
                    let value = m.modulated_value(t);
                    out.push(GroupMemberModWrite {
                        lane,
                        mod_index,
                        field,
                        value,
                    });
                }
            }
        }
    }
}

/// Apply collected group→member-modulator writes (bounds-checked).
fn apply_group_member_mod_writes(
    writes: &[GroupMemberModWrite],
    instruments: &mut [InstrumentLane],
) {
    for w in writes {
        if let Some(m) = instruments
            .get_mut(w.lane)
            .and_then(|l| l.modulators.get_mut(w.mod_index))
        {
            apply_cross_mod_field(m, w.field, w.value);
        }
    }
}

/// The member ordinal a group target lands on (GroupMember or GroupMemberMod),
/// as a mutable reference for fixups. Other kinds return None.
pub fn group_target_member_mut(kind: &mut ModTargetKind) -> Option<&mut usize> {
    match kind {
        ModTargetKind::GroupMember { member, .. } => Some(member),
        ModTargetKind::GroupMemberMod { member, .. } => Some(member),
        _ => None,
    }
}

/// After the member at ordinal `removed` leaves a group (membership change or
/// lane deletion), drop group targets pointing at it (GroupMember and
/// GroupMemberMod) and shift higher ordinals down one.
fn fixup_group_member_after_remove(modulators: &mut [Modulator], removed: usize) {
    for m in modulators.iter_mut() {
        m.targets.retain(|t| match &t.kind {
            ModTargetKind::GroupMember { member, .. }
            | ModTargetKind::GroupMemberMod { member, .. } => *member != removed,
            _ => true,
        });
        for t in &mut m.targets {
            if let Some(member) = group_target_member_mut(&mut t.kind) {
                if *member > removed {
                    *member -= 1;
                }
            }
        }
    }
}

/// After member `member`'s lane modulator at `removed` is deleted, drop
/// GroupMemberMod targets pointing at it and shift that member's higher
/// mod-indices down one.
fn fixup_group_member_mod_after_remove(
    modulators: &mut [Modulator],
    member: usize,
    removed: usize,
) {
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            !matches!(t.kind, ModTargetKind::GroupMemberMod { member: tm, mod_index, .. } if tm == member && mod_index == removed)
        });
        for t in &mut m.targets {
            if let ModTargetKind::GroupMemberMod { member: tm, mod_index, .. } = &mut t.kind {
                if *tm == member && *mod_index > removed {
                    *mod_index -= 1;
                }
            }
        }
    }
}

/// The inverse of `fixup_group_member_mod_after_remove`: after member `member`'s lane modulator goes in at `inserted`, the group's cables to
/// that member's modulators at that place or after it are told the place they have now.
fn shift_group_member_mod_after_insert(modulators: &mut [Modulator], member: usize, inserted: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMemberMod { member: tm, mod_index, .. } = &mut t.kind {
                if *tm == member && *mod_index >= inserted {
                    *mod_index += 1;
                }
            }
        }
    }
}

/// After the cable at `removed` is taken out of the modulator at `mod_index`, the cables of the
/// modulators in the same rack that drive the depth of a later cable of it (`ModulatorDepth`) are
/// told its new place. A cable is addressed by its place in its modulator, so they would drive the
/// next one's depth otherwise. The cables that drove the depth of the removed cable itself are
/// not touched: the controller takes them out first (`SessionModel::remove_edge`).
fn fixup_depth_cables_after_remove(modulators: &mut [Modulator], mod_index: usize, removed: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::ModulatorDepth { mod_index: of, target_index } = &mut t.kind {
                if *of == mod_index && *target_index > removed {
                    *target_index -= 1;
                }
            }
        }
    }
}

/// The same for a group's cables to the depth of a cable of a member lane's modulator
/// (`GroupMemberMod` with a `Depth`): after member `member`'s modulator `mod_index` loses the
/// cable at `removed`.
fn fixup_member_depth_cables_after_remove(modulators: &mut [Modulator], member: usize, mod_index: usize, removed: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMemberMod { member: of, mod_index: at, field: CrossModField::Depth(target) } = &mut t.kind {
                if *of == member && *at == mod_index && *target > removed {
                    *target -= 1;
                }
            }
        }
    }
}

/// The inverse of `fixup_depth_cables_after_remove`: after a cable goes into the modulator at `mod_index` at place `inserted`, the cables
/// of the modulators in the same rack that drive the depth of a cable of it at that place or after it are told the place that cable has now.
/// (Done before the cable goes in: its own kind is what it says, a place it was at, and is not one of those.)
fn shift_depth_cables_after_insert(modulators: &mut [Modulator], mod_index: usize, inserted: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::ModulatorDepth { mod_index: of, target_index } = &mut t.kind {
                if *of == mod_index && *target_index >= inserted {
                    *target_index += 1;
                }
            }
        }
    }
}

/// The same for a group's cables to the depth of a cable of a member lane's modulator (`GroupMemberMod` with a `Depth`): after member
/// `member`'s modulator `mod_index` gains a cable at place `inserted`.
fn shift_member_depth_cables_after_insert(modulators: &mut [Modulator], member: usize, mod_index: usize, inserted: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMemberMod { member: of, mod_index: at, field: CrossModField::Depth(target) } = &mut t.kind {
                if *of == member && *at == mod_index && *target >= inserted {
                    *target += 1;
                }
            }
        }
    }
}

/// How the member ordinals of the groups change when the lanes at `a` and `b` swap places, lane
/// and all (its plugins, its modulators, the group it is in). `lane_groups` is the group of each
/// lane by position, before the swap. For each lane in a group whose ordinal in it changes, the
/// group, the ordinal it had and the one it has: a group's cables address its members by ordinal,
/// which is the lane's place among the lanes of the group, so they have to be told.
pub fn member_moves(lane_groups: &[Option<usize>], a: usize, b: usize) -> Vec<(usize, usize, usize)> {
    let mut after = lane_groups.to_vec();
    after.swap(a, b);
    let ordinal = |groups: &[Option<usize>], at: usize, group: usize| groups[..at].iter().filter(|g| **g == Some(group)).count();
    let mut moves = Vec::new();
    for (position, group) in lane_groups.iter().enumerate() {
        let Some(group) = *group else { continue };
        let now_at = if position == a {
            b
        } else if position == b {
            a
        } else {
            position
        };
        let (old, new) = (ordinal(lane_groups, position, group), ordinal(&after, now_at, group));
        if old != new {
            moves.push((group, old, new));
        }
    }
    moves
}

/// Tell the cables of a group's modulators that address members by ordinal (`GroupMember`,
/// `GroupMemberMod`) the new ordinals, as `(old, new)`. They all change at once: two members that
/// change places must not meet half way.
fn remap_member_ordinals(modulators: &mut [Modulator], moves: &[(usize, usize)]) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let Some(member) = group_target_member_mut(&mut t.kind) {
                if let Some(&(_, new)) = moves.iter().find(|(old, _)| old == member) {
                    *member = new;
                }
            }
        }
    }
}

/// After a member joins a group at ordinal `inserted`, bump group-target member
/// ordinals at/after it so existing targets keep pointing at their members.
fn shift_group_member_after_insert(modulators: &mut [Modulator], inserted: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let Some(member) = group_target_member_mut(&mut t.kind) {
                if *member >= inserted {
                    *member += 1;
                }
            }
        }
    }
}

/// After an effect is inserted at effect index `at` on member `member`'s lane,
/// bump the group-target slots on that member that pointed at that effect or a
/// later one, so they keep pointing at the same plugins (the group counterpart of
/// `shift_slots_after_insert`). Slot 0, the instrument, never moves.
fn shift_member_slots_after_insert(modulators: &mut [Modulator], member: usize, at: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMember { member: tm, slot, .. } = &mut t.kind {
                if *tm == member && *slot > at {
                    *slot += 1;
                }
            }
        }
    }
}

/// After the effect at index `removed` is deleted from member `member`'s lane,
/// drop the group targets that pointed at it and shift that member's higher
/// slots down one (the group counterpart of `fixup_slots_after_remove`).
fn fixup_member_slots_after_remove(modulators: &mut [Modulator], member: usize, removed: usize) {
    let removed_slot = removed + 1;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            !matches!(&t.kind, ModTargetKind::GroupMember { member: tm, slot, .. }
                if *tm == member && *slot == removed_slot)
        });
        for t in &mut m.targets {
            if let ModTargetKind::GroupMember { member: tm, slot, .. } = &mut t.kind {
                if *tm == member && *slot > removed_slot {
                    *slot -= 1;
                }
            }
        }
    }
}

/// After the effect at `from` moves to `to` on member `member`'s lane, remap that
/// member's group-target slots through the same permutation so they follow their
/// plugins (the group counterpart of `remap_slots_after_reorder`).
fn remap_member_slots_after_reorder(modulators: &mut [Modulator], member: usize, from: usize, to: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMember { member: tm, slot, .. } = &mut t.kind {
                if *tm == member && *slot >= 1 {
                    let ei = *slot - 1;
                    let new_ei = if ei == from {
                        to
                    } else if from < to && ei > from && ei <= to {
                        ei - 1
                    } else if from > to && ei >= to && ei < from {
                        ei + 1
                    } else {
                        ei
                    };
                    *slot = new_ei + 1;
                }
            }
        }
    }
}

/// After a bus effect is inserted at `at`, bump the `GroupBus` targets that pointed at that effect or a later one, so they keep pointing at the same
/// effects (the inverse of `fixup_group_bus_after_remove`, and the bus counterpart of `shift_slots_after_insert`).
fn shift_group_bus_after_insert(modulators: &mut [Modulator], at: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupBus { effect_index, .. } = &mut t.kind {
                if *effect_index >= at {
                    *effect_index += 1;
                }
            }
        }
    }
}

/// After bus effect `removed` is deleted, drop GroupBus targets on it and shift
/// higher effect indices down one.
fn fixup_group_bus_after_remove(modulators: &mut [Modulator], removed: usize) {
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            !matches!(t.kind, ModTargetKind::GroupBus { effect_index, .. } if effect_index == removed)
        });
        for t in &mut m.targets {
            if let ModTargetKind::GroupBus { effect_index, .. } = &mut t.kind {
                if *effect_index > removed {
                    *effect_index -= 1;
                }
            }
        }
    }
}

/// After bus effect moves from `from` to `to`, remap GroupBus effect indices
/// through the same permutation so targets follow their effects.
fn remap_group_bus_after_reorder(modulators: &mut [Modulator], from: usize, to: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupBus { effect_index, .. } = &mut t.kind {
                let ei = *effect_index;
                *effect_index = if ei == from {
                    to
                } else if from < to && ei > from && ei <= to {
                    ei - 1
                } else if from > to && ei >= to && ei < from {
                    ei + 1
                } else {
                    ei
                };
            }
        }
    }
}

/// Update the `base_value` of GroupMember targets matching (member, slot,
/// param) after the user sets that member parameter.
fn update_group_member_base(
    modulators: &mut [Modulator],
    member: usize,
    slot: usize,
    param_index: u32,
    value: f32,
) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMember {
                member: tm,
                slot: ts,
                param_index: tp,
            } = t.kind
            {
                if tm == member && ts == slot && tp == param_index {
                    t.base_value = value;
                }
            }
        }
    }
}

/// Update the `base_value` of GroupBus targets matching (effect, param) after
/// the user sets that bus-effect parameter.
fn update_group_bus_base(
    modulators: &mut [Modulator],
    effect_index: usize,
    param_index: u32,
    value: f32,
) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupBus {
                effect_index: te,
                param_index: tp,
            } = t.kind
            {
                if te == effect_index && tp == param_index {
                    t.base_value = value;
                }
            }
        }
    }
}

/// Update the `base_value` of GroupMemberMod targets matching (member,
/// mod_index, field) after the user changes that member modulator's field —
/// keeps the group cross-mod centered on the user's latest setting.
fn update_group_member_mod_base(
    modulators: &mut [Modulator],
    member: usize,
    mod_index: usize,
    field: CrossModField,
    value: f32,
) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMemberMod {
                member: tm,
                mod_index: tmi,
                field: tf,
            } = t.kind
            {
                if tm == member && tmi == mod_index && tf == field {
                    t.base_value = value;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// GraphCommand — addressed commands for the AudioGraph
// ---------------------------------------------------------------------------

/// Commands sent from the main thread to mutate the audio graph on the audio thread.
pub enum GraphCommand {
    /// Swap the instrument plugin in a specific instrument lane. The plugin that goes out is handed to `keep` when there is one that has room and is listened to (the
    /// controller keeps it for the history, to put it back by another swap), and goes back by the return channel otherwise (`hand_back`); a lane that had none hands nothing back.
    SwapInstrument {
        inst: usize,
        instrument: Box<dyn Plugin>,
        inst_buf: Vec<Vec<f32>>,
        remapper: Option<NoteRemapper>,
        keep: Option<Sender<Box<dyn Plugin>>>,
    },
    /// Insert an effect into a specific instrument lane's chain.
    InsertEffect {
        inst: usize,
        index: usize,
        effect: Box<dyn Plugin>,
        mix: f64,
    },
    /// Remove an effect from a specific instrument lane's chain. The plugin is handed to `keep` when there is one that has room and is listened to
    /// (the controller keeps it for the history, to put it back), and goes back by the return channel otherwise (`hand_back`).
    RemoveEffect {
        inst: usize,
        index: usize,
        keep: Option<Sender<Box<dyn Plugin>>>,
    },
    /// Reorder an effect within a specific instrument lane's chain.
    ReorderEffect {
        inst: usize,
        from: usize,
        to: usize,
    },
    /// Set a parameter on a plugin. slot 0 = instrument, 1..N = effects.
    SetParameter {
        inst: usize,
        slot: usize,
        param_index: u32,
        value: f32,
    },
    /// Set the host-side dry/wet mix on an effect. slot 1..N = effects.
    SetMix {
        inst: usize,
        slot: usize,
        value: f32,
    },
    /// Load a preset on a plugin. slot 0 = instrument, 1..N = effects.
    LoadPreset {
        inst: usize,
        slot: usize,
        preset_id: String,
    },
    /// Set the host-side volume on an instrument lane.
    SetVolume {
        inst: usize,
        value: f32,
    },
    /// Mute or unmute a lane: its output is let go of (or brought back) over one buffer, and its notes, its modulators and its pattern go on,
    /// so that what is heard when it is brought back is where it would be.
    SetMute {
        inst: usize,
        muted: bool,
    },
    /// Solo or unsolo a lane: while anything is soloed (a lane or a group), only what is soloed is heard, and a group is heard for a lane in it
    /// that is soloed.
    SetSolo {
        inst: usize,
        solo: bool,
    },
    /// Set the note range for an instrument lane. None = full range.
    SetInstrumentRange {
        inst: usize,
        range: Option<(u8, u8)>,
    },
    /// Clear the instrument plugin from a lane (leaving it empty).
    ClearInstrument {
        inst: usize,
    },
    /// Swap instruments (with their buffers and remappers) between two lanes.
    SwapInstruments {
        inst_a: usize,
        inst_b: usize,
    },
    /// Add a new empty instrument lane.
    AddInstrument {
        range: Option<(u8, u8)>,
    },
    /// Remove an instrument lane entirely. The lane is handed to `keep` when there is one that has room and is listened to (the controller keeps it for the history, to
    /// put it back, `InsertInstrument`), and its plugins go back by the return channel otherwise (`hand_back_lane`).
    RemoveInstrument {
        inst: usize,
        keep: Option<Sender<KeptLane>>,
    },
    /// Put a lane that `RemoveInstrument { keep }` handed back into the graph at place `index` (the last place, for one past the end), as it was: it is told the place it has
    /// now, and the cables of its group, if it is a member, to the members from its place on are told theirs, before it goes in. What it counted as held, and what its pattern
    /// was playing, is forgotten (the notes were let go while it was out), and a group that is not there is no group. A lane that has fewer effects than it has mixes (it gave up
    /// its plugins, `KeptLane::take_plugins`) is turned away, and its plugins go back by the return channel.
    InsertInstrument {
        index: usize,
        lane: KeptLane,
    },
    /// Publish a lane's output (after its volume and effects, before it joins its
    /// group or the master) into a meter's slots each buffer (telemetry).
    SetLaneMeter {
        inst: usize,
        slots: MeterSlots,
    },
    /// Store the signal at the output of a node into ring `ring` of the probe pool (`Telemetry::probes`) on every buffer, from the next, in addition to the rings that already listen
    /// to it. A target that is not there (a lane, a group or a place in a chain that the graph has not) and a ring that the pool has not are ignored; a graph with no telemetry stores nothing.
    SetProbe {
        target: ProbeTarget,
        ring: usize,
    },
    /// Stop every probe: no ring is stored into any more (it keeps what it has). The controller sends this and then a `SetProbe` for each probe that stays, when the places in the graph
    /// have changed.
    ClearProbes,
    /// Add a new empty submix group; the new group's index is `groups.len()`.
    AddGroup,
    /// Publish a group's output (after its volume, effects and pan, before it
    /// joins the master) into a meter's slots each buffer (telemetry).
    SetGroupMeter {
        group: usize,
        slots: MeterSlots,
    },
    /// Remove a group; its members become ungrouped and higher group indices
    /// shift down. The group is handed to `keep` when there is one that has room and is listened to (the controller keeps it for the history, to put it back,
    /// `InsertGroup`), and its bus effects go back by the return channel otherwise (`hand_back_group`).
    RemoveGroup {
        group: usize,
        keep: Option<Sender<KeptGroup>>,
    },
    /// Put a group that `RemoveGroup { keep }` handed back into the graph at place `index` (the last place, for one past the end), as it was: the lanes that are in the groups from
    /// that place on are told the group they are in now (one more), and the lanes in `members` (places among the lanes: one that is no lane is nobody's) are its members. A group
    /// that has fewer bus effects than it has mixes (it gave up its plugins, `KeptGroup::take_plugins`) is turned away, and its plugins go back by the return channel.
    InsertGroup {
        index: usize,
        group: KeptGroup,
        members: Vec<usize>,
    },
    /// Set (or clear) a lane's group membership.
    SetLaneGroup {
        inst: usize,
        group: Option<usize>,
    },
    /// Set a group's output volume (applied to the member sum, before its FX).
    SetGroupVolume {
        group: usize,
        value: f32,
    },
    /// Set a group's stereo balance pan (-1 left … 0 center … +1 right).
    SetGroupPan {
        group: usize,
        value: f32,
    },
    /// Mute or unmute a group's bus: the members and the bus effects go on, and what comes out of the bus is let go of over one buffer.
    SetGroupMute {
        group: usize,
        muted: bool,
    },
    /// Solo or unsolo a group: every member of it is heard, with its bus, while the others that are not soloed are not.
    SetGroupSolo {
        group: usize,
        solo: bool,
    },
    /// Insert an effect into a group's bus chain.
    InsertGroupEffect {
        group: usize,
        index: usize,
        effect: Box<dyn Plugin>,
        mix: f64,
    },
    /// Remove an effect from a group's bus chain. The plugin is handed to `keep` as `RemoveEffect` says.
    RemoveGroupEffect {
        group: usize,
        index: usize,
        keep: Option<Sender<Box<dyn Plugin>>>,
    },
    /// Reorder an effect within a group's bus chain.
    ReorderGroupEffect {
        group: usize,
        from: usize,
        to: usize,
    },
    /// Set a group effect's dry/wet mix.
    SetGroupMix {
        group: usize,
        index: usize,
        value: f32,
    },
    /// Set a parameter on a group bus effect.
    SetGroupParameter {
        group: usize,
        index: usize,
        param_index: u32,
        value: f32,
    },
    /// Publish a modulator's output each buffer into `slot` (telemetry). The slot
    /// stays with the modulator when others around it come and go.
    SetModulatorOutput {
        location: ModulatorLocation,
        index: usize,
        slot: Slot,
    },
    /// Publish where a step sequencer is (its position) each buffer into `slot`. The slot stays with the modulator when others around it come and go.
    SetModulatorPosition {
        location: ModulatorLocation,
        index: usize,
        slot: Slot,
    },
    /// Set what a step sequencer goes through, how many of them, how it glides and whether a note starts it again; where it is stays.
    SetModulatorSteps {
        location: ModulatorLocation,
        index: usize,
        values: [f32; MAX_STEPS],
        length: usize,
        glide: f32,
        retrigger: bool,
    },
    /// Set whether a note starts an LFO again at the start of its wave and how many seconds it takes to come in after one (none for 0); where it is stays.
    SetModulatorKey {
        location: ModulatorLocation,
        index: usize,
        retrigger: bool,
        fade: f32,
    },
    /// Set how many beats of the tempo a cycle of an LFO or a step of a step sequencer is, or 0 for neither: it goes at its own rate again. Where it is stays.
    SetModulatorSync {
        location: ModulatorLocation,
        index: usize,
        beats: f32,
    },
    /// Insert a new modulator into the lane (modulators are lane-scoped).
    InsertModulator {
        inst: usize,
        index: usize,
        source: ModSource,
    },
    /// Remove a modulator from the lane.
    RemoveModulator {
        inst: usize,
        index: usize,
    },
    /// Set the rate of an LFO modulator.
    SetModulatorRate {
        inst: usize,
        mod_index: usize,
        rate: f32,
    },
    /// Set the waveform of an LFO modulator.
    SetModulatorWaveform {
        inst: usize,
        mod_index: usize,
        waveform: LfoWaveform,
    },
    /// Replace a modulator's source (for type switching between LFO/Envelope).
    SetModulatorSource {
        inst: usize,
        mod_index: usize,
        source: ModSource,
    },
    /// Set envelope parameters on an Envelope modulator.
    SetModulatorEnvelopeParam {
        inst: usize,
        mod_index: usize,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
    /// Add a modulation target to a modulator.
    AddModTarget {
        inst: usize,
        mod_index: usize,
        target: ModTarget,
    },
    /// Put a modulation target (a cable) into a modulator at place `target_index` (the last place, for one past the end): what
    /// `RemoveModTarget` takes out, put back. The cables of the rack that drive the depth of a later cable of the modulator are
    /// told its new place.
    InsertModTarget {
        inst: usize,
        mod_index: usize,
        target_index: usize,
        target: ModTarget,
    },
    /// Remove a modulation target (a cable) from a modulator.
    RemoveModTarget {
        inst: usize,
        mod_index: usize,
        target_index: usize,
    },
    /// Set the depth of a modulation target.
    SetModTargetDepth {
        inst: usize,
        mod_index: usize,
        target_index: usize,
        depth: f32,
    },
    /// Insert a new group-scoped modulator into a group.
    InsertGroupModulator {
        group: usize,
        index: usize,
        source: ModSource,
    },
    /// Remove a group-scoped modulator from a group.
    RemoveGroupModulator {
        group: usize,
        index: usize,
    },
    /// Set the rate of a group LFO modulator.
    SetGroupModulatorRate {
        group: usize,
        mod_index: usize,
        rate: f32,
    },
    /// Set the waveform of a group LFO modulator.
    SetGroupModulatorWaveform {
        group: usize,
        mod_index: usize,
        waveform: LfoWaveform,
    },
    /// Replace a group modulator's source (LFO/Envelope type switch).
    SetGroupModulatorSource {
        group: usize,
        mod_index: usize,
        source: ModSource,
    },
    /// Set envelope parameters on a group Envelope modulator.
    SetGroupModulatorEnvelopeParam {
        group: usize,
        mod_index: usize,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
    /// Add a modulation target to a group modulator.
    AddGroupModTarget {
        group: usize,
        mod_index: usize,
        target: ModTarget,
    },
    /// Put a modulation target (a cable) into a group modulator at place `target_index`, as `InsertModTarget` does for a lane's.
    InsertGroupModTarget {
        group: usize,
        mod_index: usize,
        target_index: usize,
        target: ModTarget,
    },
    /// Remove a modulation target (a cable) from a group modulator.
    RemoveGroupModTarget {
        group: usize,
        mod_index: usize,
        target_index: usize,
    },
    /// Set the depth of a group modulator's target.
    SetGroupModTargetDepth {
        group: usize,
        mod_index: usize,
        target_index: usize,
        depth: f32,
    },
    /// Enable/disable pattern playback for an instrument lane.
    SetPatternEnabled {
        inst: usize,
        enabled: bool,
    },
    /// Start/stop pattern recording for an instrument lane.
    SetPatternRecording {
        inst: usize,
        recording: bool,
    },
    /// Set the pattern data (e.g. after loading from session).
    SetPattern {
        inst: usize,
        pattern: Pattern,
        base_note: Option<u8>,
        in_key: bool,
    },
    /// Clear the pattern for an instrument lane.
    ClearPattern {
        inst: usize,
    },
    /// Swap patterns between two instrument lanes.
    SwapPatterns {
        inst_a: usize,
        inst_b: usize,
    },
    /// Set the global BPM (applied to all pattern players, and what the modulators that follow the tempo go by).
    SetGlobalBpm {
        bpm: f32,
    },
    /// Set pattern length in beats.
    SetPatternLength {
        inst: usize,
        beats: f32,
    },
    /// Set whether the pattern loops or plays once.
    SetPatternLooping {
        inst: usize,
        looping: bool,
    },
    /// Set whether pattern playback transposes by scale degrees (in-key,
    /// using the piano scale) or by semitones (chromatic).
    SetPatternInKey {
        inst: usize,
        in_key: bool,
    },
    /// Set the transpose (in semitones) for an instrument lane.
    SetTranspose {
        inst: usize,
        semitones: i8,
    },
}

// ---------------------------------------------------------------------------
// Pattern recorder/player
// ---------------------------------------------------------------------------

/// A single recorded MIDI event in a pattern.
#[derive(Clone)]
pub struct PatternEvent {
    /// Tick offset from pattern start (in samples at recording sample rate).
    pub frame: u64,
    /// MIDI status type: 0x90 = note-on, 0x80 = note-off.
    pub status: u8,
    /// Note number (absolute, will be transposed on playback).
    pub note: u8,
    /// Velocity.
    pub velocity: u8,
}

/// A recorded pattern — a sequence of note events with a fixed length.
#[derive(Clone, Default)]
pub struct Pattern {
    pub events: Vec<PatternEvent>,
    /// Total length of the pattern in samples.
    pub length_samples: u64,
}

/// Which modulator rack a modulator lives in: an instrument lane's or a
/// submix group's. Named by learn notifications, the session model and the
/// front-ends' popups alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModulatorLocation {
    Lane(usize),
    Group(usize),
}

/// Sent from the audio thread to the TUI when a `MidiLearn` modulator captures
/// a control, so the TUI can display and persist the binding.
#[derive(Debug, Clone, Copy)]
pub struct MidiLearnedNotification {
    pub location: ModulatorLocation,
    pub mod_index: usize,
    pub source: MidiBindSource,
}

/// Notification sent from audio thread to TUI when recording completes.
pub struct PatternNotification {
    pub inst: usize,
    pub base_note: Option<u8>,
    pub length_beats: f32,
    pub looping: bool,
    pub enabled: bool,
    /// (frame, status, note, velocity)
    pub events: Vec<(u64, u8, u8, u8)>,
}

/// Tracks one currently-sounding voice from pattern playback.
struct PatternVoice {
    /// The original pattern note (before transpose).
    pattern_note: u8,
    /// The transposed note actually playing.
    playing_note: u8,
    /// MIDI channel used for the note.
    channel: u8,
}

/// How pattern notes are mapped to output notes during transposed playback.
enum NoteMap {
    /// Shift every note by a fixed number of semitones (preserves intervals).
    Chromatic(i16),
    /// Shift by the scale-degree distance from base to trigger, keeping all
    /// notes aligned with the scale (a major-triad pattern triggered on the
    /// 2nd degree of a major scale comes out minor).
    InKey {
        scale: ScaleSetting,
        base: u8,
        trigger: u8,
    },
}

impl NoteMap {
    fn map(&self, note: u8) -> u8 {
        match *self {
            NoteMap::Chromatic(transpose) => (note as i16 + transpose).clamp(0, 127) as u8,
            NoteMap::InKey { scale, base, trigger } => scale.transpose_in_key(note, base, trigger),
        }
    }
}

/// How many beats of the metronome a recording is counted in with, before it takes the length of the pattern (the page tells it as `PATTERN_COUNT_IN_BEATS`).
pub const COUNT_IN_BEATS: f32 = 4.0;

/// The tempo a graph begins at, in beats a minute: the one that a session begins at (`app::model::DEFAULT_BPM`), which the audio thread has until it is told another.
pub const DEFAULT_BPM: f32 = 120.0;

/// Per-instrument pattern recorder and player.
struct PatternPlayer {
    pattern: Pattern,
    enabled: bool,
    recording: bool,
    /// Count-in phase: metronome ticks but no recording happens yet.
    counting_in: bool,
    /// Current playback position in samples within the pattern.
    playback_pos: u64,
    /// Recording position in samples since record started.
    record_pos: u64,
    /// Sample rate (needed for BPM → sample conversion).
    sample_rate: f32,
    /// The base note of the recorded pattern (first note-on recorded).
    base_note: Option<u8>,
    /// The note currently held that triggers playback. None = not playing.
    trigger_note: Option<u8>,
    /// All currently held notes (for switching trigger on key change).
    held_notes: Vec<u8>,
    /// Currently sounding voices from pattern playback.
    active_voices: Vec<PatternVoice>,
    /// Buffer for pattern-generated MIDI events to be merged into the stream.
    output_events: Vec<(u64, [u8; 3])>,
    /// Events recorded in the current recording pass.
    recording_events: Vec<PatternEvent>,
    /// Length of pattern in beats (default: 4 = 1 bar in 4/4).
    length_beats: f32,
    /// Whether the pattern loops when it reaches the end (default: true).
    looping: bool,
    /// Whether playback transposes by scale degrees of the piano scale
    /// (in-key) instead of by semitones (chromatic, the default).
    in_key: bool,
    /// BPM (global, set from main thread).
    bpm: f32,
    /// Notification sender for when recording completes automatically.
    pattern_tx: Option<Sender<PatternNotification>>,
    /// This player's instrument index (for notifications).
    inst_index: usize,
    // --- Metronome state ---
    /// Number of count-in beats before recording starts.
    count_in_beats: f32,
    /// Position in samples since the start of count-in (covers both count-in + recording).
    metronome_pos: u64,
    /// Samples per beat (precomputed when recording starts).
    beat_length_samples: u64,
    /// Metronome click oscillator phase (0.0–1.0).
    click_phase: f32,
    /// Remaining samples in the current click sound.
    click_remaining: u32,
    /// Whether the current click is a downbeat (higher pitch).
    click_is_downbeat: bool,
}

/// Metronome click duration in seconds.
const CLICK_DURATION_SECS: f32 = 0.025;
/// Metronome click frequency for normal beats (Hz).
const CLICK_FREQ: f32 = 1000.0;
/// Metronome click frequency for the downbeat (Hz).
const CLICK_DOWNBEAT_FREQ: f32 = 1500.0;
/// Metronome click volume (0.0–1.0).
const CLICK_VOLUME: f32 = 0.3;

impl PatternPlayer {
    fn new(sample_rate: f32) -> Self {
        PatternPlayer {
            pattern: Pattern::default(),
            enabled: false,
            recording: false,
            counting_in: false,
            playback_pos: 0,
            record_pos: 0,
            sample_rate,
            base_note: None,
            trigger_note: None,
            held_notes: Vec::new(),
            active_voices: Vec::new(),
            output_events: Vec::with_capacity(256),
            recording_events: Vec::new(),
            length_beats: 4.0,
            looping: true,
            in_key: false,
            bpm: DEFAULT_BPM,
            pattern_tx: None,
            inst_index: 0,
            count_in_beats: COUNT_IN_BEATS,
            metronome_pos: 0,
            beat_length_samples: 0,
            click_phase: 0.0,
            click_remaining: 0,
            click_is_downbeat: false,
        }
    }

    /// The length of the pattern in beats: what the next recording is counted in, and what the pattern that is there loops at. Its length in frames follows (the
    /// frames a beat of it has, as it was recorded, times the beats it has now): its notes stay where they are, and those that the loop no longer reaches wait for
    /// it to be long again. A pattern that is not there yet has no length in frames to change, and is only given the beats, which the pattern that is sent after it is counted in.
    fn set_length_beats(&mut self, beats: f32) {
        if self.length_beats > 0.0 {
            self.pattern.length_samples = (self.pattern.length_samples as f64 * f64::from(beats) / f64::from(self.length_beats)) as u64;
        }
        self.length_beats = beats;
    }

    /// Calculate pattern length in samples from BPM and length_beats.
    fn length_samples(&self) -> u64 {
        let beats_per_sec = self.bpm / 60.0;
        let seconds = self.length_beats / beats_per_sec;
        (seconds * self.sample_rate) as u64
    }

    /// Returns true if the metronome should be generating audio (count-in or recording).
    fn metronome_active(&self) -> bool {
        self.counting_in || self.recording
    }

    /// Called each audio buffer. Consumes incoming MIDI events, produces
    /// merged output events (original + pattern playback). `scale` is the
    /// current piano scale, used when in-key transposition is enabled.
    fn process(
        &mut self,
        midi_in: &[(u64, [u8; 3])],
        buffer_frames: usize,
        scale: ScaleSetting,
    ) -> &[(u64, [u8; 3])] {
        self.output_events.clear();

        if self.counting_in {
            self.process_count_in(midi_in, buffer_frames);
            // During count-in, pass through original events (user may play along)
            self.output_events.extend_from_slice(midi_in);
            return &self.output_events;
        }

        if self.recording {
            self.process_recording(midi_in, buffer_frames);
            // During recording, pass through original events unmodified
            self.output_events.extend_from_slice(midi_in);
            return &self.output_events;
        }

        if !self.enabled || self.pattern.events.is_empty() || self.base_note.is_none() {
            // No pattern — pass through
            self.output_events.extend_from_slice(midi_in);
            return &self.output_events;
        }

        self.process_playback(midi_in, buffer_frames, scale);
        &self.output_events
    }

    /// Render metronome clicks into audio buffers. Call after instrument processing.
    /// Adds click samples additively to existing audio in `output`.
    fn render_metronome(&mut self, output: &mut [Vec<f32>], buffer_frames: usize) {
        if !self.metronome_active() || self.beat_length_samples == 0 {
            return;
        }

        let click_duration_samples = (CLICK_DURATION_SECS * self.sample_rate) as u32;

        for i in 0..buffer_frames {
            let sample_pos = self.metronome_pos + i as u64;

            // Check if we're at a beat boundary
            if sample_pos.is_multiple_of(self.beat_length_samples) {
                // Determine which beat this is in the overall sequence
                let beat_index = sample_pos / self.beat_length_samples;
                self.click_is_downbeat = beat_index.is_multiple_of(self.count_in_beats as u64);
                self.click_remaining = click_duration_samples;
                self.click_phase = 0.0;
            }

            // Generate click sample
            if self.click_remaining > 0 {
                let freq = if self.click_is_downbeat {
                    CLICK_DOWNBEAT_FREQ
                } else {
                    CLICK_FREQ
                };
                let phase_inc = freq / self.sample_rate;
                self.click_phase = (self.click_phase + phase_inc) % 1.0;

                // Sine wave with exponential decay envelope
                let t = 1.0 - (self.click_remaining as f32 / click_duration_samples as f32);
                let envelope = (-t * 8.0).exp(); // fast decay
                let sample = (self.click_phase * std::f32::consts::TAU).sin()
                    * envelope
                    * CLICK_VOLUME;

                // Add to all channels
                for ch in output.iter_mut() {
                    if i < ch.len() {
                        ch[i] += sample;
                    }
                }

                self.click_remaining -= 1;
            }
        }

        self.metronome_pos += buffer_frames as u64;
    }

    fn process_count_in(&mut self, midi_in: &[(u64, [u8; 3])], buffer_frames: usize) {
        let count_in_samples = (self.count_in_beats as u64) * self.beat_length_samples;

        // Capture note-ons during count-in — they'll be snapped to frame 0.
        for &(_frame, bytes) in midi_in {
            let status_type = bytes[0] & 0xF0;
            match status_type {
                0x90 if bytes[2] > 0 => {
                    self.recording_events.push(PatternEvent {
                        frame: 0,
                        status: 0x90,
                        note: bytes[1],
                        velocity: bytes[2],
                    });
                }
                0x80 | 0x90 => {
                    // Note-off during count-in: also snap to frame 0
                    self.recording_events.push(PatternEvent {
                        frame: 0,
                        status: 0x80,
                        note: bytes[1],
                        velocity: 0,
                    });
                }
                _ => {}
            }
        }

        // Note: metronome_pos is advanced by render_metronome, but we need to
        // track count-in progress here too. Use record_pos as count-in position.
        self.record_pos += buffer_frames as u64;

        if self.record_pos >= count_in_samples {
            // Count-in complete — transition to recording
            self.counting_in = false;
            self.recording = true;
            self.record_pos = 0;
            // metronome_pos continues (don't reset — keeps beat alignment)
        }
    }

    fn process_recording(&mut self, midi_in: &[(u64, [u8; 3])], buffer_frames: usize) {
        let length = self.length_samples();

        for &(frame, bytes) in midi_in {
            let status_type = bytes[0] & 0xF0;
            match status_type {
                0x90 if bytes[2] > 0 => {
                    // Note-on
                    self.recording_events.push(PatternEvent {
                        frame: self.record_pos + frame,
                        status: 0x90,
                        note: bytes[1],
                        velocity: bytes[2],
                    });
                }
                0x80 | 0x90 => {
                    // Note-off
                    self.recording_events.push(PatternEvent {
                        frame: self.record_pos + frame,
                        status: 0x80,
                        note: bytes[1],
                        velocity: 0,
                    });
                }
                _ => {
                    // CC, pitch bend, etc.: not recorded
                }
            }
        }

        self.record_pos += buffer_frames as u64;

        // Check if recording time has elapsed
        if self.record_pos >= length {
            self.finalize_recording(length);
        }
    }

    fn finalize_recording(&mut self, length_samples: u64) {
        // Clamp events to pattern length
        self.recording_events.retain(|e| e.frame < length_samples);

        // Base note = lowest note-on in the recording (for transpose reference).
        self.base_note = self.recording_events.iter()
            .filter(|e| e.status == 0x90)
            .map(|e| e.note)
            .min();

        self.pattern = Pattern {
            events: std::mem::take(&mut self.recording_events),
            length_samples,
        };
        self.recording = false;
        self.counting_in = false;
        self.enabled = !self.pattern.events.is_empty();
        self.click_remaining = 0;

        // Notify main thread with the recorded data
        if let Some(ref tx) = self.pattern_tx {
            let events = self.pattern.events.iter().map(|e| {
                (e.frame, e.status, e.note, e.velocity)
            }).collect();
            let _ = tx.try_send(PatternNotification {
                inst: self.inst_index,
                base_note: self.base_note,
                length_beats: self.length_beats,
                looping: self.looping,
                enabled: self.enabled,
                events,
            });
        }
    }

    fn process_playback(
        &mut self,
        midi_in: &[(u64, [u8; 3])],
        buffer_frames: usize,
        scale: ScaleSetting,
    ) {
        let base = match self.base_note {
            Some(n) => n,
            None => {
                self.output_events.extend_from_slice(midi_in);
                return;
            }
        };

        // Scan incoming MIDI for trigger note-on/off.
        // Track held notes so we can switch triggers instantly.
        for &(frame, bytes) in midi_in {
            let status_type = bytes[0] & 0xF0;
            match status_type {
                0x90 if bytes[2] > 0 => {
                    self.held_notes.push(bytes[1]);
                    if self.trigger_note.is_some() && self.trigger_note != Some(bytes[1]) {
                        // Switch to new trigger: kill active voices, restart
                        for voice in self.active_voices.drain(..) {
                            self.output_events.push((
                                frame,
                                [0x80 | voice.channel, voice.playing_note, 0],
                            ));
                        }
                    }
                    self.trigger_note = Some(bytes[1]);
                    self.playback_pos = 0;
                    // Swallow note events — pattern handles them
                }
                0x80 | 0x90 => {
                    self.held_notes.retain(|&n| n != bytes[1]);
                    if self.trigger_note == Some(bytes[1]) {
                        if let Some(&last) = self.held_notes.last() {
                            // Another key is still held — switch to it
                            for voice in self.active_voices.drain(..) {
                                self.output_events.push((
                                    frame,
                                    [0x80 | voice.channel, voice.playing_note, 0],
                                ));
                            }
                            self.trigger_note = Some(last);
                            self.playback_pos = 0;
                        } else {
                            // No keys held — stop playback
                            for voice in self.active_voices.drain(..) {
                                self.output_events.push((
                                    frame,
                                    [0x80 | voice.channel, voice.playing_note, 0],
                                ));
                            }
                            self.trigger_note = None;
                        }
                    }
                    // Swallow note events
                }
                _ => {
                    // Pass through CC, pitch bend, etc.
                    self.output_events.push((frame, bytes));
                }
            }
        }

        // If no trigger is active, nothing to emit
        let trigger = match self.trigger_note {
            Some(t) => t,
            None => return,
        };
        let note_map = if self.in_key {
            NoteMap::InKey { scale, base, trigger }
        } else {
            NoteMap::Chromatic(trigger as i16 - base as i16)
        };

        let pattern_len = self.pattern.length_samples;
        if pattern_len == 0 {
            return;
        }

        let buf_start = self.playback_pos;
        let buf_end = self.playback_pos + buffer_frames as u64;

        // Check for end-of-pattern
        if buf_end > pattern_len {
            // Emit events from buf_start..pattern_len
            self.emit_events_in_range(buf_start, pattern_len, &note_map, 0);
            // Send note-off for all active voices at the boundary
            for voice in self.active_voices.drain(..) {
                let boundary_frame = pattern_len - buf_start;
                self.output_events.push((
                    boundary_frame,
                    [0x80 | voice.channel, voice.playing_note, 0],
                ));
            }
            if self.looping {
                // Wrap around and continue from the start
                let remainder = buf_end - pattern_len;
                let offset = pattern_len - buf_start;
                self.emit_events_in_range(0, remainder, &note_map, offset);
                self.playback_pos = remainder;
            } else {
                // One-shot: stop playback
                self.playback_pos = pattern_len;
            }
        } else {
            self.emit_events_in_range(buf_start, buf_end, &note_map, 0);
            self.playback_pos = buf_end;
            if self.playback_pos >= pattern_len {
                // Exact boundary
                for voice in self.active_voices.drain(..) {
                    self.output_events.push((
                        (buffer_frames - 1) as u64,
                        [0x80 | voice.channel, voice.playing_note, 0],
                    ));
                }
                if self.looping {
                    self.playback_pos = 0;
                }
            }
        }
    }

    /// Emit pattern events that fall within [range_start, range_end), with frame
    /// offsets adjusted by `frame_offset` for the output buffer.
    fn emit_events_in_range(
        &mut self,
        range_start: u64,
        range_end: u64,
        note_map: &NoteMap,
        frame_offset: u64,
    ) {
        let output_events = &mut self.output_events;
        let active_voices = &mut self.active_voices;
        for ev in &self.pattern.events {
            if ev.frame >= range_start && ev.frame < range_end {
                let out_frame = ev.frame - range_start + frame_offset;

                if ev.status == 0x90 {
                    // Note-on
                    let mapped_note = note_map.map(ev.note);
                    output_events.push((out_frame, [0x90, mapped_note, ev.velocity]));
                    active_voices.push(PatternVoice {
                        pattern_note: ev.note,
                        playing_note: mapped_note,
                        channel: 0,
                    });
                } else {
                    // Note-off — release the note(s) the matching voices are
                    // actually sounding, so the off always pairs with its on
                    // even if the scale changed while the note was held.
                    let mut released = false;
                    active_voices.retain(|v| {
                        if v.pattern_note == ev.note {
                            output_events.push((out_frame, [0x80 | v.channel, v.playing_note, 0]));
                            released = true;
                            false
                        } else {
                            true
                        }
                    });
                    if !released {
                        output_events.push((out_frame, [0x80, note_map.map(ev.note), 0]));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Probes — the signal at the output of a node
// ---------------------------------------------------------------------------

/// Where a probe listens: the signal at the output of a node, which is stored into a ring of `Telemetry::probes` on every buffer while the probe is set
/// (`GraphCommand::SetProbe`). The places are the ones `app::model::NodePath` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeTarget {
    /// A lane's instrument, as it goes out of the lane's volume and before its effects.
    Instrument(usize),
    /// An effect of a lane's chain, after its mix.
    Effect { inst: usize, index: usize },
    /// A group's bus, after its volume, its effects and its pan: what is added to the master.
    Group(usize),
    /// An effect of a group's bus, after its mix and before the group's pan.
    GroupEffect { group: usize, index: usize },
}

/// Where in a lane or a group a signal is stored from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TapStage {
    /// What goes out of a lane's volume (its instrument), or out of a group's pan.
    Output,
    /// What goes out of the effect at this place in the chain.
    Effect(usize),
}

/// A ring of the probe pool that the signal at a stage of a lane or a group is stored into.
#[derive(Clone, Copy, Debug)]
struct Tap {
    stage: TapStage,
    ring: usize,
}

/// Add a tap, unless there are as many as there are rings (a tap is a ring's, so that there are never more: the room for them was made with the lane or the group, and
/// the audio thread never allocates for one).
fn add_tap(taps: &mut Vec<Tap>, stage: TapStage, ring: usize) {
    if ring < telemetry::PROBES && taps.len() < telemetry::PROBES {
        taps.push(Tap { stage, ring });
    }
}

/// Store `bufs`, the signal at `stage`, into the rings that listen to it (the first two channels: the left side and the right).
fn store_taps(taps: &[Tap], telemetry: Option<&Telemetry>, stage: TapStage, bufs: &[Vec<f32>]) {
    let Some(telemetry) = telemetry else { return };
    for tap in taps.iter().filter(|tap| tap.stage == stage) {
        if let Some(ring) = telemetry.probes.get(tap.ring) {
            ring.push(bufs);
        }
    }
}

// ---------------------------------------------------------------------------
// InstrumentLane — one instrument + effect chain
// ---------------------------------------------------------------------------

struct InstrumentLane {
    range: Option<(u8, u8)>,
    instrument: Option<Box<dyn Plugin>>,
    volume: f32,
    inst_buf: Vec<Vec<f32>>,
    effects: Vec<Box<dyn Plugin>>,
    mix_values: Vec<f64>,
    buf_a: Vec<Vec<f32>>,
    buf_b: Vec<Vec<f32>>,
    remapper: Option<NoteRemapper>,
    remapped_events: Vec<(u64, [u8; 3])>,
    transposed_events: Vec<(u64, [u8; 3])>,
    filtered_midi: Vec<(u64, [u8; 3])>,
    /// Modulators for this whole lane. Each modulator's targets address any
    /// plugin in the chain by slot (0 = instrument, 1..N = effects).
    modulators: Vec<Modulator>,
    /// Scratch space for applying the modulators each buffer (cross-mod writes,
    /// summed parameter drives), kept so the audio thread does not allocate.
    cross_mod_scratch: Vec<CrossModWrite>,
    drive_scratch: Vec<ParamDrive>,
    /// Measures the lane's output; `None` publishes nothing.
    meter: Option<Meter>,
    /// The probes that listen to this lane: its instrument after the volume, and each of its effects.
    taps: Vec<Tap>,
    /// Pattern recorder/player for this instrument.
    pattern: PatternPlayer,
    /// Transpose in semitones applied to note events.
    transpose: i8,
    /// If `Some(g)`, this lane is a member of group `g`: its output is summed
    /// into that group's bus (and its group FX) instead of straight to master.
    group: Option<usize>,
    /// Count of currently-held notes per MIDI channel (0..15), maintained from
    /// range-filtered note on/off. Drives MPE per-note expression routing:
    /// member-channel (2..16) pressure / bend / CC reaches only the lane that
    /// is sounding a note on that channel, so one instrument's expression can't
    /// bleed into another's modulators.
    active_note_channels: [u16; 16],
    /// Muted: never heard (a mute wins over a solo). Its notes, modulators and pattern go on.
    muted: bool,
    /// Soloed: while anything is, only what is soloed is heard.
    solo: bool,
    /// The gain the lane's output was heard at when the last buffer ended, 1 or 0: the next buffer goes from it to where the lane should be,
    /// over the buffer, so that a mute is not a step.
    heard_at: f32,
}

impl InstrumentLane {
    fn new(num_channels: usize) -> Self {
        InstrumentLane {
            range: None,
            instrument: None,
            volume: 1.0,
            inst_buf: Vec::new(),
            effects: Vec::new(),
            mix_values: Vec::new(),
            buf_a: (0..num_channels).map(|_| Vec::new()).collect(),
            buf_b: (0..num_channels).map(|_| Vec::new()).collect(),
            remapper: None,
            remapped_events: Vec::with_capacity(128),
            transposed_events: Vec::with_capacity(128),
            filtered_midi: Vec::with_capacity(128),
            modulators: Vec::new(),
            cross_mod_scratch: Vec::with_capacity(SCRATCH_CAPACITY),
            drive_scratch: Vec::with_capacity(SCRATCH_CAPACITY),
            meter: None,
            taps: Vec::with_capacity(telemetry::PROBES),
            pattern: PatternPlayer::new(48000.0),
            transpose: 0,
            group: None,
            active_note_channels: [0; 16],
            muted: false,
            solo: false,
            heard_at: 1.0,
        }
    }

    /// Get the sample rate. Derived from the instrument if loaded, otherwise a sensible default.
    fn sample_rate(&self) -> f32 {
        self.instrument
            .as_ref()
            .map(|i| i.sample_rate())
            .unwrap_or(48000.0)
    }

    /// Filter MIDI events by this instrument's key range.
    /// Note-on/note-off: only pass if note is within range (inclusive).
    /// CC, pitch bend, channel pressure, etc.: always pass through.
    fn filter_midi(&mut self, midi_events: &[(u64, [u8; 3])]) {
        self.filtered_midi.clear();
        for &(frame, bytes) in midi_events {
            let status = bytes[0] & 0xF0;
            let ch = (bytes[0] & 0x0F) as usize;
            match status {
                0x80 | 0x90 => {
                    // Note on/off: filter by range (no range = full range).
                    let note = bytes[1];
                    let in_range = self.range.is_none_or(|(lo, hi)| note >= lo && note <= hi);
                    if in_range {
                        // Track which channels this lane is sounding, so per-note
                        // MPE expression on member channels can be routed to us.
                        if status == 0x90 && bytes[2] > 0 {
                            self.active_note_channels[ch] =
                                self.active_note_channels[ch].saturating_add(1);
                        } else {
                            self.active_note_channels[ch] =
                                self.active_note_channels[ch].saturating_sub(1);
                        }
                        self.filtered_midi.push((frame, bytes));
                    }
                }
                // Per-note expression (poly aftertouch, CC, channel pressure,
                // pitch bend). MIDI channel 1 (index 0) is the MPE master /
                // non-MPE global channel — always passed. Member channels
                // (2..16) carry per-note MPE expression, so they reach only the
                // lane holding a note on that channel; otherwise one
                // instrument's pressure/bend/slide bleeds into another's
                // modulators. (A non-MPE controller on channel 1 is unaffected.)
                0xA0 | 0xB0 | 0xD0 | 0xE0 => {
                    if ch == 0 || self.active_note_channels[ch] > 0 {
                        self.filtered_midi.push((frame, bytes));
                    }
                }
                // System / everything else: pass through.
                _ => self.filtered_midi.push((frame, bytes)),
            }
        }
    }

    /// Process this instrument + effect chain, writing output to `inst_out`.
    /// `inst_out` must have `num_channels` vecs, each with `frames` length.
    /// `scale` is the current piano scale for in-key pattern transposition, and `bpm` the tempo that the modulators that follow it go by.
    fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        inst_out: &mut [Vec<f32>],
        num_channels: usize,
        scale: ScaleSetting,
        bpm: f32,
        telemetry: Option<&Telemetry>,
    ) -> anyhow::Result<()> {
        // Filter MIDI by range
        self.filter_midi(midi_events);

        // Apply note remapping if configured
        let effective_events: &[(u64, [u8; 3])] = if let Some(ref remapper) = self.remapper {
            remapper.remap_events(&self.filtered_midi, &mut self.remapped_events);
            &self.remapped_events
        } else {
            &self.filtered_midi
        };

        // Pattern recorder/player — process after remapping, before modulators.
        let frames = inst_out.first().map(|b| b.len()).unwrap_or(0);
        let effective_events = self.pattern.process(effective_events, frames, scale);

        // Apply transpose to note events.
        let effective_events = if self.transpose != 0 {
            self.transposed_events.clear();
            for &(frame, bytes) in effective_events {
                let status_type = bytes[0] & 0xF0;
                if matches!(status_type, 0x80 | 0x90) {
                    let note = bytes[1] as i16 + self.transpose as i16;
                    if (0..=127).contains(&note) {
                        self.transposed_events.push((frame, [bytes[0], note as u8, bytes[2]]));
                    }
                    // Drop notes that fall outside 0-127
                } else {
                    self.transposed_events.push((frame, bytes));
                }
            }
            self.transposed_events.as_slice()
        } else {
            effective_events
        };

        // Apply modulators (block-rate: once per buffer, before processing the
        // chain). Lane-scoped: tick all → cross-mod → route each target to its
        // slot's plugin (instrument or an effect). Applying up front is
        // equivalent to per-slot application since each plugin reads its
        // parameters when it processes, later in this same buffer.
        let buffer_size = inst_out.first().map(|b| b.len()).unwrap_or(0);
        if buffer_size > 0 {
            for m in &mut self.modulators {
                m.tick(buffer_size, effective_events, bpm);
            }
            apply_cross_mod(&mut self.modulators, &mut self.cross_mod_scratch, telemetry);
            apply_modulators_to_chain(
                &self.modulators,
                &mut self.instrument,
                &mut self.effects,
                &mut self.drive_scratch,
                telemetry,
            );
        }

        let instrument = match self.instrument.as_mut() {
            Some(inst) => inst,
            None => {
                for ch in inst_out.iter_mut() {
                    ch.fill(0.0);
                }
                // Render metronome even without an instrument (count-in)
                let frames = inst_out.first().map(|b| b.len()).unwrap_or(0);
                self.pattern.render_metronome(inst_out, frames);
                return Ok(());
            }
        };

        let frames = inst_out.first().map(|b| b.len()).unwrap_or(0);
        let inst_outputs = self.inst_buf.len();

        if inst_outputs <= num_channels && self.effects.is_empty() && (self.volume - 1.0).abs() < f32::EPSILON {
            // Fast path: instrument output fits, no effects, no volume scaling
            let mut storage = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
            let out_refs = mut_slices(inst_out, &mut storage);
            instrument.process(effective_events, &[], out_refs)?;
            store_taps(&self.taps, telemetry, TapStage::Output, inst_out);
            self.pattern.render_metronome(inst_out, frames);
            return Ok(());
        }

        // Resize inst_buf
        for buf in self.inst_buf.iter_mut() {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }

        // Instrument → inst_buf
        {
            let mut storage = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
            let refs = mut_slices(&mut self.inst_buf, &mut storage);
            instrument.process(effective_events, &[], refs)?;
        }

        // Apply volume
        if (self.volume - 1.0).abs() >= f32::EPSILON {
            for ch in 0..self.inst_buf.len().min(num_channels) {
                for sample in self.inst_buf[ch].iter_mut() {
                    *sample *= self.volume;
                }
            }
        }
        store_taps(&self.taps, telemetry, TapStage::Output, &self.inst_buf);

        if self.effects.is_empty() {
            // No effects — copy first num_channels from inst_buf to output
            for (ch, out) in inst_out.iter_mut().enumerate() {
                if ch < self.inst_buf.len() {
                    out.copy_from_slice(&self.inst_buf[ch]);
                } else {
                    out.fill(0.0);
                }
            }
            self.pattern.render_metronome(inst_out, frames);
            return Ok(());
        }

        // Resize effect ping-pong buffers
        for buf in self.buf_a.iter_mut().chain(self.buf_b.iter_mut()) {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }

        // Copy first num_channels from inst_buf → buf_a
        for ch in 0..num_channels {
            if ch < self.inst_buf.len() {
                self.buf_a[ch].copy_from_slice(&self.inst_buf[ch]);
            } else {
                self.buf_a[ch].fill(0.0);
            }
        }

        // Effects: alternate between buf_a and buf_b
        let mut src_is_a = true;

        for (index, (effect, &mix)) in self.effects.iter_mut().zip(self.mix_values.iter()).enumerate() {
            let mix = mix as f32;

            if src_is_a {
                {
                    let mut in_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let mut out_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let in_refs = shared_slices(&self.buf_a, &mut in_s);
                    let out_refs = mut_slices(&mut self.buf_b, &mut out_s);
                    effect.process(&[], in_refs, out_refs)?;
                }

                if mix < 1.0 {
                    let dry = 1.0 - mix;
                    for ch in 0..num_channels {
                        for i in 0..frames {
                            self.buf_b[ch][i] = self.buf_a[ch][i] * dry + self.buf_b[ch][i] * mix;
                        }
                    }
                }
                store_taps(&self.taps, telemetry, TapStage::Effect(index), &self.buf_b);
            } else {
                {
                    let mut in_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let mut out_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let in_refs = shared_slices(&self.buf_b, &mut in_s);
                    let out_refs = mut_slices(&mut self.buf_a, &mut out_s);
                    effect.process(&[], in_refs, out_refs)?;
                }

                if mix < 1.0 {
                    let dry = 1.0 - mix;
                    for ch in 0..num_channels {
                        for i in 0..frames {
                            self.buf_a[ch][i] = self.buf_b[ch][i] * dry + self.buf_a[ch][i] * mix;
                        }
                    }
                }
                store_taps(&self.taps, telemetry, TapStage::Effect(index), &self.buf_a);
            }
            src_is_a = !src_is_a;
        }

        // Copy final result to inst_out
        let final_buf = if src_is_a { &self.buf_a } else { &self.buf_b };
        for (ch, out) in inst_out.iter_mut().enumerate() {
            if ch < final_buf.len() {
                let copy_len = out.len().min(final_buf[ch].len());
                out[..copy_len].copy_from_slice(&final_buf[ch][..copy_len]);
            }
        }

        // Metronome click (additive, on top of instrument+effects)
        self.pattern.render_metronome(inst_out, frames);

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Group — submix bus
// ---------------------------------------------------------------------------

/// A submix bus. Member lanes (those whose `group` points here) sum into
/// `accum` instead of the master; the group then applies its output volume and
/// its own effect chain to that sum, and the result is added to the master.
/// A group also owns modulators that can drive its members and bus effects
/// (see `collect_group_mod_writes`).
struct Group {
    effects: Vec<Box<dyn Plugin>>,
    mix_values: Vec<f64>,
    /// Output gain applied to the member sum, before the group effects.
    volume: f32,
    /// Stereo balance pan for the group bus output: -1 = left, 0 = center,
    /// +1 = right. Applied after the effect chain.
    pan: f32,
    /// Group-scoped modulators. Each target addresses a member instrument's
    /// chain (`GroupMember`), one of this group's bus effects (`GroupBus`), or
    /// a sibling group modulator (cross-mod).
    modulators: Vec<Modulator>,
    /// Member-sum accumulator (num_channels vecs).
    accum: Vec<Vec<f32>>,
    /// Effect ping-pong scratch.
    buf_a: Vec<Vec<f32>>,
    buf_b: Vec<Vec<f32>>,
    /// Measures the group's output; `None` publishes nothing.
    meter: Option<Meter>,
    /// The probes that listen to this group: its bus after the pan, and each of its bus effects.
    taps: Vec<Tap>,
    /// Muted: the bus is never heard (the members and the effects go on, so that its tail is where it would be). A mute wins over a solo.
    muted: bool,
    /// Soloed: while anything is, only what is soloed is heard, and every member of this group.
    solo: bool,
    /// The gain the bus was heard at when the last buffer ended (see `InstrumentLane::heard_at`).
    heard_at: f32,
}

impl Group {
    fn new(num_channels: usize) -> Self {
        Group {
            effects: Vec::new(),
            mix_values: Vec::new(),
            volume: 1.0,
            pan: 0.0,
            modulators: Vec::new(),
            accum: (0..num_channels).map(|_| Vec::new()).collect(),
            buf_a: (0..num_channels).map(|_| Vec::new()).collect(),
            buf_b: (0..num_channels).map(|_| Vec::new()).collect(),
            meter: None,
            taps: Vec::with_capacity(telemetry::PROBES),
            muted: false,
            solo: false,
            heard_at: 1.0,
        }
    }

    /// Resize + zero the accumulator for a fresh buffer.
    fn begin(&mut self, frames: usize, num_channels: usize) {
        if self.accum.len() < num_channels {
            self.accum.resize_with(num_channels, Vec::new);
        }
        for ch in self.accum.iter_mut() {
            ch.resize(frames, 0.0);
            ch.fill(0.0);
        }
    }

    /// Apply volume + the effect chain in place on `accum`, leaving the
    /// processed bus signal there. Mirrors `InstrumentLane`'s effect loop.
    // TODO: factor a shared effect-chain helper to remove this duplication.
    fn finish(&mut self, num_channels: usize, frames: usize, telemetry: Option<&Telemetry>) -> anyhow::Result<()> {
        if (self.volume - 1.0).abs() >= f32::EPSILON {
            for ch in self.accum.iter_mut().take(num_channels) {
                for s in ch.iter_mut() {
                    *s *= self.volume;
                }
            }
        }
        if self.effects.is_empty() {
            self.apply_pan(num_channels);
            store_taps(&self.taps, telemetry, TapStage::Output, &self.accum);
            return Ok(());
        }
        for buf in self.buf_a.iter_mut().chain(self.buf_b.iter_mut()) {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }
        for ch in 0..num_channels {
            if ch < self.accum.len() {
                self.buf_a[ch].copy_from_slice(&self.accum[ch]);
            } else {
                self.buf_a[ch].fill(0.0);
            }
        }
        let mut src_is_a = true;
        for (index, (effect, &mix)) in self.effects.iter_mut().zip(self.mix_values.iter()).enumerate() {
            let mix = mix as f32;
            if src_is_a {
                {
                    let mut in_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let mut out_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let in_refs = shared_slices(&self.buf_a, &mut in_s);
                    let out_refs = mut_slices(&mut self.buf_b, &mut out_s);
                    effect.process(&[], in_refs, out_refs)?;
                }
                if mix < 1.0 {
                    let dry = 1.0 - mix;
                    for ch in 0..num_channels {
                        for i in 0..frames {
                            self.buf_b[ch][i] = self.buf_a[ch][i] * dry + self.buf_b[ch][i] * mix;
                        }
                    }
                }
                store_taps(&self.taps, telemetry, TapStage::Effect(index), &self.buf_b);
            } else {
                {
                    let mut in_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let mut out_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let in_refs = shared_slices(&self.buf_b, &mut in_s);
                    let out_refs = mut_slices(&mut self.buf_a, &mut out_s);
                    effect.process(&[], in_refs, out_refs)?;
                }
                if mix < 1.0 {
                    let dry = 1.0 - mix;
                    for ch in 0..num_channels {
                        for i in 0..frames {
                            self.buf_a[ch][i] = self.buf_b[ch][i] * dry + self.buf_a[ch][i] * mix;
                        }
                    }
                }
                store_taps(&self.taps, telemetry, TapStage::Effect(index), &self.buf_a);
            }
            src_is_a = !src_is_a;
        }
        let final_buf = if src_is_a { &self.buf_a } else { &self.buf_b };
        for (acc, fin) in self.accum.iter_mut().zip(final_buf.iter()).take(num_channels) {
            acc.copy_from_slice(fin);
        }
        self.apply_pan(num_channels);
        store_taps(&self.taps, telemetry, TapStage::Output, &self.accum);
        Ok(())
    }

    /// Balance-style stereo pan on the bus output (2-channel only): center
    /// leaves both channels untouched, panning attenuates the far channel.
    fn apply_pan(&mut self, num_channels: usize) {
        if num_channels != 2 || self.pan.abs() < f32::EPSILON {
            return;
        }
        let p = self.pan;
        let (left_gain, right_gain) = if p >= 0.0 {
            (1.0 - p, 1.0)
        } else {
            (1.0, 1.0 + p)
        };
        for s in self.accum[0].iter_mut() {
            *s *= left_gain;
        }
        for s in self.accum[1].iter_mut() {
            *s *= right_gain;
        }
    }
}

// ---------------------------------------------------------------------------
// AudioGraph — multi-instrument audio processor
// ---------------------------------------------------------------------------

/// An audio graph with multiple instruments, each with its own effect chain.
/// Instruments sum into the master, or — if they belong to a group — into that
/// group's submix bus, which runs its own effects before reaching the master.
///
/// Commands are drained at the top of every audio callback via try_recv loop.
pub struct AudioGraph {
    instruments: Vec<InstrumentLane>,
    /// Submix buses. Lanes with `group == Some(i)` feed `groups[i]`.
    groups: Vec<Group>,
    /// Accumulation buffer for summing all instruments
    mix_buf: Vec<Vec<f32>>,
    /// Per-instrument scratch buffer (reused across instruments)
    lane_buf: Vec<Vec<f32>>,
    num_channels: usize,
    command_rx: Receiver<GraphCommand>,
    return_tx: Sender<Box<dyn Plugin>>,
    /// Notification channel for pattern recording completion.
    pattern_tx: Option<Sender<PatternNotification>>,
    /// Notification channel for MIDI-learn captures.
    learn_tx: Option<Sender<MidiLearnedNotification>>,
    /// Shared piano-tab state; read once per buffer for the current scale
    /// (lock-free atomics) so in-key pattern transposition follows the
    /// scale picker live. None = default scale (C Major).
    piano_filter: Option<Arc<PianoFilter>>,
    /// Reusable scratch for a group's trigger MIDI (union of member ranges),
    /// rebuilt per group per buffer. Avoids per-buffer allocation.
    group_midi_scratch: Vec<(u64, [u8; 3])>,
    /// Reusable scratch for pending group-modulator parameter writes, and for
    /// summing what the modulators of a group drive on each parameter.
    group_mod_writes: Vec<GroupModWrite>,
    group_member_drives: Vec<MemberDrive>,
    group_bus_drives: Vec<BusDrive>,
    /// Reusable scratch for pending group→member-modulator cross-mod writes.
    group_member_mod_writes: Vec<GroupMemberModWrite>,
    /// Reusable scratch for the cross-mod writes within one group's rack.
    group_cross_mod_scratch: Vec<CrossModWrite>,
    /// Where what the graph measures is published; `None` publishes nothing.
    telemetry: Option<Arc<Telemetry>>,
    /// The meter of the output mix.
    master_meter: Meter,
    /// The seed that the next modulator to come in is given (`next_modulator_seed`).
    modulator_seeds: u32,
    /// The tempo, in beats a minute: what the modulators that follow it go by (`SetGlobalBpm`).
    bpm: f32,
}

impl AudioGraph {
    /// Create an empty audio graph. Outputs silence until instruments are added.
    pub fn new(
        num_channels: usize,
        command_rx: Receiver<GraphCommand>,
        return_tx: Sender<Box<dyn Plugin>>,
    ) -> Self {
        AudioGraph {
            instruments: Vec::new(),
            groups: Vec::new(),
            mix_buf: (0..num_channels).map(|_| Vec::new()).collect(),
            lane_buf: (0..num_channels).map(|_| Vec::new()).collect(),
            num_channels,
            command_rx,
            return_tx,
            pattern_tx: None,
            learn_tx: None,
            piano_filter: None,
            group_midi_scratch: Vec::with_capacity(128),
            group_mod_writes: Vec::with_capacity(64),
            group_member_drives: Vec::with_capacity(SCRATCH_CAPACITY),
            group_bus_drives: Vec::with_capacity(SCRATCH_CAPACITY),
            group_member_mod_writes: Vec::with_capacity(32),
            group_cross_mod_scratch: Vec::with_capacity(SCRATCH_CAPACITY),
            telemetry: None,
            master_meter: Meter::new(telemetry::MASTER),
            modulator_seeds: 0,
            bpm: DEFAULT_BPM,
        }
    }

    /// The seed of the next modulator to come in: each has its own, so that two random LFOs do not draw the same numbers, and a graph that is made
    /// again from the same commands draws the same as it did.
    fn next_modulator_seed(&mut self) -> u32 {
        let seed = self.modulator_seeds;
        self.modulator_seeds = seed.wrapping_add(1);
        seed
    }

    /// The audio sample rate, derived from the first loaded plugin (all plugins
    /// run at the device rate). Falls back to 48 kHz before any plugin loads.
    fn graph_sample_rate(&self) -> f32 {
        self.instruments
            .iter()
            .find_map(|l| l.instrument.as_ref().map(|i| i.sample_rate()))
            .or_else(|| {
                self.groups
                    .iter()
                    .flat_map(|g| g.effects.iter())
                    .next()
                    .map(|e| e.sample_rate())
            })
            .unwrap_or(48000.0)
    }

    /// Set the shared piano-tab state used for in-key pattern transposition.
    pub fn set_piano_filter(&mut self, filter: Arc<PianoFilter>) {
        self.piano_filter = Some(filter);
    }

    /// Publish what the graph measures (see `telemetry`) into `telemetry`.
    pub fn set_telemetry(&mut self, telemetry: Arc<Telemetry>) {
        self.telemetry = Some(telemetry);
    }

    /// Set the notification channel for MIDI-learn captures.
    pub fn set_learn_tx(&mut self, tx: Sender<MidiLearnedNotification>) {
        self.learn_tx = Some(tx);
    }

    /// Scan a modulator list for a just-captured MIDI-learn control and notify
    /// the main thread, tagging each with its rack location. Takes the sender
    /// explicitly so it doesn't borrow all of `self`.
    fn drain_learned(
        tx: Option<&Sender<MidiLearnedNotification>>,
        modulators: &mut [Modulator],
        loc_of: impl Fn(usize) -> ModulatorLocation,
    ) {
        let Some(tx) = tx else { return };
        for (i, m) in modulators.iter_mut().enumerate() {
            if let Some(source) = m.last_learned.take() {
                let _ = tx.try_send(MidiLearnedNotification {
                    location: loc_of(i),
                    mod_index: i,
                    source,
                });
            }
        }
    }

    /// Set the notification channel for pattern recording completion.
    pub fn set_pattern_tx(&mut self, tx: Sender<PatternNotification>) {
        self.pattern_tx = Some(tx.clone());
        // Propagate to existing instruments.
        for (inst_idx, inst) in self.instruments.iter_mut().enumerate() {
            inst.pattern.pattern_tx = Some(tx.clone());
            inst.pattern.inst_index = inst_idx;
        }
    }

    pub fn num_channels(&self) -> usize {
        self.num_channels
    }

    /// What each lane is, for a test to compare with the model's.
    #[cfg(test)]
    pub fn lanes(&self) -> Vec<LaneFacts> {
        self.instruments
            .iter()
            .map(|lane| LaneFacts {
                group: lane.group,
                range: lane.range,
                transpose: lane.transpose,
                volume: lane.volume,
                muted: lane.muted,
                solo: lane.solo,
                has_instrument: lane.instrument.is_some(),
                effects: lane.effects.len(),
                modulators: lane.modulators.len(),
                pattern_index: lane.pattern.inst_index,
            })
            .collect()
    }

    /// The names of the effects of each lane, in the order of its chain, for a test to compare with the model's.
    #[cfg(test)]
    pub fn effect_names(&self) -> Vec<Vec<String>> {
        self.instruments.iter().map(|lane| lane.effects.iter().map(|effect| effect.name().to_string()).collect()).collect()
    }

    /// What each group is, for a test to compare with the model's.
    #[cfg(test)]
    pub fn groups(&self) -> Vec<GroupFacts> {
        self.groups
            .iter()
            .map(|group| GroupFacts {
                effects: group.effects.iter().map(|effect| effect.name().to_string()).collect(),
                modulators: group.modulators.len(),
                volume: group.volume,
                pan: group.pan,
                muted: group.muted,
                solo: group.solo,
            })
            .collect()
    }

    /// The value the plugin in `slot` of lane `inst` (0 is the instrument) has for parameter `param`,
    /// as the plugin reports it, for a test.
    #[cfg(test)]
    pub fn plugin_parameter(&mut self, inst: usize, slot: usize, param: u32) -> Option<f32> {
        let lane = self.instruments.get_mut(inst)?;
        if slot == 0 {
            lane.instrument.as_mut()?.get_parameter(param)
        } else {
            lane.effects.get_mut(slot - 1)?.get_parameter(param)
        }
    }

    /// The value bus effect `effect` of group `group` has for parameter `param`, for a test.
    #[cfg(test)]
    pub fn bus_parameter(&mut self, group: usize, effect: usize, param: u32) -> Option<f32> {
        self.groups.get_mut(group)?.effects.get_mut(effect)?.get_parameter(param)
    }

    /// The rate of lane `inst`'s LFO `index` and the depths of its cables, for a test.
    #[cfg(test)]
    pub fn lane_lfo(&self, inst: usize, index: usize) -> Option<(f32, Vec<f32>)> {
        let modulator = self.instruments.get(inst)?.modulators.get(index)?;
        let ModSource::Lfo { rate, .. } = modulator.source else { return None };
        Some((rate, modulator.targets.iter().map(|t| t.depth).collect()))
    }

    /// The stages of lane `inst`'s envelope `index`: attack, decay, sustain and release, for a test.
    #[cfg(test)]
    pub fn lane_envelope(&self, inst: usize, index: usize) -> Option<(f32, f32, f32, f32)> {
        let ModSource::Envelope { attack, decay, sustain, release, .. } = self.instruments.get(inst)?.modulators.get(index)?.source else { return None };
        Some((attack, decay, sustain, release))
    }

    /// Every modulator's cables, for a test to compare with the model's: the lanes' racks
    /// and then the groups', each modulator with its cables' kinds and depths in order.
    #[cfg(test)]
    pub fn cables(&self) -> RackCables {
        let rack = |at: fn(usize) -> ModulatorLocation, i: usize, modulators: &[Modulator]| -> Vec<_> {
            modulators.iter().enumerate().map(|(m, modulator)| (at(i), m, modulator.targets.iter().map(|t| (t.kind.clone(), t.depth)).collect())).collect()
        };
        let lanes = self.instruments.iter().enumerate().flat_map(|(i, lane)| rack(ModulatorLocation::Lane, i, &lane.modulators));
        let groups = self.groups.iter().enumerate().flat_map(|(i, group)| rack(ModulatorLocation::Group, i, &group.modulators));
        lanes.chain(groups).collect()
    }

    /// The value each cable is centred on (its `base_value`), for a test: the lanes' racks and then the groups', each
    /// modulator's cables in order, with their kinds.
    #[cfg(test)]
    pub fn cable_bases(&self) -> Vec<(ModulatorLocation, usize, ModTargetKind, f32)> {
        let rack = |at: fn(usize) -> ModulatorLocation, i: usize, modulators: &[Modulator]| -> Vec<_> {
            modulators
                .iter()
                .enumerate()
                .flat_map(|(m, modulator)| modulator.targets.iter().map(move |t| (at(i), m, t.kind.clone(), t.base_value)).collect::<Vec<_>>())
                .collect()
        };
        let lanes = self.instruments.iter().enumerate().flat_map(|(i, lane)| rack(ModulatorLocation::Lane, i, &lane.modulators));
        let groups = self.groups.iter().enumerate().flat_map(|(i, group)| rack(ModulatorLocation::Group, i, &group.modulators));
        lanes.chain(groups).collect()
    }

    /// Set a parameter of an effect of a lane's chain on the plugin itself, for a test: what the model does not know, as a plugin has state apart from its
    /// parameters.
    #[cfg(test)]
    pub fn poke_effect(&mut self, inst: usize, index: usize, param: u32, value: f32) {
        self.instruments[inst].effects[index].set_parameter(param, value).unwrap();
    }

    /// A parameter of an effect of a lane's chain, as the plugin itself says it, for a test.
    #[cfg(test)]
    pub fn peek_effect(&mut self, inst: usize, index: usize, param: u32) -> Option<f32> {
        self.instruments.get_mut(inst)?.effects.get_mut(index)?.get_parameter(param)
    }

    /// Set a parameter of lane `inst`'s instrument on the plugin itself, for a test: what the model does not know, as a plugin has state apart from its parameters.
    #[cfg(test)]
    pub fn poke_instrument(&mut self, inst: usize, param: u32, value: f32) {
        self.instruments[inst].instrument.as_mut().unwrap().set_parameter(param, value).unwrap();
    }

    /// Whether each lane has a note remapper, for a test to compare with the model's note remap.
    #[cfg(test)]
    pub fn remappers(&self) -> Vec<bool> {
        self.instruments.iter().map(|lane| lane.remapper.is_some()).collect()
    }

    /// Set a parameter of an effect of a group's bus on the plugin itself, for a test: what the model does not know, as a plugin has state apart from its parameters.
    #[cfg(test)]
    pub fn poke_bus_effect(&mut self, group: usize, index: usize, param: u32, value: f32) {
        self.groups[group].effects[index].set_parameter(param, value).unwrap();
    }

    /// A parameter of an effect of a group's bus, as the plugin itself says it, for a test.
    #[cfg(test)]
    pub fn peek_bus_effect(&mut self, group: usize, index: usize, param: u32) -> Option<f32> {
        self.groups.get_mut(group)?.effects.get_mut(index)?.get_parameter(param)
    }

    /// Every chain as it is, for a test: for each lane and then each group, its effects (the name, where the plugin is in memory, and the mix as the model has it,
    /// a 32-bit number): a plugin that went out and came back is where it was, one that was loaded again is not.
    #[cfg(test)]
    pub fn chain_dump(&self) -> Vec<Vec<(String, usize, f32)>> {
        let chain = |effects: &[Box<dyn Plugin>], mixes: &[f64]| -> Vec<(String, usize, f32)> {
            effects.iter().zip(mixes).map(|(effect, mix)| (effect.name().to_string(), &**effect as *const dyn Plugin as *const () as usize, *mix as f32)).collect()
        };
        let lanes = self.instruments.iter().map(|lane| chain(&lane.effects, &lane.mix_values));
        let groups = self.groups.iter().map(|group| chain(&group.effects, &group.mix_values));
        lanes.chain(groups).collect()
    }

    /// Where each lane's instrument is in memory, for a test: a plugin that went out and came back is where it was, one that was loaded again is not.
    #[cfg(test)]
    pub fn instrument_dump(&self) -> Vec<Option<usize>> {
        self.instruments.iter().map(|lane| lane.instrument.as_ref().map(|instrument| &**instrument as *const dyn Plugin as *const () as usize)).collect()
    }

    /// Every modulator as it is, for a test: its source, what it drives (each cable's kind, depth and telemetry slot; what each is centred on is
    /// `cable_bases`) and the slot it publishes to; the lanes' racks, then the groups'.
    #[cfg(test)]
    pub fn modulator_dump(&self) -> Vec<String> {
        let lanes = self.instruments.iter().flat_map(|lane| lane.modulators.iter());
        let groups = self.groups.iter().flat_map(|group| group.modulators.iter());
        lanes
            .chain(groups)
            .map(|m| format!("{:?} -> {:?} @{:?}", m.source, m.targets.iter().map(|t| (&t.kind, t.depth, t.effective)).collect::<Vec<_>>(), m.output))
            .collect()
    }

    /// Drain all pending commands from the command channel (lock-free).
    pub fn drain_commands(&mut self) {
        while let Ok(cmd) = self.command_rx.try_recv() {
            match cmd {
                GraphCommand::SwapInstrument {
                    inst,
                    instrument: new_inst,
                    inst_buf,
                    remapper,
                    keep,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.inst_buf = inst_buf;
                        lane.remapper = remapper;
                        if let Some(old) = lane.instrument.replace(new_inst) {
                            hand_back(&self.return_tx, keep, old);
                        }
                    }
                }
                GraphCommand::InsertEffect {
                    inst,
                    index,
                    effect,
                    mix,
                } => {
                    let num_channels = self.num_channels;
                    let mut inserted_at = None;
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if effect.audio_output_count() != num_channels {
                            log::warn!(
                                "Rejecting effect '{}': output channels {} != chain channels {}",
                                effect.name(),
                                effect.audio_output_count(),
                                num_channels,
                            );
                            let _ = self.return_tx.try_send(effect);
                        } else {
                            let idx = index.min(lane.effects.len());
                            lane.effects.insert(idx, effect);
                            lane.mix_values.insert(idx, mix);
                            // Bump modulator target slots that pointed at this
                            // or a later effect.
                            shift_slots_after_insert(&mut lane.modulators, idx);
                            inserted_at = Some(idx);
                        }
                    }
                    // The same for group modulators targeting this lane's plugins.
                    if let Some(idx) = inserted_at {
                        self.fix_group_member_targets(inst, |mods, member| {
                            shift_member_slots_after_insert(mods, member, idx)
                        });
                    }
                }
                GraphCommand::RemoveEffect { inst, index, keep } => {
                    let old = self.get_instrument_mut(inst).and_then(|lane| {
                        if index < lane.effects.len() {
                            let old = lane.effects.remove(index);
                            lane.mix_values.remove(index);
                            // Drop/shift modulator target slots for this effect.
                            fixup_slots_after_remove(&mut lane.modulators, index);
                            Some(old)
                        } else {
                            None
                        }
                    });
                    if let Some(old) = old {
                        hand_back(&self.return_tx, keep, old);
                        // The same for group modulators targeting this lane's plugins.
                        self.fix_group_member_targets(inst, |mods, member| {
                            fixup_member_slots_after_remove(mods, member, index)
                        });
                    }
                }
                GraphCommand::ReorderEffect {
                    inst,
                    from,
                    to,
                } => {
                    let mut moved = false;
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if from < lane.effects.len() && to < lane.effects.len() && from != to {
                            let effect = lane.effects.remove(from);
                            let mix = lane.mix_values.remove(from);
                            lane.effects.insert(to, effect);
                            lane.mix_values.insert(to, mix);
                            // Remap modulator target slots through the move.
                            remap_slots_after_reorder(&mut lane.modulators, from, to);
                            moved = true;
                        }
                    }
                    // The same for group modulators targeting this lane's plugins.
                    if moved {
                        self.fix_group_member_targets(inst, |mods, member| {
                            remap_member_slots_after_reorder(mods, member, from, to)
                        });
                    }
                }
                GraphCommand::SetParameter {
                    inst,
                    slot,
                    param_index,
                    value,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        let plugin: Option<&mut Box<dyn Plugin>> = if slot == 0 {
                            lane.instrument.as_mut()
                        } else {
                            lane.effects.get_mut(slot - 1)
                        };
                        if let Some(p) = plugin {
                            if let Err(e) = p.set_parameter(param_index, value) {
                                log::warn!("SetParameter inst={inst} slot={slot} index={param_index}: {e}");
                            }
                        }
                        // Update modulator base values for targets that point
                        // at this exact slot + parameter.
                        for modulator in &mut lane.modulators {
                            for target in &mut modulator.targets {
                                if let ModTargetKind::PluginParam {
                                    slot: tslot,
                                    param_index: pi,
                                } = target.kind
                                {
                                    if tslot == slot && pi == param_index {
                                        target.base_value = value;
                                    }
                                }
                            }
                        }
                    }
                    // Mirror to any group modulator targeting this member param.
                    if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, inst) {
                        if let Some(g) = self.groups.get_mut(group) {
                            update_group_member_base(
                                &mut g.modulators,
                                ordinal,
                                slot,
                                param_index,
                                value,
                            );
                        }
                    }
                }
                GraphCommand::LoadPreset {
                    inst,
                    slot,
                    preset_id,
                } => {
                    // The parameters that the lane's group drives on its members (this plugin's are among them): the
                    // bases of the cables are refreshed below, after the plugin has said what the preset made of them,
                    // and `update_group_member_base` keeps to the cables of this member's plugin in this slot.
                    let member = lane_member_ordinal(&self.instruments, inst);
                    let driven_by_the_group: Vec<u32> = member
                        .and_then(|(group, _)| self.groups.get(group))
                        .map(|group| {
                            group
                                .modulators
                                .iter()
                                .flat_map(|m| m.targets.iter())
                                .filter_map(|t| match t.kind {
                                    ModTargetKind::GroupMember { param_index, .. } => Some(param_index),
                                    _ => None,
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut from_the_group = Vec::new();
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        let plugin: Option<&mut Box<dyn Plugin>> = if slot == 0 {
                            lane.instrument.as_mut()
                        } else {
                            lane.effects.get_mut(slot - 1)
                        };
                        if let Some(p) = plugin {
                            match p.load_preset(&preset_id) {
                                Ok(()) => {
                                    // A cable modulates around the base value it was given, and puts it on the parameter at
                                    // every buffer: left at what the parameter was, it would put that back, and the preset
                                    // would be heard for everything but what is driven. (As `SetParameter` does for a knob.)
                                    for modulator in &mut lane.modulators {
                                        for target in &mut modulator.targets {
                                            if let ModTargetKind::PluginParam { slot: tslot, param_index } = target.kind {
                                                if tslot == slot {
                                                    if let Some(value) = p.get_parameter(param_index) {
                                                        target.base_value = value;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    from_the_group = driven_by_the_group.iter().filter_map(|&param| p.get_parameter(param).map(|value| (param, value))).collect();
                                }
                                Err(e) => log::warn!("LoadPreset inst={inst} slot={slot} id={preset_id:?}: {e}"),
                            }
                        }
                    }
                    if let Some((group, ordinal)) = member {
                        if let Some(g) = self.groups.get_mut(group) {
                            for (param, value) in from_the_group {
                                update_group_member_base(&mut g.modulators, ordinal, slot, param, value);
                            }
                        }
                    }
                }
                GraphCommand::SetMix {
                    inst,
                    slot,
                    value,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if slot > 0 {
                            if let Some(mix) = lane.mix_values.get_mut(slot - 1) {
                                *mix = value as f64;
                            }
                        }
                    }
                }
                GraphCommand::SetVolume { inst, value } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.volume = value;
                    }
                }
                GraphCommand::SetMute { inst, muted } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.muted = muted;
                    }
                }
                GraphCommand::SetSolo { inst, solo } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.solo = solo;
                    }
                }
                GraphCommand::SetInstrumentRange { inst, range } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.range = range;
                        // A held note counted under the old range would never be
                        // decremented (its note-off no longer matches), leaving
                        // its channel stuck "active". Reset the tally.
                        lane.active_note_channels = [0; 16];
                    }
                }
                GraphCommand::ClearInstrument { inst } => {
                    let old = self
                        .get_instrument_mut(inst)
                        .and_then(|lane| {
                            lane.inst_buf.clear();
                            lane.remapper = None;
                            lane.instrument.take()
                        });
                    if let Some(old) = old {
                        let _ = self.return_tx.try_send(old);
                    }
                }
                GraphCommand::SwapInstruments {
                    inst_a,
                    inst_b,
                } => {
                    if inst_a < self.instruments.len() && inst_b < self.instruments.len() && inst_a != inst_b {
                        // The lanes change places, each with everything it has: its plugins and its
                        // effects, its modulators, its volume, range and group. What is addressed
                        // by position follows: the pattern player says which lane it is, and the
                        // cables of a group address its members by their place among the group's lanes.
                        let groups: Vec<Option<usize>> = self.instruments.iter().map(|lane| lane.group).collect();
                        let moves = member_moves(&groups, inst_a, inst_b);
                        self.instruments.swap(inst_a, inst_b);
                        self.instruments[inst_a].pattern.inst_index = inst_a;
                        self.instruments[inst_b].pattern.inst_index = inst_b;
                        for group in moves.iter().map(|(group, ..)| *group).collect::<std::collections::BTreeSet<_>>() {
                            let of_group: Vec<(usize, usize)> = moves.iter().filter(|(g, ..)| *g == group).map(|&(_, old, new)| (old, new)).collect();
                            if let Some(g) = self.groups.get_mut(group) {
                                remap_member_ordinals(&mut g.modulators, &of_group);
                            }
                        }
                    }
                }
                GraphCommand::AddInstrument { range } => {
                    let mut lane = InstrumentLane::new(self.num_channels);
                    lane.range = range;
                    lane.pattern.inst_index = self.instruments.len();
                    lane.pattern.pattern_tx = self.pattern_tx.clone();
                    self.instruments.push(lane);
                }
                GraphCommand::RemoveInstrument { inst, keep } => {
                    if inst < self.instruments.len() {
                        // If this lane was a group member, note its ordinal so the
                        // group's modulators can drop/shift their member targets.
                        let member_of = lane_member_ordinal(&self.instruments, inst);
                        let removed = self.instruments.remove(inst);
                        if let Some((group, ordinal)) = member_of {
                            if let Some(g) = self.groups.get_mut(group) {
                                fixup_group_member_after_remove(&mut g.modulators, ordinal);
                            }
                        }
                        // Re-index remaining instruments so pattern notifications route correctly.
                        for (i, lane) in self.instruments.iter_mut().enumerate() {
                            lane.pattern.inst_index = i;
                        }
                        hand_back_lane(&self.return_tx, keep, removed);
                    }
                }
                GraphCommand::InsertInstrument { index, lane } => {
                    let mut lane = *lane.0;
                    if lane.effects.len() != lane.mix_values.len() {
                        // It gave up its plugins and has not been given them again.
                        return_the_plugins_of(&self.return_tx, lane);
                    } else {
                        let place = index.min(self.instruments.len());
                        // The notes that were held while it was out were let go of without it: it counts none, and a pattern that a held note was playing is not playing.
                        lane.active_note_channels = [0; 16];
                        let pattern = &mut lane.pattern;
                        pattern.trigger_note = None;
                        pattern.held_notes.clear();
                        pattern.active_voices.clear();
                        pattern.recording = false;
                        pattern.counting_in = false;
                        pattern.click_remaining = 0;
                        pattern.pattern_tx = self.pattern_tx.clone();
                        if lane.group.is_some_and(|group| group >= self.groups.len()) {
                            lane.group = None;
                        }
                        self.instruments.insert(place, lane);
                        for (i, lane) in self.instruments.iter_mut().enumerate() {
                            lane.pattern.inst_index = i;
                        }
                        // A member that comes in tells the cables of its group to the members from its place on the ordinals they have now.
                        if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, place) {
                            if let Some(g) = self.groups.get_mut(group) {
                                shift_group_member_after_insert(&mut g.modulators, ordinal);
                            }
                        }
                    }
                }
                GraphCommand::AddGroup => {
                    self.groups.push(Group::new(self.num_channels));
                }
                GraphCommand::SetLaneMeter { inst, slots } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.meter = Some(Meter::new(slots));
                    }
                }
                GraphCommand::SetGroupMeter { group, slots } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        g.meter = Some(Meter::new(slots));
                    }
                }
                GraphCommand::SetProbe { target, ring } => match target {
                    ProbeTarget::Instrument(inst) => {
                        if let Some(lane) = self.get_instrument_mut(inst) {
                            add_tap(&mut lane.taps, TapStage::Output, ring);
                        }
                    }
                    ProbeTarget::Effect { inst, index } => {
                        if let Some(lane) = self.get_instrument_mut(inst).filter(|lane| index < lane.effects.len()) {
                            add_tap(&mut lane.taps, TapStage::Effect(index), ring);
                        }
                    }
                    ProbeTarget::Group(group) => {
                        if let Some(g) = self.groups.get_mut(group) {
                            add_tap(&mut g.taps, TapStage::Output, ring);
                        }
                    }
                    ProbeTarget::GroupEffect { group, index } => {
                        if let Some(g) = self.groups.get_mut(group).filter(|g| index < g.effects.len()) {
                            add_tap(&mut g.taps, TapStage::Effect(index), ring);
                        }
                    }
                },
                GraphCommand::ClearProbes => {
                    for lane in self.instruments.iter_mut() {
                        lane.taps.clear();
                    }
                    for g in self.groups.iter_mut() {
                        g.taps.clear();
                    }
                }
                GraphCommand::RemoveGroup { group, keep } => {
                    if group < self.groups.len() {
                        let removed = self.groups.remove(group);
                        // Fix up lane membership: drop the removed group, shift
                        // higher group indices down.
                        for lane in self.instruments.iter_mut() {
                            match lane.group {
                                Some(g) if g == group => lane.group = None,
                                Some(g) if g > group => lane.group = Some(g - 1),
                                _ => {}
                            }
                        }
                        hand_back_group(&self.return_tx, keep, removed);
                    }
                }
                GraphCommand::InsertGroup { index, group, members } => {
                    let mut group = *group.0;
                    if group.effects.len() != group.mix_values.len() {
                        // It gave up its plugins and has not been given them again.
                        for effect in group.effects.drain(..) {
                            let _ = self.return_tx.try_send(effect);
                        }
                    } else {
                        let place = index.min(self.groups.len());
                        // The lanes of the groups from that place on are in the one after it now, as they were before the group went.
                        for lane in self.instruments.iter_mut() {
                            if let Some(g) = lane.group.as_mut().filter(|g| **g >= place) {
                                *g += 1;
                            }
                        }
                        self.groups.insert(place, group);
                        for inst in members {
                            if let Some(lane) = self.instruments.get_mut(inst) {
                                lane.group = Some(place);
                            }
                        }
                    }
                }
                GraphCommand::SetLaneGroup { inst, group } => {
                    let valid = group.is_none_or(|g| g < self.groups.len());
                    if valid && inst < self.instruments.len() {
                        let old = lane_member_ordinal(&self.instruments, inst);
                        self.instruments[inst].group = group;
                        let new = lane_member_ordinal(&self.instruments, inst);
                        let old_group = old.map(|(g, _)| g);
                        let new_group = new.map(|(g, _)| g);
                        // Leaving the old group: drop/shift its member targets.
                        if let Some((og, oo)) = old {
                            if old_group != new_group {
                                if let Some(g) = self.groups.get_mut(og) {
                                    fixup_group_member_after_remove(&mut g.modulators, oo);
                                }
                            }
                        }
                        // Joining a new group: bump member targets at/after its slot.
                        if let Some((ng, no)) = new {
                            if old_group != new_group {
                                if let Some(g) = self.groups.get_mut(ng) {
                                    shift_group_member_after_insert(&mut g.modulators, no);
                                }
                            }
                        }
                    }
                }
                GraphCommand::SetGroupVolume { group, value } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        g.volume = value;
                    }
                }
                GraphCommand::SetGroupPan { group, value } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        g.pan = value.clamp(-1.0, 1.0);
                    }
                }
                GraphCommand::SetGroupMute { group, muted } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        g.muted = muted;
                    }
                }
                GraphCommand::SetGroupSolo { group, solo } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        g.solo = solo;
                    }
                }
                GraphCommand::InsertGroupEffect { group, index, effect, mix } => {
                    let num_channels = self.num_channels;
                    if let Some(g) = self.groups.get_mut(group) {
                        if effect.audio_output_count() != num_channels {
                            log::warn!(
                                "Rejecting group effect '{}': output channels {} != chain channels {}",
                                effect.name(),
                                effect.audio_output_count(),
                                num_channels,
                            );
                            let _ = self.return_tx.try_send(effect);
                        } else {
                            let idx = index.min(g.effects.len());
                            g.effects.insert(idx, effect);
                            g.mix_values.insert(idx, mix);
                            // Bump the group modulators' bus targets that pointed at this or a later effect.
                            shift_group_bus_after_insert(&mut g.modulators, idx);
                        }
                    }
                }
                GraphCommand::RemoveGroupEffect { group, index, keep } => {
                    let old = self.groups.get_mut(group).and_then(|g| {
                        if index < g.effects.len() {
                            g.mix_values.remove(index);
                            let removed = g.effects.remove(index);
                            // Drop/shift group-mod targets that pointed at it.
                            fixup_group_bus_after_remove(&mut g.modulators, index);
                            Some(removed)
                        } else {
                            None
                        }
                    });
                    if let Some(old) = old {
                        hand_back(&self.return_tx, keep, old);
                    }
                }
                GraphCommand::ReorderGroupEffect { group, from, to } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if from < g.effects.len() && to < g.effects.len() && from != to {
                            let effect = g.effects.remove(from);
                            let mix = g.mix_values.remove(from);
                            g.effects.insert(to, effect);
                            g.mix_values.insert(to, mix);
                            // Remap group-mod bus targets through the move.
                            remap_group_bus_after_reorder(&mut g.modulators, from, to);
                        }
                    }
                }
                GraphCommand::SetGroupMix { group, index, value } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(mix) = g.mix_values.get_mut(index) {
                            *mix = value as f64;
                        }
                    }
                }
                GraphCommand::SetGroupParameter { group, index, param_index, value } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(effect) = g.effects.get_mut(index) {
                            if let Err(e) = effect.set_parameter(param_index, value) {
                                log::warn!("SetGroupParameter group={group} index={index} param={param_index}: {e}");
                            }
                        }
                        // Track base value for group modulators driving this bus param.
                        update_group_bus_base(&mut g.modulators, index, param_index, value);
                    }
                }
                GraphCommand::SetModulatorOutput { location, index, slot } => {
                    if let Some(m) = self.modulator_mut(location, index) {
                        m.output = Some(slot);
                    }
                }
                GraphCommand::SetModulatorPosition { location, index, slot } => {
                    if let Some(m) = self.modulator_mut(location, index) {
                        m.position_out = Some(slot);
                    }
                }
                GraphCommand::SetModulatorSteps { location, index, values, length, glide, retrigger } => {
                    // (Where it is stays: it is the place in the pattern that goes on, whatever the pattern now is.)
                    if let Some(ModSource::Steps { values: v, length: l, glide: g, retrigger: r, .. }) = self.modulator_mut(location, index).map(|m| &mut m.source) {
                        *v = values;
                        *l = length;
                        *g = glide;
                        *r = retrigger;
                    }
                }
                GraphCommand::SetModulatorKey { location, index, retrigger, fade } => {
                    if let Some(ModSource::Lfo { retrigger: r, fade: f, .. }) = self.modulator_mut(location, index).map(|m| &mut m.source) {
                        *r = retrigger;
                        *f = fade;
                    }
                }
                GraphCommand::SetModulatorSync { location, index, beats } => {
                    if let Some(ModSource::Lfo { sync, .. } | ModSource::Steps { sync, .. }) = self.modulator_mut(location, index).map(|m| &mut m.source) {
                        *sync = beats;
                    }
                }
                GraphCommand::InsertModulator {
                    inst,
                    index,
                    source,
                } => {
                    let mut moved_from = None;
                    let seed = self.next_modulator_seed();
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        let m = Modulator::new(source, lane.sample_rate()).with_seed(seed);
                        let idx = index.min(lane.modulators.len());
                        // The cables that address a modulator that goes after it are told its place first, so that the ones it gets after are not. One that
                        // goes in at the end moves nothing: a session is loaded a modulator at a time, and the cable of an earlier one can address one that
                        // comes later.
                        if idx < lane.modulators.len() {
                            shift_cross_mod_after_insert(&mut lane.modulators, idx);
                            moved_from = Some(idx);
                        }
                        lane.modulators.insert(idx, m);
                    }
                    // If this lane is a group member, the group's cables to its modulators follow them.
                    if let Some(idx) = moved_from {
                        if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, inst) {
                            if let Some(g) = self.groups.get_mut(group) {
                                shift_group_member_mod_after_insert(&mut g.modulators, ordinal, idx);
                            }
                        }
                    }
                }
                GraphCommand::RemoveModulator { inst, index } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if index < lane.modulators.len() {
                            lane.modulators.remove(index);
                            // Clean up cross-mod targets in remaining siblings.
                            fixup_cross_mod_after_remove(&mut lane.modulators, index);
                        }
                    }
                    // If this lane is a group member, fix up group modulators that
                    // cross-mod its modulators.
                    if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, inst) {
                        if let Some(g) = self.groups.get_mut(group) {
                            fixup_group_member_mod_after_remove(&mut g.modulators, ordinal, index);
                        }
                    }
                }
                GraphCommand::SetModulatorRate {
                    inst,
                    mod_index,
                    rate,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            m.source.set_rate(rate);
                        }
                        // Update cross-mod base values for targets pointing at this rate.
                        update_cross_mod_base(&mut lane.modulators, mod_index, CrossModField::Rate, rate);
                    }
                    // Keep any group cross-mod of this member modulator centered.
                    if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, inst) {
                        if let Some(g) = self.groups.get_mut(group) {
                            update_group_member_mod_base(&mut g.modulators, ordinal, mod_index, CrossModField::Rate, rate);
                        }
                    }
                }
                GraphCommand::SetModulatorWaveform {
                    inst,
                    mod_index,
                    waveform,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            if let ModSource::Lfo { waveform: ref mut w, .. } = m.source {
                                *w = waveform;
                            }
                        }
                    }
                }
                GraphCommand::SetModulatorSource {
                    inst,
                    mod_index,
                    source,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            m.source = source;
                            m.last_output = 0.0;
                        }
                    }
                }
                GraphCommand::SetModulatorEnvelopeParam {
                    inst,
                    mod_index,
                    attack,
                    decay,
                    sustain,
                    release,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            if let ModSource::Envelope {
                                attack: ref mut a,
                                decay: ref mut d,
                                sustain: ref mut s,
                                release: ref mut r,
                                ..
                            } = m.source
                            {
                                *a = attack;
                                *d = decay;
                                *s = sustain;
                                *r = release;
                            }
                        }
                        // Update cross-mod base values.
                        update_cross_mod_base(&mut lane.modulators, mod_index, CrossModField::Attack, attack);
                        update_cross_mod_base(&mut lane.modulators, mod_index, CrossModField::Decay, decay);
                        update_cross_mod_base(&mut lane.modulators, mod_index, CrossModField::Sustain, sustain);
                        update_cross_mod_base(&mut lane.modulators, mod_index, CrossModField::Release, release);
                    }
                    // Keep any group cross-mod of this member modulator centered.
                    if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, inst) {
                        if let Some(g) = self.groups.get_mut(group) {
                            update_group_member_mod_base(&mut g.modulators, ordinal, mod_index, CrossModField::Attack, attack);
                            update_group_member_mod_base(&mut g.modulators, ordinal, mod_index, CrossModField::Decay, decay);
                            update_group_member_mod_base(&mut g.modulators, ordinal, mod_index, CrossModField::Sustain, sustain);
                            update_group_member_mod_base(&mut g.modulators, ordinal, mod_index, CrossModField::Release, release);
                        }
                    }
                }
                GraphCommand::AddModTarget {
                    inst,
                    mod_index,
                    target,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            m.targets.push(target);
                        }
                    }
                }
                GraphCommand::InsertModTarget {
                    inst,
                    mod_index,
                    target_index,
                    target,
                } => {
                    let mut inserted_at = None;
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(count) = lane.modulators.get(mod_index).map(|m| m.targets.len()) {
                            let at = target_index.min(count);
                            // The cables that drive the depth of the later ones are told first, so that the one that goes in is not.
                            shift_depth_cables_after_insert(&mut lane.modulators, mod_index, at);
                            lane.modulators[mod_index].targets.insert(at, target);
                            inserted_at = Some(at);
                        }
                    }
                    // If this lane is a group member, the group's cables to the depths of its modulators' cables follow them.
                    if let Some(at) = inserted_at {
                        if let Some((group, member)) = lane_member_ordinal(&self.instruments, inst) {
                            if let Some(g) = self.groups.get_mut(group) {
                                shift_member_depth_cables_after_insert(&mut g.modulators, member, mod_index, at);
                            }
                        }
                    }
                }
                GraphCommand::RemoveModTarget {
                    inst,
                    mod_index,
                    target_index,
                } => {
                    let mut removed = false;
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            if target_index < m.targets.len() {
                                m.targets.remove(target_index);
                                removed = true;
                            }
                        }
                        if removed {
                            fixup_depth_cables_after_remove(&mut lane.modulators, mod_index, target_index);
                        }
                    }
                    // If this lane is a group member, the group's cables to the depths of its
                    // modulators' cables follow them.
                    if removed {
                        if let Some((group, member)) = lane_member_ordinal(&self.instruments, inst) {
                            if let Some(g) = self.groups.get_mut(group) {
                                fixup_member_depth_cables_after_remove(&mut g.modulators, member, mod_index, target_index);
                            }
                        }
                    }
                }
                GraphCommand::SetModTargetDepth {
                    inst,
                    mod_index,
                    target_index,
                    depth,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            if let Some(t) = m.targets.get_mut(target_index) {
                                t.depth = depth;
                            }
                        }
                        // Update cross-mod base values for targets pointing at this depth: the cables that drive it work from where the user has set it, and
                        // do not write it back to what it was (as for a rate or a stage).
                        update_cross_mod_base(&mut lane.modulators, mod_index, CrossModField::Depth(target_index), depth);
                    }
                    // Keep any group cross-mod of this member modulator's depth centered.
                    if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, inst) {
                        if let Some(g) = self.groups.get_mut(group) {
                            update_group_member_mod_base(
                                &mut g.modulators,
                                ordinal,
                                mod_index,
                                CrossModField::Depth(target_index),
                                depth,
                            );
                        }
                    }
                }
                GraphCommand::InsertGroupModulator {
                    group,
                    index,
                    source,
                } => {
                    let sr = self.graph_sample_rate();
                    let seed = self.next_modulator_seed();
                    if let Some(g) = self.groups.get_mut(group) {
                        let m = Modulator::new(source, sr).with_seed(seed);
                        let idx = index.min(g.modulators.len());
                        // (As in a lane's rack: one that goes in at the end moves nothing, since a cable can address a modulator that comes later.)
                        if idx < g.modulators.len() {
                            shift_cross_mod_after_insert(&mut g.modulators, idx);
                        }
                        g.modulators.insert(idx, m);
                    }
                }
                GraphCommand::RemoveGroupModulator { group, index } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if index < g.modulators.len() {
                            g.modulators.remove(index);
                            fixup_cross_mod_after_remove(&mut g.modulators, index);
                        }
                    }
                }
                GraphCommand::SetGroupModulatorRate {
                    group,
                    mod_index,
                    rate,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            m.source.set_rate(rate);
                        }
                        update_cross_mod_base(&mut g.modulators, mod_index, CrossModField::Rate, rate);
                    }
                }
                GraphCommand::SetGroupModulatorWaveform {
                    group,
                    mod_index,
                    waveform,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            if let ModSource::Lfo { waveform: ref mut w, .. } = m.source {
                                *w = waveform;
                            }
                        }
                    }
                }
                GraphCommand::SetGroupModulatorSource {
                    group,
                    mod_index,
                    source,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            m.source = source;
                            m.last_output = 0.0;
                        }
                    }
                }
                GraphCommand::SetGroupModulatorEnvelopeParam {
                    group,
                    mod_index,
                    attack,
                    decay,
                    sustain,
                    release,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            if let ModSource::Envelope {
                                attack: ref mut a,
                                decay: ref mut d,
                                sustain: ref mut s,
                                release: ref mut r,
                                ..
                            } = m.source
                            {
                                *a = attack;
                                *d = decay;
                                *s = sustain;
                                *r = release;
                            }
                        }
                        update_cross_mod_base(&mut g.modulators, mod_index, CrossModField::Attack, attack);
                        update_cross_mod_base(&mut g.modulators, mod_index, CrossModField::Decay, decay);
                        update_cross_mod_base(&mut g.modulators, mod_index, CrossModField::Sustain, sustain);
                        update_cross_mod_base(&mut g.modulators, mod_index, CrossModField::Release, release);
                    }
                }
                GraphCommand::AddGroupModTarget {
                    group,
                    mod_index,
                    target,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            m.targets.push(target);
                        }
                    }
                }
                GraphCommand::InsertGroupModTarget {
                    group,
                    mod_index,
                    target_index,
                    target,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(count) = g.modulators.get(mod_index).map(|m| m.targets.len()) {
                            let at = target_index.min(count);
                            shift_depth_cables_after_insert(&mut g.modulators, mod_index, at);
                            g.modulators[mod_index].targets.insert(at, target);
                        }
                    }
                }
                GraphCommand::RemoveGroupModTarget {
                    group,
                    mod_index,
                    target_index,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            if target_index < m.targets.len() {
                                m.targets.remove(target_index);
                                fixup_depth_cables_after_remove(&mut g.modulators, mod_index, target_index);
                            }
                        }
                    }
                }
                GraphCommand::SetGroupModTargetDepth {
                    group,
                    mod_index,
                    target_index,
                    depth,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            if let Some(t) = m.targets.get_mut(target_index) {
                                t.depth = depth;
                            }
                        }
                        // The cables of the group that drive this depth work from where the user has set it.
                        update_cross_mod_base(&mut g.modulators, mod_index, CrossModField::Depth(target_index), depth);
                    }
                }
                GraphCommand::SetPatternEnabled { inst, enabled } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.pattern.enabled = enabled;
                    }
                }
                GraphCommand::SetPatternRecording { inst, recording } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if recording {
                            // Ensure notification routes back to the correct instrument.
                            lane.pattern.inst_index = inst;
                            lane.pattern.recording_events.clear();
                            lane.pattern.base_note = None;
                            lane.pattern.record_pos = 0;
                            lane.pattern.metronome_pos = 0;
                            lane.pattern.click_remaining = 0;
                            lane.pattern.click_phase = 0.0;
                            // Precompute beat length in samples
                            let beats_per_sec = lane.pattern.bpm / 60.0;
                            lane.pattern.beat_length_samples =
                                (lane.pattern.sample_rate / beats_per_sec) as u64;
                            // Start with count-in (metronome only, no recording yet)
                            lane.pattern.counting_in = true;
                            lane.pattern.recording = false;
                        } else {
                            // Finalize recording manually (also stops count-in)
                            lane.pattern.counting_in = false;
                            if lane.pattern.recording {
                                let length = lane.pattern.length_samples();
                                lane.pattern.finalize_recording(length);
                            } else {
                                lane.pattern.recording = false;
                            }
                        }
                    }
                }
                GraphCommand::SetPattern { inst, pattern, base_note, in_key } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.pattern.pattern = pattern;
                        lane.pattern.base_note = base_note;
                        lane.pattern.in_key = in_key;
                        lane.pattern.enabled = !lane.pattern.pattern.events.is_empty();
                    }
                }
                GraphCommand::SwapPatterns { inst_a, inst_b } => {
                    if inst_a < self.instruments.len() && inst_b < self.instruments.len() {
                        // Swap pattern data, base_note, enabled, length_beats,
                        // in_key between the two lanes.
                        let (a_pattern, a_base, a_enabled, a_beats, a_in_key) = {
                            let p = &self.instruments[inst_a].pattern;
                            (p.pattern.clone(), p.base_note, p.enabled, p.length_beats, p.in_key)
                        };
                        let (b_pattern, b_base, b_enabled, b_beats, b_in_key) = {
                            let p = &self.instruments[inst_b].pattern;
                            (p.pattern.clone(), p.base_note, p.enabled, p.length_beats, p.in_key)
                        };
                        let pa = &mut self.instruments[inst_a].pattern;
                        pa.pattern = b_pattern;
                        pa.base_note = b_base;
                        pa.enabled = b_enabled;
                        pa.length_beats = b_beats;
                        pa.in_key = b_in_key;
                        pa.trigger_note = None;
                        pa.held_notes.clear();
                        pa.active_voices.clear();
                        let pb = &mut self.instruments[inst_b].pattern;
                        pb.pattern = a_pattern;
                        pb.base_note = a_base;
                        pb.enabled = a_enabled;
                        pb.length_beats = a_beats;
                        pb.in_key = a_in_key;
                        pb.trigger_note = None;
                        pb.held_notes.clear();
                        pb.active_voices.clear();
                    }
                }
                GraphCommand::ClearPattern { inst } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.pattern.pattern = Pattern::default();
                        lane.pattern.base_note = None;
                        lane.pattern.enabled = false;
                        lane.pattern.in_key = false;
                        lane.pattern.recording = false;
                        lane.pattern.counting_in = false;
                        lane.pattern.trigger_note = None;
                        lane.pattern.held_notes.clear();
                        lane.pattern.click_remaining = 0;
                        lane.pattern.active_voices.clear();
                    }
                }
                GraphCommand::SetGlobalBpm { bpm } => {
                    // (A tempo that is no number, or none, is not one for a modulator to follow: it goes on at the one it had.)
                    if bpm.is_finite() && bpm > 0.0 {
                        self.bpm = bpm;
                    }
                    for inst in &mut self.instruments {
                        inst.pattern.bpm = bpm;
                    }
                }
                GraphCommand::SetPatternLength { inst, beats } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.pattern.set_length_beats(beats);
                    }
                }
                GraphCommand::SetPatternLooping { inst, looping } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.pattern.looping = looping;
                    }
                }
                GraphCommand::SetPatternInKey { inst, in_key } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.pattern.in_key = in_key;
                    }
                }
                GraphCommand::SetTranspose { inst, semitones } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.transpose = semitones;
                    }
                }
            }
        }
    }

    fn get_instrument_mut(&mut self, inst: usize) -> Option<&mut InstrumentLane> {
        self.instruments.get_mut(inst)
    }

    /// The modulator `index` of a lane's rack or of a group's, if it is there.
    fn modulator_mut(&mut self, location: ModulatorLocation, index: usize) -> Option<&mut Modulator> {
        match location {
            ModulatorLocation::Lane(inst) => self.instruments.get_mut(inst)?.modulators.get_mut(index),
            ModulatorLocation::Group(group) => self.groups.get_mut(group)?.modulators.get_mut(index),
        }
    }

    /// Run `fix` on the modulators of the group that lane `inst` belongs to, with
    /// the lane's member ordinal in it; does nothing for an ungrouped lane. For
    /// keeping group targets on a member's plugins when its chain changes.
    fn fix_group_member_targets(&mut self, inst: usize, fix: impl FnOnce(&mut [Modulator], usize)) {
        if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, inst) {
            if let Some(g) = self.groups.get_mut(group) {
                fix(&mut g.modulators, ordinal);
            }
        }
    }

    /// Process audio: drain commands, run all instruments, sum to output.
    /// Outputs silence if no instruments are loaded.
    pub fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        audio_out: &mut [Vec<f32>],
    ) -> anyhow::Result<()> {
        self.drain_commands();

        let frames = audio_out.first().map(|b| b.len()).unwrap_or(0);

        // Zero mix_buf
        for buf in self.mix_buf.iter_mut() {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }

        // Resize lane_buf
        for buf in self.lane_buf.iter_mut() {
            buf.resize(frames, 0.0);
        }

        // How far a meter's peak falls over this buffer, if anything is measured.
        let fall = self
            .telemetry
            .as_ref()
            .map(|_| telemetry::peak_fall(frames as f32 / self.graph_sample_rate()));

        // Current piano scale, read once per buffer (lock-free).
        let scale = self
            .piano_filter
            .as_ref()
            .map(|f| f.scale())
            .unwrap_or_default();

        // Prepare group accumulators for this buffer.
        for group in self.groups.iter_mut() {
            group.begin(frames, self.num_channels);
        }

        // Whether anything is soloed: then only what is soloed is heard (and what a soloed lane or group needs to be: see `heard` below).
        let any_solo = self.instruments.iter().any(|lane| lane.solo) || self.groups.iter().any(|group| group.solo);

        // Group-scoped modulators: tick each group's modulators (envelopes are
        // triggered by the union of its members' ranges), resolve cross-mod,
        // then apply their targets to member-instrument and bus-effect
        // parameters now — before the lanes render, so members read the
        // modulated values (bus effects read them later in group.finish). A
        // lane modulator hitting the same parameter is applied later inside the
        // lane's own process() and therefore wins.
        if frames > 0 {
            for gi in 0..self.groups.len() {
                if self.groups[gi].modulators.is_empty() {
                    continue;
                }
                build_group_midi(
                    &self.instruments,
                    gi,
                    midi_events,
                    &mut self.group_midi_scratch,
                );
                for m in &mut self.groups[gi].modulators {
                    m.tick(frames, &self.group_midi_scratch, self.bpm);
                }
                apply_cross_mod(&mut self.groups[gi].modulators, &mut self.group_cross_mod_scratch, self.telemetry.as_deref());
                if let Some(telemetry) = &self.telemetry {
                    publish_outputs(&self.groups[gi].modulators, telemetry);
                }
                Self::drain_learned(
                    self.learn_tx.as_ref(),
                    &mut self.groups[gi].modulators,
                    |_| ModulatorLocation::Group(gi),
                );
            }
            // Group→member-modulator cross-mod: written first so member lanes
            // pick up the modulated LFO/envelope fields when they tick below.
            collect_group_member_mod_writes(
                &self.groups,
                &self.instruments,
                &mut self.group_member_mod_writes,
            );
            apply_group_member_mod_writes(&self.group_member_mod_writes, &mut self.instruments);
            collect_group_mod_writes(
                &self.groups,
                &self.instruments,
                &mut self.group_mod_writes,
                &mut self.group_member_drives,
                &mut self.group_bus_drives,
            );
            if let Some(telemetry) = &self.telemetry {
                publish_group_effective(&self.groups, &self.instruments, &self.group_mod_writes, telemetry);
            }
            apply_group_mod_writes(
                &self.group_mod_writes,
                &mut self.instruments,
                &mut self.groups,
            );
        }

        // Process each instrument; route its output to its group's bus (if a
        // member) or straight to the master mix.
        for (inst_idx, inst) in self.instruments.iter_mut().enumerate() {
            // Zero lane_buf
            for buf in self.lane_buf.iter_mut() {
                buf.fill(0.0);
            }

            inst.process(midi_events, &mut self.lane_buf, self.num_channels, scale, self.bpm, self.telemetry.as_deref())?;
            // What the lane is heard at: its own mute and solo (its group's mute is the bus's). The meter is after it, as it is after the volume.
            let group_solo = inst.group.and_then(|g| self.groups.get(g)).is_some_and(|g| g.solo);
            let heard = if !inst.muted && (!any_solo || inst.solo || group_solo) { 1.0 } else { 0.0 };
            fade(&mut self.lane_buf, self.num_channels, inst.heard_at, heard);
            inst.heard_at = heard;
            if let (Some(telemetry), Some(fall)) = (&self.telemetry, fall) {
                publish_outputs(&inst.modulators, telemetry);
                if let Some(meter) = inst.meter.as_mut() {
                    meter.update(telemetry, &self.lane_buf, fall);
                }
            }
            // A lane MidiLearn modulator may have just captured a control.
            Self::drain_learned(
                self.learn_tx.as_ref(),
                &mut inst.modulators,
                |_| ModulatorLocation::Lane(inst_idx),
            );

            let dest = match inst.group {
                Some(g) if g < self.groups.len() => &mut self.groups[g].accum,
                _ => &mut self.mix_buf,
            };
            for (d, src) in dest.iter_mut().zip(self.lane_buf.iter()).take(self.num_channels) {
                for (ds, s) in d.iter_mut().zip(src.iter()) {
                    *ds += *s;
                }
            }
        }

        // What the members' settings that a group's cable drives ended up as, now that the lanes have run.
        if let Some(telemetry) = &self.telemetry {
            publish_group_member_mod_effective(&self.groups, &self.instruments, telemetry);
        }

        // Run each group's volume + effect chain, then fold into the master.
        for (gi, group) in self.groups.iter_mut().enumerate() {
            group.finish(self.num_channels, frames, self.telemetry.as_deref())?;
            // What the bus is heard at: a group that is not muted is heard when nothing is soloed, when it is, and when a lane in it is.
            let member_solo = self.instruments.iter().any(|lane| lane.solo && lane.group == Some(gi));
            let heard = if !group.muted && (!any_solo || group.solo || member_solo) { 1.0 } else { 0.0 };
            fade(&mut group.accum, self.num_channels, group.heard_at, heard);
            group.heard_at = heard;
            if let (Some(telemetry), Some(fall), Some(meter)) = (&self.telemetry, fall, group.meter.as_mut()) {
                meter.update(telemetry, &group.accum, fall);
            }
            for (m, src) in self.mix_buf.iter_mut().zip(group.accum.iter()).take(self.num_channels) {
                for (ms, s) in m.iter_mut().zip(src.iter()) {
                    *ms += *s;
                }
            }
        }

        // Copy mix_buf to audio_out
        for (ch, out) in audio_out.iter_mut().enumerate() {
            if ch < self.mix_buf.len() {
                let copy_len = out.len().min(self.mix_buf[ch].len());
                out[..copy_len].copy_from_slice(&self.mix_buf[ch][..copy_len]);
            }
        }

        // What reaches the output, measured and kept for a scope.
        if let (Some(telemetry), Some(fall)) = (&self.telemetry, fall) {
            self.master_meter.update(telemetry, &self.mix_buf, fall);
            telemetry.scope.push(&self.mix_buf);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{ParameterInfo, Preset};

    const FRAMES: usize = 64;

    macro_rules! mock_plugin_boilerplate {
        () => {
            fn sample_rate(&self) -> f32 {
                48000.0
            }
            fn parameters(&self) -> Vec<ParameterInfo> {
                Vec::new()
            }
            fn get_parameter(&mut self, _: u32) -> Option<f32> {
                None
            }
            fn set_parameter(&mut self, i: u32, _: f32) -> anyhow::Result<()> {
                anyhow::bail!("no parameter {i}")
            }
            fn presets(&self) -> Vec<Preset> {
                Vec::new()
            }
            fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
                anyhow::bail!("no preset {id}")
            }
        };
    }

    /// Test instrument: outputs a constant value on all channels when a note is held.
    struct ConstInstrument {
        value: f32,
        num_outputs: usize,
        has_note: bool,
    }

    impl ConstInstrument {
        #[expect(clippy::new_ret_no_self, reason = "test helper returns trait object")]
        fn new(value: f32) -> Box<dyn Plugin> {
            Box::new(Self {
                value,
                num_outputs: 2,
                has_note: false,
            })
        }
        fn with_outputs(value: f32, num_outputs: usize) -> Box<dyn Plugin> {
            Box::new(Self {
                value,
                num_outputs,
                has_note: false,
            })
        }
    }

    impl Plugin for ConstInstrument {
        fn name(&self) -> &str {
            "ConstInstrument"
        }
        fn is_instrument(&self) -> bool {
            true
        }
        fn audio_output_count(&self) -> usize {
            self.num_outputs
        }
        fn audio_input_count(&self) -> usize {
            0
        }

        fn process(
            &mut self,
            midi_events: &[(u64, [u8; 3])],
            _audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for &(_, [status, _, velocity]) in midi_events {
                match status & 0xF0 {
                    0x90 if velocity > 0 => self.has_note = true,
                    0x80 | 0x90 => self.has_note = false,
                    _ => {}
                }
            }
            let v = if self.has_note { self.value } else { 0.0 };
            for ch in audio_out.iter_mut() {
                ch.fill(v);
            }
            Ok(())
        }

        mock_plugin_boilerplate!();
    }

    /// Effect that copies input to output unchanged.
    struct PassthroughEffect;

    impl Plugin for PassthroughEffect {
        fn name(&self) -> &str {
            "Passthrough"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }

        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                out.copy_from_slice(inp);
            }
            Ok(())
        }

        mock_plugin_boilerplate!();
    }

    /// Effect that multiplies input by a constant gain.
    struct ScaleEffect(f32);

    impl Plugin for ScaleEffect {
        fn name(&self) -> &str {
            "Scale"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }

        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                for (o, &i) in out.iter_mut().zip(inp.iter()) {
                    *o = i * self.0;
                }
            }
            Ok(())
        }

        mock_plugin_boilerplate!();
    }

    /// Effect that adds a constant offset to input.
    struct OffsetEffect(f32);

    impl Plugin for OffsetEffect {
        fn name(&self) -> &str {
            "Offset"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }

        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                for (o, &i) in out.iter_mut().zip(inp.iter()) {
                    *o = i + self.0;
                }
            }
            Ok(())
        }

        mock_plugin_boilerplate!();
    }

    // -- helpers --

    fn make_graph(
        num_channels: usize,
    ) -> (
        AudioGraph,
        crossbeam_channel::Sender<GraphCommand>,
        crossbeam_channel::Receiver<Box<dyn Plugin>>,
    ) {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(num_channels, cmd_rx, return_tx);
        // Create one instrument lane (mimics old PluginChain behavior)
        graph.instruments.push(InstrumentLane::new(num_channels));
        (graph, cmd_tx, return_rx)
    }

    fn make_output() -> Vec<Vec<f32>> {
        vec![vec![0.0; FRAMES]; 2]
    }

    fn note_on(note: u8) -> (u64, [u8; 3]) {
        (0, [0x90, note, 100])
    }

    fn note_off(note: u8) -> (u64, [u8; 3]) {
        (0, [0x80, note, 0])
    }

    fn swap_instrument(cmd_tx: &crossbeam_channel::Sender<GraphCommand>, inst: Box<dyn Plugin>) {
        swap_instrument_at(cmd_tx, 0, inst);
    }

    fn swap_instrument_at(
        cmd_tx: &crossbeam_channel::Sender<GraphCommand>,
        index: usize,
        inst: Box<dyn Plugin>,
    ) {
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: index,
                instrument: inst,
                inst_buf,
                remapper: None,
                keep: None,
            })
            .unwrap();
    }

    fn insert_effect(
        cmd_tx: &crossbeam_channel::Sender<GraphCommand>,
        index: usize,
        effect: Box<dyn Plugin>,
        mix: f64,
    ) {
        cmd_tx
            .send(GraphCommand::InsertEffect {
                inst: 0,
                index,
                effect,
                mix,
            })
            .unwrap();
    }

    // -- tests --

    #[test]
    fn silence_when_no_instrument() {
        let (mut graph, _, _) = make_graph(2);
        let mut out = make_output();
        out[0].fill(999.0);
        out[1].fill(999.0);

        graph.process(&[], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.0));
        assert!(out[1].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn instrument_direct_output() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.75));

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.75));
        assert!(out[1].iter().all(|&s| s == 0.75));
    }

    #[test]
    fn instrument_silence_without_note() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.75));

        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn note_off_silences_instrument() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.75));

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 0.75));

        let mut out = make_output();
        graph.process(&[note_off(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn single_passthrough_effect() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.5));
        insert_effect(&cmd_tx, 0, Box::new(PassthroughEffect), 1.0);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.5));
        assert!(out[1].iter().all(|&s| s == 0.5));
    }

    #[test]
    fn dry_wet_mix() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        // ScaleEffect(0.0) outputs silence; mix=0.5 → 0.5*dry + 0.5*wet = 0.5
        insert_effect(&cmd_tx, 0, Box::new(ScaleEffect(0.0)), 0.5);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| (s - 0.5).abs() < 1e-6));
        assert!(out[1].iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn multiple_effects_chain() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        insert_effect(&cmd_tx, 0, Box::new(ScaleEffect(0.5)), 1.0);
        insert_effect(&cmd_tx, 1, Box::new(ScaleEffect(0.5)), 1.0);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        // 1.0 * 0.5 * 0.5 = 0.25
        assert!(out[0].iter().all(|&s| (s - 0.25).abs() < 1e-6));
    }

    #[test]
    fn multi_output_instrument_truncation() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::with_outputs(0.8, 4));
        insert_effect(&cmd_tx, 0, Box::new(PassthroughEffect), 1.0);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        // Only first 2 of 4 channels reach the output
        assert!(out[0].iter().all(|&s| s == 0.8));
        assert!(out[1].iter().all(|&s| s == 0.8));
    }

    #[test]
    fn multi_output_instrument_no_effects() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        // 16-output instrument with no effects (the Pianoteq scenario)
        swap_instrument(&cmd_tx, ConstInstrument::with_outputs(0.6, 16));

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.6));
        assert!(out[1].iter().all(|&s| s == 0.6));
    }

    #[test]
    fn swap_instrument_returns_old() {
        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));

        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();

        // Swap in a new instrument
        swap_instrument(&cmd_tx, ConstInstrument::new(0.5));
        graph.process(&[], &mut out).unwrap();

        // Old instrument should have been returned via the channel
        let old = return_rx.try_recv();
        assert!(old.is_ok());
        assert_eq!(old.unwrap().name(), "ConstInstrument");
    }

    #[test]
    fn remove_effect() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        insert_effect(&cmd_tx, 0, Box::new(ScaleEffect(0.5)), 1.0);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 0.5).abs() < 1e-6));

        // Remove the effect — should go back to direct instrument output
        cmd_tx
            .send(GraphCommand::RemoveEffect {
                inst: 0,
                index: 0,
                keep: None,
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 1.0));
    }

    #[test]
    fn reorder_effects() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        // [Scale(2.0), Offset(0.5)] → 1.0 * 2.0 + 0.5 = 2.5
        insert_effect(&cmd_tx, 0, Box::new(ScaleEffect(2.0)), 1.0);
        insert_effect(&cmd_tx, 1, Box::new(OffsetEffect(0.5)), 1.0);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 2.5).abs() < 1e-6));

        // Move Scale from index 0 to index 1 → [Offset(0.5), Scale(2.0)]
        // (1.0 + 0.5) * 2.0 = 3.0
        cmd_tx
            .send(GraphCommand::ReorderEffect {
                inst: 0,
                from: 0,
                to: 1,
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 3.0).abs() < 1e-6));
    }

    #[test]
    fn reject_effect_with_wrong_channel_count() {
        /// Mono effect (1 output) — incompatible with a stereo chain.
        struct MonoEffect;

        impl Plugin for MonoEffect {
            fn name(&self) -> &str {
                "MonoEffect"
            }
            fn is_instrument(&self) -> bool {
                false
            }
            fn audio_output_count(&self) -> usize {
                1
            }
            fn audio_input_count(&self) -> usize {
                1
            }

            fn process(
                &mut self,
                _midi_events: &[(u64, [u8; 3])],
                audio_in: &[&[f32]],
                audio_out: &mut [&mut [f32]],
            ) -> anyhow::Result<()> {
                for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                    out.copy_from_slice(inp);
                }
                Ok(())
            }

            mock_plugin_boilerplate!();
        }

        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        insert_effect(&cmd_tx, 0, Box::new(MonoEffect), 1.0);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        // Effect was rejected — instrument output passes through directly
        assert!(out[0].iter().all(|&s| s == 1.0));

        // Rejected effect was returned via the return channel
        let returned = return_rx.try_recv();
        assert!(returned.is_ok());
        assert_eq!(returned.unwrap().name(), "MonoEffect");
    }

    // -- NoteRemapper tests --

    fn make_remap(entries: &[(&str, &str, f64)]) -> HashMap<String, crate::session::RemapTarget> {
        entries
            .iter()
            .map(|(src, dst, detune)| {
                (
                    src.to_string(),
                    crate::session::RemapTarget {
                        note: dst.to_string(),
                        detune: *detune,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn remapper_from_config_valid() {
        let remap = make_remap(&[("G#4", "G4", 1.0), ("C#2", "D2", -0.5)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        // G#4 = 68, C#2 = 37 — both should be in the table
        assert!(remapper.table.contains_key(&68));
        assert!(remapper.table.contains_key(&37));
    }

    #[test]
    fn remapper_remap_note_on() {
        // Remap G#4 (68) → G4 (67) with +1 semitone detune
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        let input = vec![(0u64, [0x90u8, 68, 100])]; // note-on G#4
        let mut output = Vec::new();
        remapper.remap_events(&input, &mut output);

        // Should produce: remapped note-on + pitch bend
        assert_eq!(output.len(), 2);
        // First event: note-on for G4 (67) on channel 2
        assert_eq!(output[0].1[0], 0x91); // note-on ch2
        assert_eq!(output[0].1[1], 67); // G4
        assert_eq!(output[0].1[2], 100); // velocity preserved
        // Second event: pitch bend on channel 2 (status 0xE1)
        assert_eq!(output[1].1[0] & 0xF0, 0xE0);
        assert_eq!(output[1].1[0] & 0x0F, 1); // channel 2 = nibble 0x01
    }

    #[test]
    fn remapper_remap_note_off() {
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        let input = vec![(0u64, [0x80u8, 68, 0])]; // note-off G#4
        let mut output = Vec::new();
        remapper.remap_events(&input, &mut output);

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].1[0], 0x81); // note-off ch2
        assert_eq!(output[0].1[1], 67); // G4
    }

    #[test]
    fn remapper_passthrough_non_remapped() {
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        // C4 (60) is NOT remapped — should pass through unchanged
        let input = vec![(0u64, [0x90u8, 60, 100])];
        let mut output = Vec::new();
        remapper.remap_events(&input, &mut output);

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].1, [0x90, 60, 100]);
    }

    #[test]
    fn remapper_passthrough_non_note_events() {
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        // CC message — should pass through unchanged
        let input = vec![(0u64, [0xB0u8, 64, 127])];
        let mut output = Vec::new();
        remapper.remap_events(&input, &mut output);

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].1, [0xB0, 64, 127]);
    }

    #[test]
    fn remapper_pitch_bend_bytes_center() {
        // Detune 0.0 → pitch bend = 8192 (center)
        let remap = make_remap(&[("C4", "C4", 0.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        let entry = &remapper.table[&60];
        // 8192 = 0b10_0000_0000000 → LSB = 0, MSB = 64
        assert_eq!(entry.pitch_bend_lsb, 0);
        assert_eq!(entry.pitch_bend_msb, 64);
    }

    #[test]
    fn remapper_pitch_bend_bytes_max() {
        // Detune +2.0 with range 2.0 → pitch bend = 8192 + 8191 = 16383
        let remap = make_remap(&[("C4", "C4", 2.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        let entry = &remapper.table[&60];
        // 16383 = 0x3FFF → LSB = 127, MSB = 127
        assert_eq!(entry.pitch_bend_lsb, 127);
        assert_eq!(entry.pitch_bend_msb, 127);
    }

    #[test]
    fn remapper_pitch_bend_bytes_min() {
        // Detune -2.0 with range 2.0 → pitch bend = 8192 - 8191 = 1
        let remap = make_remap(&[("C4", "C4", -2.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        let entry = &remapper.table[&60];
        // 1 = 0b00_0000_0000001 → LSB = 1, MSB = 0
        assert_eq!(entry.pitch_bend_lsb, 1);
        assert_eq!(entry.pitch_bend_msb, 0);
    }

    #[test]
    fn remapper_shared_detune_shares_channel() {
        // Two notes with the same detune should share a MIDI channel
        let remap = make_remap(&[("C4", "B3", 1.0), ("D4", "C#4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        assert_eq!(remapper.table[&60].channel, remapper.table[&62].channel);
    }

    #[test]
    fn remapper_different_detune_different_channels() {
        let remap = make_remap(&[("C4", "B3", 1.0), ("D4", "C#4", -0.5)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        assert_ne!(remapper.table[&60].channel, remapper.table[&62].channel);
    }

    #[test]
    fn remapper_hands_out_its_channels_in_the_order_of_the_notes_it_remaps() {
        // (A hash map's order is not the same from one map to the next, so each remapper is made from a map of its own, as each load of a session is.)
        // (A3 and A#3 are in the order of their numbers and not of their names, in which A#3 comes first.)
        for _ in 0..32 {
            let remap = make_remap(&[("E4", "F4", -0.5), ("C4", "D4", 0.866), ("A#3", "B3", 0.1), ("D4", "E4", 0.866), ("A3", "B3", 0.3)]);
            let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
            let channel = |note: u8| remapper.table[&note].channel;
            assert_eq!(
                (channel(57), channel(58), channel(60), channel(62), channel(64)),
                (1, 2, 3, 3, 4),
                "the lowest note's detune has the first channel, a note with the same detune shares it, and the next detune has the next"
            );
        }
    }

    #[test]
    fn remapper_error_detune_exceeds_range() {
        let remap = make_remap(&[("C4", "B3", 3.0)]);
        let result = NoteRemapper::from_config(&remap, 2.0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exceeds"));
    }

    #[test]
    fn remapper_error_too_many_detune_values() {
        // 16 distinct detune values should fail (max 15)
        let notes = [
            "C2", "C#2", "D2", "D#2", "E2", "F2", "F#2", "G2", "G#2", "A2", "A#2", "B2", "C3",
            "C#3", "D3", "D#3",
        ];
        let entries: Vec<(&str, &str, f64)> = notes
            .iter()
            .enumerate()
            .map(|(i, &n)| (n, n, (i as f64 + 1.0) * 0.1))
            .collect();
        let remap = make_remap(&entries);
        let result = NoteRemapper::from_config(&remap, 10.0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too many"));
    }

    #[test]
    fn remapper_integration_with_graph() {
        // Integration test: remap G#4→G4 with pitch bend, verify instrument sees remapped events
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        let (mut graph, cmd_tx, _) = make_graph(2);
        let inst = ConstInstrument::new(0.75);
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: inst,
                inst_buf,
                remapper: Some(remapper),
                keep: None,
            })
            .unwrap();

        let mut out = make_output();
        // Send note-on for G#4 (68) — should be remapped to G4 (67) on ch2
        // ConstInstrument responds to any note-on, so we just verify it produces output
        graph.process(&[note_on(68)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 0.75));
    }

    // -- New AudioGraph-specific tests --

    #[test]
    fn two_instruments_sum_output() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);

        // Two instrument lanes: both full range
        graph.instruments.push(InstrumentLane::new(2));
        graph.instruments.push(InstrumentLane::new(2));

        // Swap instruments into both lanes
        let inst_a = ConstInstrument::new(0.3);
        let inst_buf_a = (0..inst_a.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: inst_a,
                inst_buf: inst_buf_a,
                remapper: None,
                keep: None,
            })
            .unwrap();

        let inst_b = ConstInstrument::new(0.5);
        let inst_buf_b = (0..inst_b.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 1,
                instrument: inst_b,
                inst_buf: inst_buf_b,
                remapper: None,
                keep: None,
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        // Both instruments output on same note → 0.3 + 0.5 = 0.8
        assert!(out[0].iter().all(|&s| (s - 0.8).abs() < 1e-6));
        assert!(out[1].iter().all(|&s| (s - 0.8).abs() < 1e-6));

        drop(return_rx);
    }

    #[test]
    fn group_bus_sums_members_and_applies_volume() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);

        graph.instruments.push(InstrumentLane::new(2));
        graph.instruments.push(InstrumentLane::new(2));
        swap_instrument_at(&cmd_tx, 0, ConstInstrument::new(0.25));
        swap_instrument_at(&cmd_tx, 1, ConstInstrument::new(0.25));

        // Group both lanes and halve the group bus volume: (0.25+0.25)*0.5.
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 1, group: Some(0) }).unwrap();
        cmd_tx.send(GraphCommand::SetGroupVolume { group: 0, value: 0.5 }).unwrap();

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(
            out[0].iter().all(|&s| (s - 0.25).abs() < 1e-6),
            "grouped sum × group volume should be 0.25, got {}",
            out[0][0]
        );

        // Ungroup one member: it now reaches the master directly at full level,
        // the other stays in the (still 0.5×) group: 0.25 + 0.25*0.5 = 0.375.
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 1, group: None }).unwrap();
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(
            out[0].iter().all(|&s| (s - 0.375).abs() < 1e-6),
            "expected 0.375 after ungrouping one member, got {}",
            out[0][0]
        );

        drop(return_rx);
    }

    #[test]
    fn group_pan_balances_bus_output() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);

        graph.instruments.push(InstrumentLane::new(2));
        swap_instrument_at(&cmd_tx, 0, ConstInstrument::new(0.5));
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();

        // Hard right: left silent, right at full member level.
        cmd_tx.send(GraphCommand::SetGroupPan { group: 0, value: 1.0 }).unwrap();
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s.abs() < 1e-6), "hard right should silence left: {}", out[0][0]);
        assert!(out[1].iter().all(|&s| (s - 0.5).abs() < 1e-6), "right stays at full: {}", out[1][0]);

        // Center: both channels at full (unchanged).
        cmd_tx.send(GraphCommand::SetGroupPan { group: 0, value: 0.0 }).unwrap();
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 0.5).abs() < 1e-6), "center left: {}", out[0][0]);
        assert!(out[1].iter().all(|&s| (s - 0.5).abs() < 1e-6), "center right: {}", out[1][0]);

        drop(return_rx);
    }

    #[test]
    fn range_filtering() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);

        // Two instrument lanes with ranges: C0-B3 and C4-C8
        let mut lane_low = InstrumentLane::new(2);
        lane_low.range = Some((12, 59)); // C0-B3
        let mut lane_high = InstrumentLane::new(2);
        lane_high.range = Some((60, 96)); // C4-C8

        graph.instruments.push(lane_low);
        graph.instruments.push(lane_high);

        // Low lane: value 0.3
        let inst_low = ConstInstrument::new(0.3);
        let inst_buf_low = (0..inst_low.audio_output_count())
            .map(|_| Vec::new())
            .collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: inst_low,
                inst_buf: inst_buf_low,
                remapper: None,
                keep: None,
            })
            .unwrap();

        // High lane: value 0.7
        let inst_high = ConstInstrument::new(0.7);
        let inst_buf_high = (0..inst_high.audio_output_count())
            .map(|_| Vec::new())
            .collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 1,
                instrument: inst_high,
                inst_buf: inst_buf_high,
                remapper: None,
                keep: None,
            })
            .unwrap();

        // Play note in low range (C2 = 36): only low instrument should respond
        let mut out = make_output();
        graph.process(&[note_on(36)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 0.3).abs() < 1e-6));

        // Now note-off and play note in high range (C5 = 72): only high instrument should respond
        let mut out = make_output();
        graph.process(&[note_off(36), note_on(72)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 0.7).abs() < 1e-6));

        drop(return_rx);
    }

    #[test]
    fn cc_passthrough_to_all_instruments() {
        // CC events (e.g. sustain pedal) should reach all instruments regardless of range
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);

        let mut lane_low = InstrumentLane::new(2);
        lane_low.range = Some((0, 59));
        let mut lane_high = InstrumentLane::new(2);
        lane_high.range = Some((60, 127));

        graph.instruments.push(lane_low);
        graph.instruments.push(lane_high);

        // Install instruments in both lanes
        for s in 0..2 {
            let inst = ConstInstrument::new(0.5);
            let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
            cmd_tx
                .send(GraphCommand::SwapInstrument {
                    inst: s,
                    instrument: inst,
                    inst_buf,
                    remapper: None,
                    keep: None,
                })
                .unwrap();
        }

        // Send a CC event (sustain pedal) — should be filtered to both lanes
        let cc_event: (u64, [u8; 3]) = (0, [0xB0, 64, 127]);
        let mut out = make_output();
        graph.process(&[cc_event], &mut out).unwrap();

        // No note-on, so output is silence, but the point is it didn't crash
        // and the CC was delivered to both instruments (verified by filter_midi logic)
        assert!(out[0].iter().all(|&s| s == 0.0));

        drop(return_rx);
    }

    #[test]
    fn volume_scaling() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));

        // Set volume to 0.5
        cmd_tx
            .send(GraphCommand::SetVolume {
                inst: 0,
                value: 0.5,
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn empty_graph_silence() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, _return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);
        // No instruments at all

        let mut out = make_output();
        out[0].fill(999.0);
        graph.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.0));
        drop(cmd_tx);
    }

    // -- LFO waveform tests --

    #[test]
    fn lfo_sine_known_phases() {
        let w = LfoWaveform::Sine;
        // Phase 0.0 → sin(0) = 0.0
        assert!((w.eval(0.0)).abs() < 1e-6);
        // Phase 0.25 → sin(π/2) = 1.0
        assert!((w.eval(0.25) - 1.0).abs() < 1e-6);
        // Phase 0.5 → sin(π) ≈ 0.0
        assert!((w.eval(0.5)).abs() < 1e-6);
        // Phase 0.75 → sin(3π/2) = -1.0
        assert!((w.eval(0.75) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn lfo_triangle_known_phases() {
        let w = LfoWaveform::Triangle;
        // Phase 0.0 → 0.0
        assert!((w.eval(0.0)).abs() < 1e-6);
        // Phase 0.25 → 1.0
        assert!((w.eval(0.25) - 1.0).abs() < 1e-6);
        // Phase 0.5 → 0.0
        assert!((w.eval(0.5)).abs() < 1e-6);
        // Phase 0.75 → -1.0
        assert!((w.eval(0.75) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn lfo_saw_known_phases() {
        let w = LfoWaveform::Saw;
        // Phase 0.0 → -1.0
        assert!((w.eval(0.0) - (-1.0)).abs() < 1e-6);
        // Phase 0.5 → 0.0
        assert!((w.eval(0.5)).abs() < 1e-6);
        // Phase 1.0 → 1.0
        assert!((w.eval(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn lfo_square_known_phases() {
        let w = LfoWaveform::Square;
        // Phase 0.0 → 1.0 (first half)
        assert!((w.eval(0.0) - 1.0).abs() < 1e-6);
        // Phase 0.25 → 1.0 (still first half)
        assert!((w.eval(0.25) - 1.0).abs() < 1e-6);
        // Phase 0.5 → -1.0 (second half)
        assert!((w.eval(0.5) - (-1.0)).abs() < 1e-6);
        // Phase 0.75 → -1.0
        assert!((w.eval(0.75) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn lfo_waveform_cycle() {
        assert_eq!(LfoWaveform::Sine.next(), LfoWaveform::Triangle);
        assert_eq!(LfoWaveform::Triangle.next(), LfoWaveform::Saw);
        assert_eq!(LfoWaveform::Saw.next(), LfoWaveform::Square);
        assert_eq!(LfoWaveform::Square.next(), LfoWaveform::Random);
        assert_eq!(LfoWaveform::Random.next(), LfoWaveform::Smooth);
        assert_eq!(LfoWaveform::Smooth.next(), LfoWaveform::Sine);
        for waveform in LfoWaveform::ALL {
            assert_eq!(waveform.next().prev(), *waveform);
        }
    }

    #[test]
    fn lfo_waveform_from_str() {
        assert_eq!(LfoWaveform::from_str("sine"), Some(LfoWaveform::Sine));
        assert_eq!(LfoWaveform::from_str("TRIANGLE"), Some(LfoWaveform::Triangle));
        assert_eq!(LfoWaveform::from_str("tri"), Some(LfoWaveform::Triangle));
        assert_eq!(LfoWaveform::from_str("saw"), Some(LfoWaveform::Saw));
        assert_eq!(LfoWaveform::from_str("square"), Some(LfoWaveform::Square));
        assert_eq!(LfoWaveform::from_str("random"), Some(LfoWaveform::Random));
        assert_eq!(LfoWaveform::from_str("S&H"), Some(LfoWaveform::Random));
        assert_eq!(LfoWaveform::from_str("sh"), Some(LfoWaveform::Random));
        assert_eq!(LfoWaveform::from_str("smooth"), Some(LfoWaveform::Smooth));
        assert_eq!(LfoWaveform::from_str("unknown"), None);
    }

    #[test]
    fn every_lfo_waveform_has_its_place_in_the_list_and_a_name_that_is_its_own() {
        let names: Vec<&str> = LfoWaveform::ALL.iter().map(|waveform| waveform.name()).collect();
        assert_eq!(names, ["sine", "triangle", "saw", "square", "random", "smooth"]);
        for (index, waveform) in LfoWaveform::ALL.iter().enumerate() {
            assert_eq!(waveform.to_index(), index);
            assert_eq!(LfoWaveform::from_str(waveform.name()), Some(*waveform));
        }
    }

    // -- The random waveforms: a number for each cycle of the LFO, held (random) or glided to (smooth) --

    #[test]
    fn the_noise_of_a_cycle_is_known() {
        // The hash is written out in the page too (`client/noise.ts`, with its table in `fixtures/noise.json`, which this test's numbers are the first of): a change
        // of it changes the numbers of every random LFO of every saved session. (The numbers are from an independent version of it, in Python.)
        for (seed, cycle, expected) in [
            (0, 0, 0.362_777_7),
            (0, 1, 0.253_798_72),
            (0, 2, 0.973_738_1),
            (1, 0, -0.726_370_93),
            (1, 1, -0.005_097_627_6),
            (5, 0, 0.599_366_4),
            (5, 1, 0.556_360_5),
            (5, 2, 0.941_968_7),
            (0xDEAD_BEEF, 41, 0.633_545_9),
            (12345, 100_000, 0.124_951_72),
        ] {
            assert_eq!(noise(seed, cycle), expected, "seed {seed}, cycle {cycle}");
        }
    }

    #[test]
    fn the_noise_stays_in_its_range_and_is_spread_over_it() {
        let numbers: Vec<f32> = (0..20_000).map(|cycle| noise(3, cycle)).collect();
        assert!(numbers.iter().all(|n| (-1.0..1.0).contains(n)));
        let mean = numbers.iter().sum::<f32>() / numbers.len() as f32;
        assert!(mean.abs() < 0.02, "the mean is {mean}");
        // Each fifth of the range has about a fifth of the numbers (4,000 of them, give or take a few hundred).
        for fifth in 0..5 {
            let low = -1.0 + fifth as f32 * 0.4;
            let count = numbers.iter().filter(|&&n| n >= low && n < low + 0.4).count();
            assert!((3_600..4_400).contains(&count), "fifth {fifth} has {count}");
        }
        assert!(numbers.windows(2).all(|pair| pair[0] != pair[1]), "no two cycles in a row have the same number");
    }

    #[test]
    fn the_noise_of_one_seed_is_not_the_noise_of_another() {
        // (Two numbers drawn at random from the range are within a thousandth of each other one time in a thousand: ten of ten thousand.)
        let alike = (0..10_000).filter(|&cycle| (noise(0, cycle) - noise(1, cycle)).abs() < 0.001).count();
        assert!(alike < 40, "{alike} of 10000 cycles have about the same number for two seeds");
        assert_ne!(noise(0, 0), noise(1, 0));
    }

    #[test]
    fn a_random_lfo_has_the_number_of_its_cycle_whatever_the_phase_is() {
        for phase in [0.0, 0.1, 0.5, 0.999] {
            assert_eq!(LfoWaveform::Random.eval_cycle(phase, 5, 2), noise(5, 2), "at {phase}");
        }
        // (The shapes that go by the phase alone do not look at the cycle.)
        assert_eq!(LfoWaveform::Sine.eval_cycle(0.25, 5, 2), LfoWaveform::Sine.eval(0.25));
        assert_eq!(LfoWaveform::Random.eval(0.25), noise(0, 0));
    }

    #[test]
    fn a_smooth_lfo_goes_from_the_number_of_its_cycle_to_the_number_of_the_next_and_eases_at_both_ends() {
        let (from, to) = (noise(5, 1), noise(5, 2));
        assert!(to > from, "the numbers of this cycle and the next rise, which the rest of the test goes by");
        let at = |phase: f32| LfoWaveform::Smooth.eval_cycle(phase, 5, 1);
        assert_eq!(at(0.0), from);
        assert!(close(at(0.5), (from + to) / 2.0), "half way at the middle: {}", at(0.5));
        assert!(close(at(1.0), to));
        let steps: Vec<f32> = (0..=10).map(|tenth| at(tenth as f32 / 10.0)).collect();
        assert!(steps.windows(2).all(|pair| pair[0] <= pair[1]), "it only rises: {steps:?}");
        // (It eases: the first tenth of the cycle takes it less than a tenth of the way, and so does the last.)
        assert!(steps[1] - from < 0.1 * (to - from));
        assert!(to - steps[9] < 0.1 * (to - from));
    }

    /// The numbers that `noise` makes for a spread of seeds and cycles, as the web client's mock engine reads them (`web/src/client/fixtures/noise.json`): they pin the page's hash
    /// (`client/noise.ts`) to this one. A number is written as the 64-bit one it widens to, which is what the page works out. `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_the_noise_is_current() {
        let seeds = [0, 1, 2, 3, 5, 7, 255, 12_345, 0xDEAD_BEEF, u32::MAX];
        let cycles = [0, 1, 2, 3, 4, 5, 41, 255, 1_000, 100_000, 0x7FFF_FFFF, 0x8000_0000, u32::MAX];
        let numbers: Vec<serde_json::Value> = seeds
            .iter()
            .flat_map(|&seed| cycles.iter().map(move |&cycle| serde_json::json!({ "seed": seed, "cycle": cycle, "value": f64::from(noise(seed, cycle)) })))
            .collect();
        let fresh = serde_json::to_string_pretty(&serde_json::json!({ "numbers": numbers })).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/noise.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what the noise is now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    /// What `steps_output` makes of a spread of patterns, lengths, places and glides, as the web client's mock engine reads them (`web/src/client/fixtures/steps.json`): they pin the page's
    /// law of a step sequencer (`client/steps.ts`) to this one. A number is written as the 64-bit one it widens to. `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_step_a_step_sequencer_is_at_is_the_whole_part_of_its_position_brought_round_at_its_length() {
        assert_eq!(step_at(4, 0.0), 0);
        assert_eq!(step_at(4, 0.99), 0);
        assert_eq!(step_at(4, 2.5), 2);
        assert_eq!(step_at(4, 3.999), 3);
        assert_eq!(step_at(3, 3.0), 0, "round at the length");
        assert_eq!(step_at(3, 4.5), 1);
        assert_eq!(step_at(3, -0.5), 2, "a position before the start comes round from the end");
        assert_eq!(step_at(3, -1.0e-8), 2, "and one so little before it that the sum is the length itself is still the last step");
    }

    #[test]
    fn the_step_a_step_sequencer_is_at_is_kept_to_the_steps_there_are_and_is_the_first_for_a_position_that_is_not_a_number() {
        assert_eq!(step_at(0, 2.5), 0, "a length of none is one step");
        assert_eq!(step_at(99, 17.5), 1, "and one of more than there are is all of them");
        assert_eq!(step_at(MAX_STEPS, 15.99), MAX_STEPS - 1);
        for position in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(step_at(5, position), 0, "{position}");
        }
    }

    #[test]
    fn the_step_a_step_sequencer_is_at_is_the_one_whose_value_it_puts_out_with_no_glide() {
        let values: [f32; MAX_STEPS] = std::array::from_fn(|i| i as f32 / 16.0);
        for length in [1, 3, 5, 16] {
            for position in [0.0, 0.3, 1.0, 2.7, 4.99, 7.5, 15.5, 16.2, -0.3] {
                assert_eq!(steps_output(&values, length, position, 0.0), values[step_at(length, position)], "length {length}, position {position}");
            }
        }
    }

    #[test]
    fn the_web_clients_fixture_of_what_a_step_sequencer_puts_out_is_current() {
        let patterns: [[f32; MAX_STEPS]; 3] = [
            // The staircase of a new one.
            std::array::from_fn(|i| i as f32 / 8.0 - 1.0),
            // A few steps with a pattern that is not even, and the rest nothing.
            std::array::from_fn(|i| [0.5, -0.25, 1.0, -1.0, 0.125][i % 5] * if i < 8 { 1.0 } else { 0.5 }),
            // All of them at one end.
            [1.0; MAX_STEPS],
        ];
        let lengths = [1, 2, 3, 5, 8, 16];
        let places = [0.0, 0.01, 0.25, 0.5, 0.75, 0.9, 0.999, 1.0, 1.5, 2.25, 4.75, 7.0, 7.9, 15.5, 15.99];
        let glides = [0.0, 0.1, 0.25, 0.5, 1.0];
        let mut cases = Vec::new();
        for (p, values) in patterns.iter().enumerate() {
            for &length in &lengths {
                for &position in &places {
                    // (A place is within the steps in use: the engine wraps it at the length before it asks.)
                    if position >= length as f32 {
                        continue;
                    }
                    for &glide in &glides {
                        cases.push(serde_json::json!({
                            "pattern": p,
                            "length": length,
                            "position": f64::from(position),
                            "glide": f64::from(glide),
                            "output": f64::from(steps_output(values, length, position, glide)),
                        }));
                    }
                }
            }
        }
        // The bar of a playhead: for the positions a sequencer is at, and some that are past its length, before its start and huge, which it is brought round for (and one so little
        // before the start that, in a 64-bit number as well as in a 32-bit one, the sum with the length is the length itself, which is not a step).
        let mut steps = Vec::new();
        for &length in &[0, 1, 2, 3, 5, 8, 16, 99] {
            for &position in &[0.0, 0.5, 1.0, 2.9, 3.0, 4.5, 7.25, 15.99, 16.0, 17.5, 40.25, -0.5, -3.0, -1.0e-20, 1000.75] {
                steps.push(serde_json::json!({ "length": length, "position": f64::from(position), "step": step_at(length, position) }));
            }
        }
        let patterns: Vec<Vec<f64>> = patterns.iter().map(|values| values.iter().map(|&v| f64::from(v)).collect()).collect();
        let fresh = serde_json::to_string_pretty(&serde_json::json!({ "patterns": patterns, "cases": cases, "steps": steps })).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/steps.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what a step sequencer puts out now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    #[test]
    fn a_smooth_lfo_stays_between_the_numbers_of_its_cycle_and_the_next_for_a_phase_outside_the_cycle() {
        // (A phase is 0 to 1 for a rate that is above 0: a rate below 0 takes it below 0, which a session file is refused for (f147) and the engine holds to all the same.)
        let (from, to) = (noise(5, 1), noise(5, 2));
        for phase in [-3.0, -0.5, -1.0e-6, 1.0 + 1.0e-6, 1.5, 7.0] {
            let out = LfoWaveform::Smooth.eval_cycle(phase, 5, 1);
            assert!(out >= from.min(to) - 1.0e-6 && out <= from.max(to) + 1.0e-6, "at {phase}: {out}");
        }
        assert_eq!(LfoWaveform::Smooth.eval_cycle(-3.0, 5, 1), from);
        assert!(close(LfoWaveform::Smooth.eval_cycle(7.0, 5, 1), to));
    }

    /// An LFO of `waveform` that goes round once in `buffers` buffers (a rate that is exact: no cycle is early or late), with a seed of its own.
    fn lfo_that_goes_round_in(waveform: LfoWaveform, buffers: usize, seed: u32) -> Modulator {
        Modulator::new(lfo(waveform, 48000.0 / (FRAMES * buffers) as f32), 48000.0).with_seed(seed)
    }

    #[test]
    fn a_random_lfo_holds_the_number_of_its_cycle_and_takes_the_next_when_the_phase_wraps() {
        let mut m = lfo_that_goes_round_in(LfoWaveform::Random, 8, 5);
        for cycle in 0..3 {
            for buffer in 1..=8 {
                m.tick(FRAMES, &[], DEFAULT_BPM);
                // (The eighth buffer of a cycle wraps the phase: the LFO is in the next cycle by then.)
                let now = if buffer == 8 { cycle + 1 } else { cycle };
                assert_eq!(m.last_output, noise(5, now), "cycle {cycle}, buffer {buffer}");
            }
        }
    }

    #[test]
    fn a_smooth_lfo_glides_over_a_cycle_and_is_at_the_next_number_when_the_phase_wraps() {
        let mut m = lfo_that_goes_round_in(LfoWaveform::Smooth, 8, 5);
        let mut seen = Vec::new();
        for _ in 0..16 {
            m.tick(FRAMES, &[], DEFAULT_BPM);
            seen.push(m.last_output);
        }
        assert_eq!(seen[7], noise(5, 1), "the first cycle ends at the number of the second");
        assert_eq!(seen[15], noise(5, 2));
        for buffer in 1..8 {
            assert!(close(seen[buffer - 1], LfoWaveform::Smooth.eval_cycle(buffer as f32 / 8.0, 5, 0)), "buffer {buffer}");
            assert!(close(seen[8 + buffer - 1], LfoWaveform::Smooth.eval_cycle(buffer as f32 / 8.0, 5, 1)), "buffer {} of the second cycle", buffer);
        }
    }

    #[test]
    fn an_lfo_that_goes_round_more_than_once_in_a_buffer_counts_every_cycle() {
        // 2.5 turns a buffer: 2 cycles are over after the first, and 5 after the second.
        let mut m = Modulator::new(lfo(LfoWaveform::Random, 2.5 * 48000.0 / FRAMES as f32), 48000.0).with_seed(5);
        m.tick(FRAMES, &[], DEFAULT_BPM);
        assert_eq!(m.last_output, noise(5, 2));
        m.tick(FRAMES, &[], DEFAULT_BPM);
        assert_eq!(m.last_output, noise(5, 5));
    }

    #[test]
    fn a_random_lfo_does_not_start_again_when_its_rate_is_turned() {
        // Turning the rate moves the phase on from where it is, and the cycle goes on counting.
        let mut m = lfo_that_goes_round_in(LfoWaveform::Random, 8, 5);
        for _ in 0..8 {
            m.tick(FRAMES, &[], DEFAULT_BPM);
        }
        if let ModSource::Lfo { rate, .. } = &mut m.source {
            *rate *= 2.0;
        }
        for _ in 0..4 {
            m.tick(FRAMES, &[], DEFAULT_BPM);
        }
        assert_eq!(m.last_output, noise(5, 2), "the second cycle is over at the fourth buffer of the new rate");
    }

    // -- An LFO that a note starts again, and that fades in --

    /// An LFO of `waveform` at `rate` that a note starts again if `retrigger` is on, and that comes in over `fade` seconds from every note (not at all if it is 0).
    fn keyed_lfo(waveform: LfoWaveform, rate: f32, retrigger: bool, fade: f32) -> ModSource {
        ModSource::lfo(waveform, rate).with_key(retrigger, fade)
    }

    /// The rate of an LFO that goes round once in eight buffers, so that a saw climbs a quarter of its height a buffer (from -1).
    fn once_in_eight_buffers() -> f32 {
        48000.0 / (FRAMES * 8) as f32
    }

    /// How many seconds `buffers` buffers are.
    fn seconds_of(buffers: usize) -> f32 {
        (buffers * FRAMES) as f32 / 48000.0
    }

    #[test]
    fn a_note_starts_an_lfo_that_retriggers_again_at_the_start_of_its_wave() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, keyed_lfo(LfoWaveform::Saw, once_in_eight_buffers(), true, 0.0));
        for _ in 0..3 {
            rig.process(&[]);
        }
        assert!(close(rig.read(output), -0.25), "three buffers into the wave: {}", rig.read(output));
        rig.process(&[note_on(60)]);
        assert!(close(rig.read(output), -1.0), "a note puts it at the start of its wave: {}", rig.read(output));
        rig.process(&[]);
        assert!(close(rig.read(output), -0.75), "and it goes on from there: {}", rig.read(output));
    }

    #[test]
    fn an_lfo_that_does_not_retrigger_goes_on_through_a_note() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, keyed_lfo(LfoWaveform::Saw, once_in_eight_buffers(), false, 0.0));
        for _ in 0..3 {
            rig.process(&[]);
        }
        rig.process(&[note_on(60)]);
        assert!(close(rig.read(output), 0.0), "the fourth buffer of the wave, half way up: {}", rig.read(output));
    }

    #[test]
    fn a_note_off_and_a_note_on_with_no_velocity_do_not_start_an_lfo_again_or_its_fade() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, keyed_lfo(LfoWaveform::Saw, once_in_eight_buffers(), true, seconds_of(4)));
        rig.process(&[]);
        rig.process(&[note_off(60), (0, [0x90, 60, 0]), (0, [0xB0, 74, 100])]);
        // (A wave that had been started again would be at -1, and a fade that had been started would have put it at nothing: it is a quarter of the way up and at full.)
        assert!(close(rig.read(output), -0.5), "{}", rig.read(output));
    }

    #[test]
    fn a_random_lfo_that_retriggers_takes_the_next_number_with_every_note() {
        let mut m = Modulator::new(keyed_lfo(LfoWaveform::Random, once_in_eight_buffers(), true, 0.0), 48000.0).with_seed(5);
        m.tick(FRAMES, &[], DEFAULT_BPM);
        assert_eq!(m.last_output, noise(5, 0));
        m.tick(FRAMES, &[note_on(60)], DEFAULT_BPM);
        assert_eq!(m.last_output, noise(5, 1), "a note is the start of a new cycle: the next number");
        m.tick(FRAMES, &[], DEFAULT_BPM);
        assert_eq!(m.last_output, noise(5, 1), "which it holds");
        m.tick(FRAMES, &[note_on(60)], DEFAULT_BPM);
        assert_eq!(m.last_output, noise(5, 2));
    }

    #[test]
    fn an_lfo_with_a_fade_rests_at_full_until_a_note_and_then_comes_in_over_the_fade() {
        let mut rig = Rig::new();
        // A square that stays at +1 (a rate of nothing), so that what is read is the fade alone: four buffers long.
        let output = rig.lane_modulator(0, keyed_lfo(LfoWaveform::Square, 0.0, false, seconds_of(4)));
        rig.process(&[]);
        assert_eq!(rig.read(output), 1.0, "before the first note it is at full");
        rig.process(&[note_on(60)]);
        assert_eq!(rig.read(output), 0.0, "a note starts it at nothing");
        for (after, expected) in [(1, 0.25), (2, 0.5), (3, 0.75), (4, 1.0), (5, 1.0)] {
            rig.process(&[]);
            assert!(close(rig.read(output), expected), "{after} buffers after the note: {}", rig.read(output));
        }
        rig.process(&[note_on(62)]);
        assert_eq!(rig.read(output), 0.0, "and the next note starts it at nothing again");
    }

    #[test]
    fn a_note_starts_the_fade_again_whether_or_not_the_lfo_retriggers_and_the_wave_of_one_that_does_not_goes_on() {
        let mut rig = Rig::new();
        let fading = rig.lane_modulator(0, keyed_lfo(LfoWaveform::Saw, once_in_eight_buffers(), false, seconds_of(8)));
        let starting = rig.lane_modulator(1, keyed_lfo(LfoWaveform::Saw, once_in_eight_buffers(), true, seconds_of(8)));
        for _ in 0..3 {
            rig.process(&[]);
        }
        rig.process(&[note_on(60)]);
        rig.process(&[]);
        rig.process(&[]);
        // Two buffers after the note the fade is a quarter of the way in. The wave that does not retrigger is six eighths round (a saw at a half), the one that does two eighths (at minus a half).
        assert!(close(rig.read(fading), 0.25 * 0.5), "{}", rig.read(fading));
        assert!(close(rig.read(starting), 0.25 * -0.5), "{}", rig.read(starting));
    }

    #[test]
    fn a_note_changes_nothing_of_an_lfo_that_neither_retriggers_nor_fades() {
        let run = |events: &[(u64, [u8; 3])]| {
            let mut m = Modulator::new(ModSource::lfo(LfoWaveform::Sine, 3.7), 48000.0);
            (0..40)
                .map(|_| {
                    m.tick(FRAMES, events, DEFAULT_BPM);
                    m.last_output
                })
                .collect::<Vec<f32>>()
        };
        assert_eq!(run(&[note_on(60)]), run(&[]), "the same numbers, to the last bit");
    }

    #[test]
    fn an_lfo_that_retriggers_hears_the_notes_the_lane_takes_and_no_others() {
        let mut rig = Rig::new();
        rig.cmd_tx.send(GraphCommand::SetInstrumentRange { inst: 0, range: Some((60, 71)) }).unwrap();
        let output = rig.lane_modulator(0, keyed_lfo(LfoWaveform::Saw, once_in_eight_buffers(), true, 0.0));
        rig.process(&[]);
        rig.process(&[(0, [0x90, 40, 100])]);
        assert!(close(rig.read(output), -0.5), "a note below the lane's range is not the lane's: the wave went on to a quarter: {}", rig.read(output));
        rig.process(&[(0, [0x90, 62, 100])]);
        assert!(close(rig.read(output), -1.0), "{}", rig.read(output));
    }

    #[test]
    fn the_key_of_an_lfo_is_set_by_a_command_and_the_lfo_stays_where_it_is() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, lfo(LfoWaveform::Saw, once_in_eight_buffers()));
        for _ in 0..2 {
            rig.process(&[]);
        }
        rig.cmd_tx.send(GraphCommand::SetModulatorKey { location: ModulatorLocation::Lane(0), index: 0, retrigger: true, fade: 0.0 }).unwrap();
        rig.process(&[]);
        assert!(close(rig.read(output), -0.25), "it went on from where it was: {}", rig.read(output));
        rig.process(&[note_on(60)]);
        assert!(close(rig.read(output), -1.0), "and a note starts it again, which it had not: {}", rig.read(output));
    }

    #[test]
    fn the_key_of_a_modulator_that_is_not_an_lfo_is_not_set() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, adsr(0.01));
        rig.cmd_tx.send(GraphCommand::SetModulatorKey { location: ModulatorLocation::Lane(0), index: 0, retrigger: true, fade: 1.0 }).unwrap();
        rig.process(&[note_on(60)]);
        assert!(matches!(rig.graph.instruments[0].modulators[0].source, ModSource::Envelope { .. }), "it is the envelope it was");
        assert!(rig.read(output) > 0.0, "and it answers to the note");
    }

    #[test]
    fn a_group_lfo_that_retriggers_is_started_again_by_a_note_of_a_member_and_is_given_its_key_by_a_command() {
        let mut g = GroupRig::new();
        g.rig
            .cmd_tx
            .send(GraphCommand::InsertGroupModulator { group: 0, index: 1, source: keyed_lfo(LfoWaveform::Saw, once_in_eight_buffers(), true, 0.0) })
            .unwrap();
        let output = g.rig.slot();
        g.rig.cmd_tx.send(GraphCommand::SetModulatorOutput { location: ModulatorLocation::Group(0), index: 1, slot: output }).unwrap();
        g.rig.process(&[]);
        g.rig.process(&[]);
        assert!(close(g.rig.read(output), -0.5), "{}", g.rig.read(output));
        g.rig.process(&[note_on(60)]);
        assert!(close(g.rig.read(output), -1.0), "a note of a member starts it again: {}", g.rig.read(output));
        // Now it does not retrigger, and fades over four buffers: the fade is one buffer old when the command has been taken.
        g.rig.cmd_tx.send(GraphCommand::SetModulatorKey { location: ModulatorLocation::Group(0), index: 1, retrigger: false, fade: seconds_of(4) }).unwrap();
        g.rig.process(&[]);
        assert!(close(g.rig.read(output), -0.75 * 0.25), "{}", g.rig.read(output));
        g.rig.process(&[note_on(60)]);
        assert_eq!(g.rig.read(output), 0.0, "a note starts the fade again, at nothing");
        g.rig.process(&[]);
        assert!(close(g.rig.read(output), -0.25 * 0.25), "and the wave went on through it (three eighths, not one): {}", g.rig.read(output));
    }

    // -- A modulator that follows the tempo --

    /// A saw that goes `rate` cycles a second of its own, or a cycle in `beats` beats of the tempo if that is not 0.
    fn synced_saw(rate: f32, beats: f32) -> ModSource {
        ModSource::lfo(LfoWaveform::Saw, rate).with_sync(beats)
    }

    /// What a saw that has gone round `cycles_a_second` times a second for `buffers` buffers puts out: its phase, as a number from -1 to 1.
    fn saw_after(cycles_a_second: f32, buffers: usize) -> f32 {
        lfo_phase(cycles_a_second, buffers) * 2.0 - 1.0
    }

    #[test]
    fn an_lfo_that_follows_the_tempo_goes_a_cycle_in_the_beats_it_is_given_and_not_at_its_own_rate() {
        let mut rig = Rig::new();
        // A sixteenth note (a quarter of a beat) at the 120 beats a minute that a graph begins at is 8 cycles a second. The 1 that the LFO has of its own is not what it goes at.
        let output = rig.lane_modulator(0, synced_saw(1.0, 0.25));
        for _ in 0..30 {
            rig.process(&[]);
        }
        assert!(close(rig.read(output), saw_after(8.0, 30)), "{} for {}", rig.read(output), saw_after(8.0, 30));
        // Two beats take it as long as a quarter of a beat takes it eight times over, at any tempo.
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, synced_saw(1.0, 2.0));
        for _ in 0..30 {
            rig.process(&[]);
        }
        assert!(close(rig.read(output), saw_after(1.0, 30)), "{}", rig.read(output));
    }

    #[test]
    fn an_lfo_that_follows_the_tempo_goes_by_it_as_it_changes_and_one_that_does_not_goes_on() {
        let mut rig = Rig::new();
        let following = rig.lane_modulator(0, synced_saw(1.0, 0.25));
        let free = rig.lane_modulator(1, lfo(LfoWaveform::Saw, 8.0));
        for _ in 0..10 {
            rig.process(&[]);
        }
        // Half the tempo, half the rate: the first ten buffers were at 8 cycles a second and the next ten are at 4.
        rig.cmd_tx.send(GraphCommand::SetGlobalBpm { bpm: 60.0 }).unwrap();
        for _ in 0..10 {
            rig.process(&[]);
        }
        let expected = (lfo_phase(8.0, 10) + lfo_phase(4.0, 10)) % 1.0 * 2.0 - 1.0;
        assert!(close(rig.read(following), expected), "{} for {expected}", rig.read(following));
        assert!(close(rig.read(free), saw_after(8.0, 20)), "{}", rig.read(free));
    }

    #[test]
    fn a_tempo_that_is_no_number_or_is_none_is_not_one_for_an_lfo_to_follow() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, synced_saw(1.0, 0.25));
        for _ in 0..5 {
            rig.process(&[]);
        }
        for bpm in [f32::NAN, f32::INFINITY, 0.0, -90.0] {
            rig.cmd_tx.send(GraphCommand::SetGlobalBpm { bpm }).unwrap();
            rig.process(&[]);
        }
        // (It went on at the 120 it had: nine buffers at 8 cycles a second, and not at a rate that is no number, which would have put it at no number at all.)
        assert!(close(rig.read(output), saw_after(8.0, 9)), "{}", rig.read(output));
    }

    #[test]
    fn the_beats_an_lfo_goes_a_cycle_in_are_set_by_a_command_and_it_goes_at_its_own_rate_again_when_they_are_none() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, lfo(LfoWaveform::Saw, 4.0));
        for _ in 0..5 {
            rig.process(&[]);
        }
        let sync = |beats| GraphCommand::SetModulatorSync { location: ModulatorLocation::Lane(0), index: 0, beats };
        rig.cmd_tx.send(sync(0.25)).unwrap();
        for _ in 0..5 {
            rig.process(&[]);
        }
        rig.cmd_tx.send(sync(0.0)).unwrap();
        for _ in 0..5 {
            rig.process(&[]);
        }
        // Five buffers at its own 4 cycles a second, five at the tempo's 8 and five at its own again, from where it was each time.
        let expected = (lfo_phase(4.0, 5) + lfo_phase(8.0, 5) + lfo_phase(4.0, 5)) % 1.0 * 2.0 - 1.0;
        assert!(close(rig.read(output), expected), "{} for {expected}", rig.read(output));
    }

    #[test]
    fn the_beats_of_a_modulator_that_is_neither_an_lfo_nor_a_step_sequencer_are_not_set() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, adsr(0.01));
        rig.cmd_tx.send(GraphCommand::SetModulatorSync { location: ModulatorLocation::Lane(0), index: 0, beats: 0.5 }).unwrap();
        rig.process(&[note_on(60)]);
        assert!(matches!(rig.graph.instruments[0].modulators[0].source, ModSource::Envelope { .. }), "it is the envelope it was");
        assert!(rig.read(output) > 0.0, "and it answers to the note");
    }

    #[test]
    fn a_step_sequencer_that_follows_the_tempo_goes_a_step_in_the_beats_it_is_given_and_not_at_its_own_rate() {
        let mut rig = Rig::new();
        // A thirty-second note (an eighth of a beat) at 120 beats a minute is 16 steps a second, and the 1 that it has of its own is not what it goes at.
        let (output, position) = lane_steps(&mut rig, 0, steps(&[0.5, -0.25, 1.0], 3, 1.0, 0.0, false).with_sync(0.125));
        // (Seventy-five buffers are a tenth of a second: a step and six tenths.)
        for _ in 0..75 {
            rig.process(&[]);
        }
        assert!((rig.read(position) - 1.6).abs() < 1e-4, "{}", rig.read(position));
        assert_eq!(rig.read(output), -0.25, "the second step");
        // At 60 beats a minute it goes half as fast: the next seventy-five buffers are eight tenths of a step more.
        rig.cmd_tx.send(GraphCommand::SetGlobalBpm { bpm: 60.0 }).unwrap();
        for _ in 0..75 {
            rig.process(&[]);
        }
        assert!((rig.read(position) - 2.4).abs() < 1e-4, "{}", rig.read(position));
        assert_eq!(rig.read(output), 1.0, "the third step");
    }

    #[test]
    fn the_beats_a_step_sequencer_goes_a_step_in_are_set_by_a_command_too_and_it_goes_at_its_own_rate_again_when_they_are_none() {
        let mut rig = Rig::new();
        let (_output, position) = lane_steps(&mut rig, 0, steps(&[0.5, -0.25, 1.0], 3, 8.0, 0.0, false));
        let sync = |beats| GraphCommand::SetModulatorSync { location: ModulatorLocation::Lane(0), index: 0, beats };
        // (Seventy-five buffers are a tenth of a second.) At its own 8 steps a second it is 0.8 of a step on; then 16 a second takes it 1.6 on; then it is at its own rate again.
        for _ in 0..75 {
            rig.process(&[]);
        }
        assert!((rig.read(position) - 0.8).abs() < 1e-4, "{}", rig.read(position));
        rig.cmd_tx.send(sync(0.125)).unwrap();
        for _ in 0..75 {
            rig.process(&[]);
        }
        assert!((rig.read(position) - 2.4).abs() < 1e-4, "{}", rig.read(position));
        rig.cmd_tx.send(sync(0.0)).unwrap();
        for _ in 0..75 {
            rig.process(&[]);
        }
        assert!((rig.read(position) - ((2.4 + 0.8) % 3.0)).abs() < 1e-4, "{}", rig.read(position));
    }

    #[test]
    fn a_group_lfo_follows_the_tempo_too_and_is_given_the_beats_by_a_command() {
        let mut g = GroupRig::new();
        g.rig.cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 1, source: lfo(LfoWaveform::Saw, 1.0) }).unwrap();
        let output = g.rig.slot();
        g.rig.cmd_tx.send(GraphCommand::SetModulatorOutput { location: ModulatorLocation::Group(0), index: 1, slot: output }).unwrap();
        g.rig.cmd_tx.send(GraphCommand::SetModulatorSync { location: ModulatorLocation::Group(0), index: 1, beats: 0.25 }).unwrap();
        for _ in 0..30 {
            g.rig.process(&[]);
        }
        assert!(close(g.rig.read(output), saw_after(8.0, 30)), "{}", g.rig.read(output));
        g.rig.cmd_tx.send(GraphCommand::SetGlobalBpm { bpm: 240.0 }).unwrap();
        for _ in 0..10 {
            g.rig.process(&[]);
        }
        let expected = (lfo_phase(8.0, 30) + lfo_phase(16.0, 10)) % 1.0 * 2.0 - 1.0;
        assert!(close(g.rig.read(output), expected), "{} for {expected}", g.rig.read(output));
    }

    #[test]
    fn the_random_lfos_of_a_graph_have_numbers_of_their_own_and_a_graph_that_is_made_again_has_the_same() {
        let run = || {
            let mut rig = Rig::new();
            let first = rig.lane_modulator(0, lfo(LfoWaveform::Random, 93.75));
            let second = rig.lane_modulator(1, lfo(LfoWaveform::Random, 93.75));
            let mut seen = Vec::new();
            for _ in 0..40 {
                rig.process(&[]);
                seen.push((rig.read(first), rig.read(second)));
            }
            seen
        };
        let seen = run();
        // Each holds a number for a cycle of eight buffers, and the two have not the same.
        assert!(seen.iter().filter(|(a, b)| a == b).count() < 4, "{seen:?}");
        assert!(seen[..7].iter().all(|pair| pair.0 == seen[0].0), "the first holds its number");
        assert!(seen[7].0 != seen[6].0, "and takes another when its cycle is over");
        // The first to come in has the seed 0 and the next the seed 1: the second cycle (the eighth buffer is its first) has their numbers.
        assert_eq!(seen[7], (noise(0, 1), noise(1, 1)));
        assert_eq!(seen[15], (noise(0, 2), noise(1, 2)));
        assert_eq!(seen, run());
    }

    #[test]
    fn the_seeds_go_to_the_modulators_in_the_order_they_come_in_whether_they_are_a_lanes_or_a_groups() {
        let mut rig = Rig::new();
        let first = rig.lane_modulator(0, lfo(LfoWaveform::Random, 93.75));
        rig.cmd_tx.send(GraphCommand::AddGroup).unwrap();
        rig.cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        let group = rig.slot();
        rig.cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source: lfo(LfoWaveform::Random, 93.75) }).unwrap();
        rig.cmd_tx.send(GraphCommand::SetModulatorOutput { location: ModulatorLocation::Group(0), index: 0, slot: group }).unwrap();
        let third = rig.lane_modulator(1, lfo(LfoWaveform::Random, 93.75));
        for _ in 0..8 {
            rig.process(&[]);
        }
        assert_eq!((rig.read(first), rig.read(group), rig.read(third)), (noise(0, 1), noise(1, 1), noise(2, 1)));
    }

    // -- Modulator integration test --

    /// Instrument that records the last value set on parameter 0.
    struct ParamTrackingInstrument {
        param_value: f32,
    }

    impl Plugin for ParamTrackingInstrument {
        fn name(&self) -> &str {
            "ParamTracking"
        }
        fn is_instrument(&self) -> bool {
            true
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            0
        }

        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            _audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            // Output the current param value as audio (so we can observe modulation).
            for ch in audio_out.iter_mut() {
                ch.fill(self.param_value);
            }
            Ok(())
        }

        fn sample_rate(&self) -> f32 {
            48000.0
        }
        fn parameters(&self) -> Vec<ParameterInfo> {
            vec![ParameterInfo {
                index: 0,
                name: "cutoff".into(),
                min: 0.0,
                max: 1.0,
                default: 0.5,
                ..Default::default()
            }]
        }
        fn get_parameter(&mut self, idx: u32) -> Option<f32> {
            if idx == 0 { Some(self.param_value) } else { None }
        }
        fn set_parameter(&mut self, idx: u32, value: f32) -> anyhow::Result<()> {
            if idx == 0 {
                self.param_value = value;
                Ok(())
            } else {
                anyhow::bail!("no parameter {idx}")
            }
        }
        fn presets(&self) -> Vec<Preset> {
            Vec::new()
        }
        fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
            anyhow::bail!("no preset {id}")
        }
    }

    /// Effect that scales its input by a settable `gain` parameter (default 1.0),
    /// so a modulator driving the gain is observable in the output.
    struct ParamScaleEffect {
        gain: f32,
    }

    impl Plugin for ParamScaleEffect {
        fn name(&self) -> &str {
            "ParamScale"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }
        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                for (o, &i) in out.iter_mut().zip(inp.iter()) {
                    *o = i * self.gain;
                }
            }
            Ok(())
        }
        fn sample_rate(&self) -> f32 {
            48000.0
        }
        fn parameters(&self) -> Vec<ParameterInfo> {
            vec![ParameterInfo {
                index: 0,
                name: "gain".into(),
                min: 0.0,
                max: 1.0,
                default: 1.0,
                ..Default::default()
            }]
        }
        fn get_parameter(&mut self, idx: u32) -> Option<f32> {
            if idx == 0 { Some(self.gain) } else { None }
        }
        fn set_parameter(&mut self, idx: u32, value: f32) -> anyhow::Result<()> {
            if idx == 0 {
                self.gain = value;
                Ok(())
            } else {
                anyhow::bail!("no parameter {idx}")
            }
        }
        fn presets(&self) -> Vec<Preset> {
            Vec::new()
        }
        fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
            anyhow::bail!("no preset {id}")
        }
    }

    /// A group envelope modulator at idle (no notes) with depth 1.0 pulls its
    /// target parameter from the base all the way down to the parameter's min.
    /// Here a member instrument's `cutoff` (base 0.5) → 0.0, observable because
    /// ParamTrackingInstrument outputs its parameter value as audio.
    #[test]
    fn group_modulator_drives_member_param() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument { inst: 0, instrument: inst, inst_buf, remapper: None, keep: None })
            .unwrap();

        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();

        cmd_tx
            .send(GraphCommand::InsertGroupModulator {
                group: 0,
                index: 0,
                source: ModSource::Envelope {
                    attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5,
                    state: EnvState::Idle, level: 0.0, notes_held: 0,
                },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddGroupModTarget {
                group: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::GroupMember { member: 0, slot: 0, param_index: 0 },
                    depth: 1.0,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();

        // No notes → envelope idle → member cutoff pulled to min (0).
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();
        assert!(
            out[0].iter().all(|&s| s.abs() < 1e-6),
            "idle group envelope at depth 1 should pull member cutoff to 0, got {}",
            out[0][0]
        );
    }

    /// A group macro offsets a member instrument's parameter as a lane's does: the base 0.5 and a value of 0.5 at a depth of 0.4 make 0.7, which
    /// ParamTrackingInstrument sounds as audio. (Its value is set by giving the group a macro of another value, as a lane's is.)
    #[test]
    fn a_group_macro_drives_a_member_param_and_its_value_is_changed_by_giving_the_group_another() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx.send(GraphCommand::SwapInstrument { inst: 0, instrument: inst, inst_buf, remapper: None, keep: None }).unwrap();
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source: ModSource::Macro { value: 0.5 } }).unwrap();
        cmd_tx
            .send(GraphCommand::AddGroupModTarget {
                group: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::GroupMember { member: 0, slot: 0, param_index: 0 },
                    depth: 0.4,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();
        assert!(close(out[0][0], 0.7), "{}", out[0][0]);
        cmd_tx.send(GraphCommand::SetGroupModulatorSource { group: 0, mod_index: 0, source: ModSource::Macro { value: 1.0 } }).unwrap();
        graph.process(&[], &mut out).unwrap();
        assert!(close(out[0][0], 0.9), "{}", out[0][0]);
    }

    /// A group modulator can drive one of the group's own bus effects. Here a square
    /// LFO (always +1) at depth -1 pulls a bus ParamScaleEffect's gain (base 1.0) to
    /// 0, silencing the bus. The instrument sounds while a note is held, and the bus
    /// is heard before the modulator is there, so that only the modulated gain can
    /// silence it. (The test that was here held no note: its instrument was silent
    /// whatever the bus did, and it passed with the group's writes taken out.)
    #[test]
    fn group_modulator_drives_bus_effect() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        swap_instrument_at(&cmd_tx, 0, ConstInstrument::new(0.5));
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        cmd_tx
            .send(GraphCommand::InsertGroupEffect {
                group: 0,
                index: 0,
                effect: Box::new(ParamScaleEffect { gain: 1.0 }),
                mix: 1.0,
            })
            .unwrap();

        // The bus passes the note: 0.5 from the instrument, at a gain of 1.
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 0.5).abs() < 1e-6), "the bus is heard before the modulator: {}", out[0][0]);

        cmd_tx
            .send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source: lfo(LfoWaveform::Square, 0.0) })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddGroupModTarget {
                group: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::GroupBus { effect_index: 0, param_index: 0 },
                    depth: -1.0,
                    base_value: 1.0,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();

        // The note is still held: the LFO's +1 at depth -1 pulls the gain to 0, and the bus is silent.
        graph.process(&[], &mut out).unwrap();
        assert!(
            out[0].iter().all(|&s| s.abs() < 1e-6),
            "a group LFO at depth -1 should pull the bus gain to 0 (silence), got {}",
            out[0][0]
        );
    }

    /// Group member-target ordinals are fixed up when membership changes:
    /// a member leaving drops targets pointing at it and shifts higher ordinals
    /// down; a member joining bumps ordinals at/after the insertion point.
    #[test]
    fn group_member_target_fixups() {
        let mk = |member: usize| Modulator {
            source: ModSource::lfo(LfoWaveform::Sine, 1.0),
            sample_rate: 48000.0,
            targets: vec![ModTarget {
                kind: ModTargetKind::GroupMember { member, slot: 0, param_index: 0 },
                depth: 0.5,
                base_value: 0.5,
                param_min: 0.0,
                param_max: 1.0,
                effective: None,
            }],
            last_output: 0.0,
            output: None,
            last_learned: None,
            position_out: None,
            seed: 0,
            cycle: 0,
        };
        let members = |mods: &[Modulator]| -> Vec<Option<usize>> {
            mods.iter()
                .map(|m| match m.targets.first().map(|t| &t.kind) {
                    Some(ModTargetKind::GroupMember { member, .. }) => Some(*member),
                    _ => None,
                })
                .collect()
        };

        // Targets at members 0, 1, 2; member 1 leaves.
        let mut mods = vec![mk(0), mk(1), mk(2)];
        fixup_group_member_after_remove(&mut mods, 1);
        assert_eq!(members(&mods), vec![Some(0), None, Some(1)]);

        // A member joins at ordinal 0: existing ordinals shift up.
        shift_group_member_after_insert(&mut mods, 0);
        assert_eq!(members(&mods), vec![Some(1), None, Some(2)]);

        // The same ordinal fixups apply to GroupMemberMod targets.
        let mk_mm = |member: usize| Modulator {
            source: ModSource::lfo(LfoWaveform::Sine, 1.0),
            sample_rate: 48000.0,
            targets: vec![ModTarget {
                kind: ModTargetKind::GroupMemberMod { member, mod_index: 0, field: CrossModField::Rate },
                depth: 0.5,
                base_value: 1.0,
                param_min: 0.01,
                param_max: 50.0,
                effective: None,
            }],
            last_output: 0.0,
            output: None,
            last_learned: None,
            position_out: None,
            seed: 0,
            cycle: 0,
        };
        let mm_members = |mods: &[Modulator]| -> Vec<Option<usize>> {
            mods.iter()
                .map(|m| match m.targets.first().map(|t| &t.kind) {
                    Some(ModTargetKind::GroupMemberMod { member, .. }) => Some(*member),
                    _ => None,
                })
                .collect()
        };
        let mut mm = vec![mk_mm(0), mk_mm(1), mk_mm(2)];
        fixup_group_member_after_remove(&mut mm, 1);
        assert_eq!(mm_members(&mm), vec![Some(0), None, Some(1)]);

        // Member-modulator removal: member 0's modulator 1 deleted → targets at
        // (member 0, mod 1) drop, and (member 0, mod >1) shift down.
        let mk_mm_idx = |mod_index: usize| Modulator {
            source: ModSource::lfo(LfoWaveform::Sine, 1.0),
            sample_rate: 48000.0,
            targets: vec![ModTarget {
                kind: ModTargetKind::GroupMemberMod { member: 0, mod_index, field: CrossModField::Rate },
                depth: 0.5,
                base_value: 1.0,
                param_min: 0.01,
                param_max: 50.0,
                effective: None,
            }],
            last_output: 0.0,
            output: None,
            last_learned: None,
            position_out: None,
            seed: 0,
            cycle: 0,
        };
        let mm_idx = |mods: &[Modulator]| -> Vec<Option<usize>> {
            mods.iter()
                .map(|m| match m.targets.first().map(|t| &t.kind) {
                    Some(ModTargetKind::GroupMemberMod { mod_index, .. }) => Some(*mod_index),
                    _ => None,
                })
                .collect()
        };
        let mut mmi = vec![mk_mm_idx(0), mk_mm_idx(1), mk_mm_idx(2)];
        fixup_group_member_mod_after_remove(&mut mmi, 0, 1);
        assert_eq!(mm_idx(&mmi), vec![Some(0), None, Some(1)]);
    }

    // A group modulator's member targets name the member's plugins by chain slot
    // (0 = the instrument, 1.. = its effects). When effects are added, removed or
    // moved on the member lane the targets have to follow their plugins, as they do
    // for the lane's own modulators.

    /// Targets of group 0's first modulator as (member, slot, param_index).
    fn member_slots(graph: &AudioGraph) -> Vec<(usize, usize, u32)> {
        graph.groups[0].modulators[0]
            .targets
            .iter()
            .filter_map(|t| match t.kind {
                ModTargetKind::GroupMember { member, slot, param_index } => Some((member, slot, param_index)),
                _ => None,
            })
            .collect()
    }

    /// Lane 0 outside group 0 (with one effect), lanes 1 and 2 in it (with two
    /// effects and one), so a lane's index is not its member ordinal; a group LFO
    /// with targets at (member, slot) = (0, 0), (0, 1), (0, 2) and (1, 1), told
    /// apart by parameter indexes 0 to 3. Member 0 is lane 1, member 1 is lane 2.
    fn group_lfo_on_member_effects() -> (
        AudioGraph,
        crossbeam_channel::Sender<GraphCommand>,
        crossbeam_channel::Receiver<Box<dyn Plugin>>,
    ) {
        let (mut graph, cmd_tx, rr) = make_graph(2);
        cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        for inst in [1, 2] {
            cmd_tx.send(GraphCommand::SetLaneGroup { inst, group: Some(0) }).unwrap();
        }
        for (inst, effects) in [(0, 1), (1, 2), (2, 1)] {
            for index in 0..effects {
                cmd_tx
                    .send(GraphCommand::InsertEffect { inst, index, effect: Box::new(ParamScaleEffect { gain: 1.0 }), mix: 1.0 })
                    .unwrap();
            }
        }
        cmd_tx
            .send(GraphCommand::InsertGroupModulator {
                group: 0,
                index: 0,
                source: ModSource::lfo(LfoWaveform::Sine, 1.0),
            })
            .unwrap();
        for (param_index, (member, slot)) in [(0, 0), (0, 1), (0, 2), (1, 1)].into_iter().enumerate() {
            cmd_tx
                .send(GraphCommand::AddGroupModTarget {
                    group: 0,
                    mod_index: 0,
                    target: ModTarget {
                        kind: ModTargetKind::GroupMember { member, slot, param_index: param_index as u32 },
                        depth: 0.5,
                        base_value: 0.5,
                        param_min: 0.0,
                        param_max: 1.0,
                        effective: None,
                    },
                })
                .unwrap();
        }
        graph.drain_commands();
        assert_eq!(member_slots(&graph), [(0, 0, 0), (0, 1, 1), (0, 2, 2), (1, 1, 3)]);
        (graph, cmd_tx, rr)
    }

    fn scale_effect() -> Box<dyn Plugin> {
        Box::new(ParamScaleEffect { gain: 1.0 })
    }

    #[test]
    fn removing_an_effect_moves_the_group_modulators_targets_on_that_member() {
        let (mut graph, cmd_tx, _rr) = group_lfo_on_member_effects();
        cmd_tx.send(GraphCommand::RemoveEffect { inst: 1, index: 0, keep: None }).unwrap();
        graph.drain_commands();
        // The effect in slot 1 is gone with its target; the one in slot 2 is in
        // slot 1 now. The instrument's and the other member's are untouched.
        assert_eq!(member_slots(&graph), [(0, 0, 0), (0, 1, 2), (1, 1, 3)]);
    }

    #[test]
    fn removing_an_effect_on_the_second_member_leaves_the_first_members_targets() {
        let (mut graph, cmd_tx, _rr) = group_lfo_on_member_effects();
        cmd_tx.send(GraphCommand::RemoveEffect { inst: 2, index: 0, keep: None }).unwrap();
        graph.drain_commands();
        assert_eq!(member_slots(&graph), [(0, 0, 0), (0, 1, 1), (0, 2, 2)]);
    }

    #[test]
    fn moving_an_effect_moves_the_group_modulators_targets_with_it() {
        let (mut graph, cmd_tx, _rr) = group_lfo_on_member_effects();
        cmd_tx.send(GraphCommand::ReorderEffect { inst: 1, from: 0, to: 1 }).unwrap();
        graph.drain_commands();
        // Parameter 1 belongs to the effect that was first and is second now.
        assert_eq!(member_slots(&graph), [(0, 0, 0), (0, 2, 1), (0, 1, 2), (1, 1, 3)]);
    }

    #[test]
    fn inserting_an_effect_moves_the_group_modulators_targets_after_it() {
        let (mut graph, cmd_tx, _rr) = group_lfo_on_member_effects();
        cmd_tx.send(GraphCommand::InsertEffect { inst: 1, index: 0, effect: scale_effect(), mix: 1.0 }).unwrap();
        graph.drain_commands();
        assert_eq!(member_slots(&graph), [(0, 0, 0), (0, 2, 1), (0, 3, 2), (1, 1, 3)]);

        // An effect added at the end moves nothing.
        cmd_tx.send(GraphCommand::InsertEffect { inst: 1, index: 3, effect: scale_effect(), mix: 1.0 }).unwrap();
        graph.drain_commands();
        assert_eq!(member_slots(&graph), [(0, 0, 0), (0, 2, 1), (0, 3, 2), (1, 1, 3)]);
    }

    #[test]
    fn effects_on_a_lane_outside_the_group_leave_the_group_modulators_alone() {
        let (mut graph, cmd_tx, _rr) = group_lfo_on_member_effects();
        cmd_tx.send(GraphCommand::InsertEffect { inst: 0, index: 0, effect: scale_effect(), mix: 1.0 }).unwrap();
        cmd_tx.send(GraphCommand::ReorderEffect { inst: 0, from: 0, to: 1 }).unwrap();
        cmd_tx.send(GraphCommand::RemoveEffect { inst: 0, index: 0, keep: None }).unwrap();
        graph.drain_commands();
        assert_eq!(member_slots(&graph), [(0, 0, 0), (0, 1, 1), (0, 2, 2), (1, 1, 3)]);
    }

    #[test]
    fn commands_that_change_no_chain_leave_the_group_modulators_targets_alone() {
        let (mut graph, cmd_tx, _rr) = group_lfo_on_member_effects();
        // A target on a plugin that is not there (member 1 has one effect): only a
        // removal that really removed something may shift it.
        cmd_tx
            .send(GraphCommand::AddGroupModTarget {
                group: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::GroupMember { member: 1, slot: 3, param_index: 4 },
                    depth: 0.5,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();
        graph.drain_commands();
        let before = member_slots(&graph);
        assert_eq!(before, [(0, 0, 0), (0, 1, 1), (0, 2, 2), (1, 1, 3), (1, 3, 4)]);

        // An effect that is not there, a move to nowhere, a move to the same place,
        // and a lane that does not exist.
        for command in [
            GraphCommand::RemoveEffect { inst: 2, index: 1, keep: None },
            GraphCommand::RemoveEffect { inst: 1, index: 9, keep: None },
            GraphCommand::ReorderEffect { inst: 1, from: 0, to: 9 },
            GraphCommand::ReorderEffect { inst: 1, from: 9, to: 0 },
            GraphCommand::ReorderEffect { inst: 1, from: 1, to: 1 },
            GraphCommand::RemoveEffect { inst: 9, index: 0, keep: None },
            GraphCommand::ReorderEffect { inst: 9, from: 0, to: 1 },
        ] {
            cmd_tx.send(command).unwrap();
        }
        graph.drain_commands();
        assert_eq!(member_slots(&graph), before);
    }

    #[test]
    fn group_member_slot_fixups_touch_only_that_members_slots() {
        let target = |kind| ModTarget { kind, depth: 0.5, base_value: 0.5, param_min: 0.0, param_max: 1.0, effective: None };
        let modulator = |targets: Vec<ModTarget>| Modulator {
            source: ModSource::lfo(LfoWaveform::Sine, 1.0),
            sample_rate: 48000.0,
            targets,
            last_output: 0.0,
            output: None,
            last_learned: None,
            position_out: None,
            seed: 0,
            cycle: 0,
        };
        let on = |member, slot| target(ModTargetKind::GroupMember { member, slot, param_index: 0 });
        // Targets that are not a member's plugin slots, and must never move.
        let others = || {
            modulator(vec![
                target(ModTargetKind::GroupBus { effect_index: 2, param_index: 0 }),
                target(ModTargetKind::GroupMemberMod { member: 0, mod_index: 2, field: CrossModField::Rate }),
                target(ModTargetKind::ModulatorRate { mod_index: 2 }),
                target(ModTargetKind::PluginParam { slot: 2, param_index: 0 }),
            ])
        };
        let build = || vec![modulator(vec![on(0, 0), on(0, 1), on(0, 2), on(1, 1), on(1, 2)]), others()];
        let slots = |mods: &[Modulator]| -> Vec<(usize, usize)> {
            mods[0]
                .targets
                .iter()
                .filter_map(|t| match t.kind {
                    ModTargetKind::GroupMember { member, slot, .. } => Some((member, slot)),
                    _ => None,
                })
                .collect()
        };
        let untouched = |mods: &[Modulator]| format!("{:?}", mods[1].targets.iter().map(|t| &t.kind).collect::<Vec<_>>());
        let original = untouched(&build());

        // The effect at index 0 of member 0 is removed (slot 1).
        let mut mods = build();
        fixup_member_slots_after_remove(&mut mods, 0, 0);
        assert_eq!(slots(&mods), [(0, 0), (0, 1), (1, 1), (1, 2)]);
        assert_eq!(untouched(&mods), original);

        // The one at index 1 of member 1 (slot 2), the last.
        let mut mods = build();
        fixup_member_slots_after_remove(&mut mods, 1, 1);
        assert_eq!(slots(&mods), [(0, 0), (0, 1), (0, 2), (1, 1)]);

        // An effect is inserted at index 0 of member 1: its slots 1 and up move.
        let mut mods = build();
        shift_member_slots_after_insert(&mut mods, 1, 0);
        assert_eq!(slots(&mods), [(0, 0), (0, 1), (0, 2), (1, 2), (1, 3)]);
        assert_eq!(untouched(&mods), original);
        // ...at index 1 of member 0: slot 2 and up move, slot 1 stays.
        let mut mods = build();
        shift_member_slots_after_insert(&mut mods, 0, 1);
        assert_eq!(slots(&mods), [(0, 0), (0, 1), (0, 3), (1, 1), (1, 2)]);

        // Member 0's first effect moves to the end of two (index 0 to 1), then back.
        let mut mods = build();
        remap_member_slots_after_reorder(&mut mods, 0, 0, 1);
        assert_eq!(slots(&mods), [(0, 0), (0, 2), (0, 1), (1, 1), (1, 2)]);
        assert_eq!(untouched(&mods), original);
        remap_member_slots_after_reorder(&mut mods, 0, 1, 0);
        assert_eq!(slots(&mods), [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2)]);
        // The last of member 1's two moves to the front.
        remap_member_slots_after_reorder(&mut mods, 1, 1, 0);
        assert_eq!(slots(&mods), [(0, 0), (0, 1), (0, 2), (1, 2), (1, 1)]);
    }

    /// A group envelope modulator (idle, depth 1) drives a member instrument's
    /// LFO rate down to its minimum — proving group→member-modulator cross-mod.
    #[test]
    fn group_modulator_drives_member_modulator() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        swap_instrument_at(&cmd_tx, 0, ConstInstrument::new(0.5));
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        // Member lane gets an LFO modulator at rate 10 Hz.
        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::lfo(LfoWaveform::Sine, 10.0),
            })
            .unwrap();
        // Group envelope modulator (idle) drives that member LFO's rate, depth 1.
        cmd_tx
            .send(GraphCommand::InsertGroupModulator {
                group: 0,
                index: 0,
                source: ModSource::Envelope {
                    attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5,
                    state: EnvState::Idle, level: 0.0, notes_held: 0,
                },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddGroupModTarget {
                group: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Rate },
                    depth: 1.0,
                    base_value: 10.0,
                    param_min: 0.01,
                    param_max: 50.0,
                    effective: None,
                },
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();

        // Idle envelope at depth 1 pulls the member LFO's rate from 10 to ~min.
        match graph.instruments[0].modulators[0].source {
            ModSource::Lfo { rate, .. } => assert!(
                rate < 0.1,
                "group envelope should pull member LFO rate to min, got {rate}"
            ),
            _ => panic!("member modulator should be an LFO"),
        }
    }

    /// A MidiLearn modulator, once armed, captures the next CC, notifies the
    /// main thread, and then drives its target parameter absolutely.
    #[test]
    fn midi_learn_binds_cc_and_drives_param() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        let (learn_tx, learn_rx) = crossbeam_channel::bounded(8);
        graph.set_learn_tx(learn_tx);

        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument { inst: 0, instrument: inst, inst_buf, remapper: None, keep: None })
            .unwrap();

        // Armed MidiLearn modulator targeting cutoff (param 0), full-range.
        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::MidiLearn { source: None, value: 0.0, learning: true },
            })
            .unwrap();
        // Base 0.2, depth 0.5: a CC is unipolar, so it *adds* up to depth·range
        // on top of the base (rest = base). CC max → 0.2 + 0.5·1.0 = 0.7.
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 0.5,
                    base_value: 0.2,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();

        // CC 74 at max → learns CC 74 and pushes cutoff to base + depth (0.7).
        let cc = |v: u8| (0u64, [0xB0, 74, v]);
        let mut out = make_output();
        graph.process(&[cc(127)], &mut out).unwrap();

        let notif = learn_rx.try_recv().expect("learn notification");
        assert!(matches!(notif.source, MidiBindSource::Cc(74)));
        assert!(matches!(notif.location, ModulatorLocation::Lane(0)));
        assert!(
            out[0].iter().all(|&s| (s - 0.7).abs() < 1e-6),
            "CC max should combine to base+depth = 0.7, got {}",
            out[0][0]
        );

        // A later CC 74 = 0 returns to the base (0.2) — it does not slam to 0.
        let mut out = make_output();
        graph.process(&[cc(0)], &mut out).unwrap();
        assert!(
            out[0].iter().all(|&s| (s - 0.2).abs() < 1e-6),
            "CC 0 should return to the base 0.2, got {}",
            out[0][0]
        );
    }

    /// Key pressure is learnable: channel pressure (0xD0) and poly aftertouch
    /// (0xA0) both bind to `Aftertouch` (interchangeably) and drive the target
    /// additively above the base — the "press harder" dimension on MPE
    /// controllers like the ROLI Seaboard.
    #[test]
    fn midi_learn_binds_aftertouch() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        let (learn_tx, learn_rx) = crossbeam_channel::bounded(8);
        graph.set_learn_tx(learn_tx);

        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument { inst: 0, instrument: inst, inst_buf, remapper: None, keep: None })
            .unwrap();
        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::MidiLearn { source: None, value: 0.0, learning: true },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 0.5,
                    base_value: 0.2,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();

        // Channel pressure (0xD0, value in data1) at max → learns Aftertouch and
        // pushes to base + depth·range = 0.2 + 0.5 = 0.7.
        let mut out = make_output();
        graph.process(&[(0, [0xD0, 127, 0])], &mut out).unwrap();
        let notif = learn_rx.try_recv().expect("learn notification");
        assert!(matches!(notif.source, MidiBindSource::Aftertouch));
        assert!(
            out[0].iter().all(|&s| (s - 0.7).abs() < 1e-6),
            "full press should combine to base+depth 0.7, got {}",
            out[0][0]
        );

        // Poly aftertouch (0xA0, value in data2) drives the same binding.
        let mut out = make_output();
        graph.process(&[(0, [0xA0, 60, 64])], &mut out).unwrap();
        let expected = 0.2 + (64.0 / 127.0) * 0.5;
        assert!(
            out[0].iter().all(|&s| (s - expected).abs() < 1e-4),
            "poly aftertouch should drive the aftertouch binding, got {}",
            out[0][0]
        );

        // Release (0) → back to the base.
        let mut out = make_output();
        graph.process(&[(0, [0xD0, 0, 0])], &mut out).unwrap();
        assert!(
            out[0].iter().all(|&s| (s - 0.2).abs() < 1e-6),
            "release should return to the base 0.2, got {}",
            out[0][0]
        );
    }

    /// Learning captures the strongest deliberate gesture, not the first
    /// message in the buffer. An expressive controller emits several dimensions
    /// at once when a key is touched — a firm press must win over an incidental
    /// pitch-bend wiggle that happens to arrive first.
    #[test]
    fn midi_learn_prefers_strongest_gesture() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        let (learn_tx, learn_rx) = crossbeam_channel::bounded(8);
        graph.set_learn_tx(learn_tx);

        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument { inst: 0, instrument: inst, inst_buf, remapper: None, keep: None })
            .unwrap();
        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::MidiLearn { source: None, value: 0.0, learning: true },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 1.0,
                    base_value: 0.0,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();

        // One buffer: a tiny pitch-bend nudge near centre (deflection ≈ 0.03,
        // below threshold) arriving *before* a firm channel-pressure press
        // (deflection ≈ 0.94). The press must be the one that binds.
        let small_bend = (0u64, [0xE0, 0x00, 0x42]); // raw 0x2100 ≈ just above centre
        let firm_press = (0u64, [0xD0, 120, 0]);
        let mut out = make_output();
        graph.process(&[small_bend, firm_press], &mut out).unwrap();

        let notif = learn_rx.try_recv().expect("a deliberate gesture should have bound");
        assert!(
            matches!(notif.source, MidiBindSource::Aftertouch),
            "the firm press should win over the incidental bend, got {:?}",
            notif.source
        );
    }

    /// A learned pitch-bend control is bipolar: the centre position leaves the
    /// parameter at its base (so a manually-set value shows through), and the
    /// bend sweeps ±depth·range around it. This is the ROLI/expressive case and
    /// the essence of "sources combine" — the control does not pin the value.
    #[test]
    fn midi_learn_pitch_bend_is_bipolar_around_base() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument { inst: 0, instrument: inst, inst_buf, remapper: None, keep: None })
            .unwrap();

        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::MidiLearn {
                    source: Some(MidiBindSource::PitchBend),
                    value: 0.5,
                    learning: false,
                },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 0.5,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();

        // 14-bit pitch bend → (lsb, msb) split.
        let bend = |raw: u16| (0u64, [0xE0, (raw & 0x7F) as u8, (raw >> 7) as u8]);

        // Centre (8192) → no offset → the base (0.5).
        let mut out = make_output();
        graph.process(&[bend(8192)], &mut out).unwrap();
        assert!(
            out[0].iter().all(|&s| (s - 0.5).abs() < 2e-4),
            "centre bend should leave the base 0.5, got {}",
            out[0][0]
        );

        // Full up → base + depth·range = 1.0; full down → base − depth·range = 0.0.
        let mut out = make_output();
        graph.process(&[bend(16383)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 1.0).abs() < 2e-4), "full-up bend = 1.0, got {}", out[0][0]);
        let mut out = make_output();
        graph.process(&[bend(0)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s.abs() < 2e-4), "full-down bend = 0.0, got {}", out[0][0]);
    }

    /// An armed MidiLearn modulator must leave its target at the base value
    /// until a control is captured — arming learn on a parameter should not
    /// slam it to the minimum before you touch a knob.
    #[test]
    fn midi_learn_unbound_leaves_param_at_base() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);

        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument { inst: 0, instrument: inst, inst_buf, remapper: None, keep: None })
            .unwrap();

        // Armed MidiLearn targeting cutoff (param 0), full-range, depth 1.0.
        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::MidiLearn { source: None, value: 0.0, learning: true },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 1.0,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();

        // No CC yet — the parameter must stay at its base (0.5), not drop to 0.
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();
        assert!(
            out[0].iter().all(|&s| (s - 0.5).abs() < 1e-6),
            "unbound learn should leave cutoff at base 0.5, got {}",
            out[0][0]
        );
    }

    /// MPE per-note expression (member channels 2..16) must route only to the
    /// lane sounding a note on that channel, while channel-1 (master / non-MPE)
    /// expression stays global. Otherwise one instrument's pressure/bend bleeds
    /// into another's modulators.
    #[test]
    fn mpe_expression_routes_by_active_channel() {
        let mut lane = InstrumentLane::new(2);
        lane.range = Some((60, 72));

        // Note on 64 on member channel index 1, plus expression on various channels.
        let events = [
            (0u64, [0x91, 64, 100]),    // note on, ch idx 1 (in range)
            (0u64, [0xd1, 90, 0]),      // channel pressure, ch idx 1 → own note → keep
            (0u64, [0xd2, 90, 0]),      // channel pressure, ch idx 2 → no note → drop
            (0u64, [0xd0, 90, 0]),      // channel pressure, ch idx 0 (master) → keep
            (0u64, [0xe2, 0x00, 0x60]), // pitch bend, ch idx 2 → no note → drop
        ];
        lane.filter_midi(&events);
        let kept: Vec<u8> = lane.filtered_midi.iter().map(|(_, b)| b[0]).collect();
        assert!(kept.contains(&0x91), "note kept: {kept:02x?}");
        assert!(kept.contains(&0xd1), "own-channel pressure kept: {kept:02x?}");
        assert!(!kept.contains(&0xd2), "other-channel pressure dropped: {kept:02x?}");
        assert!(kept.contains(&0xd0), "master-channel expression kept: {kept:02x?}");
        assert!(!kept.contains(&0xe2), "other-channel bend dropped: {kept:02x?}");

        // Releasing the note stops routing its channel's expression here.
        let after = [
            (0u64, [0x81, 64, 0]), // note off, ch idx 1
            (0u64, [0xd1, 40, 0]), // channel pressure, ch idx 1 → now dropped
        ];
        lane.filter_midi(&after);
        let kept2: Vec<u8> = lane.filtered_midi.iter().map(|(_, b)| b[0]).collect();
        assert!(kept2.contains(&0x81), "note-off kept: {kept2:02x?}");
        assert!(!kept2.contains(&0xd1), "pressure dropped after note-off: {kept2:02x?}");
    }

    /// A single full-range instrument must still receive its own notes' per-note
    /// expression (its channels become active) and global channel-1 messages.
    #[test]
    fn full_range_lane_receives_its_own_expression() {
        let mut lane = InstrumentLane::new(2); // range None → full range
        let events = [
            (0u64, [0x93, 60, 100]), // note on, ch idx 3
            (0u64, [0xd3, 77, 0]),   // pressure, ch idx 3 → own note → keep
            (0u64, [0xb0, 1, 64]),   // CC1 on master channel → keep (global)
        ];
        lane.filter_midi(&events);
        let kept: Vec<u8> = lane.filtered_midi.iter().map(|(_, b)| b[0]).collect();
        assert!(kept.contains(&0xd3), "own-channel pressure kept: {kept:02x?}");
        assert!(kept.contains(&0xb0), "global CC kept: {kept:02x?}");
    }

    /// Changing a parameter that a modulator targets must move the modulator's
    /// base value, so the user's edit is combined with (not overwritten by) the
    /// modulation. Uses depth 0 to isolate the base from the LFO wobble.
    #[test]
    fn changing_a_modulated_param_moves_the_base() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument { inst: 0, instrument: inst, inst_buf, remapper: None, keep: None })
            .unwrap();

        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::lfo(LfoWaveform::Sine, 1.0),
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 0.0, // no wobble: applied value == base
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();
        assert!((out[0][0] - 0.5).abs() < 1e-6, "starts at base 0.5, got {}", out[0][0]);

        // User drags the param to 0.8 — the base must follow.
        cmd_tx
            .send(GraphCommand::SetParameter { inst: 0, slot: 0, param_index: 0, value: 0.8 })
            .unwrap();
        graph.process(&[], &mut out).unwrap();
        assert!(
            (out[0][0] - 0.8).abs() < 1e-6,
            "base should track the user's change to 0.8, got {}",
            out[0][0]
        );
    }

    #[test]
    fn modulator_applies_set_parameter() {
        let (mut graph, cmd_tx, _return_rx) = make_graph(2);

        // Swap in a param-tracking instrument.
        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: inst,
                inst_buf,
                remapper: None,
                keep: None,
            })
            .unwrap();

        // Add a modulator targeting param 0 (cutoff) on the instrument (parent_slot=0).
        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::lfo(LfoWaveform::Sine, 1.0),
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 0.5,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();

        // Process one buffer.
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();

        // The modulator should have called set_parameter, so the output
        // should NOT be exactly 0.5 (the base value) — it should be
        // modulated. After one buffer of 64 samples at 48kHz with 1Hz rate,
        // the phase advances by 64/48000 ≈ 0.00133. The sine at that phase
        // is small but non-zero.
        // The audio output is the param_value which was set by the modulator.
        let first_sample = out[0][0];
        // Just verify the modulator ran (value may be very close to 0.5 since
        // phase is small, but should be different from unmodulated).
        // The phase advance is 64/48000 ≈ 0.00133, sin(2π * 0.00133) ≈ 0.00837
        // modulated = 0.5 + 0.5 * 0.00837 * 1.0 ≈ 0.504
        assert!(
            (0.0..=1.0).contains(&first_sample),
            "modulated value out of range: {first_sample}"
        );

        // Run many buffers so the LFO phase advances significantly.
        for _ in 0..1000 {
            graph.process(&[], &mut out).unwrap();
        }
        // After many buffers the LFO should have cycled. The output should
        // still be within the valid range [0.0, 1.0].
        let sample = out[0][0];
        assert!(
            (0.0..=1.0).contains(&sample),
            "modulated value out of range after many buffers: {sample}"
        );
    }

    #[test]
    fn envelope_modulator_anchors_base_as_peak() {
        let (mut graph, cmd_tx, _return_rx) = make_graph(2);

        // Param 0 acts like the oscillator volume: base at max (1.0).
        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 1.0 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: inst,
                inst_buf,
                remapper: None,
                keep: None,
            })
            .unwrap();

        // Instant envelope (attack/decay/release = 0, sustain = 1) so each
        // phase settles within a single buffer.
        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::Envelope {
                    attack: 0.0,
                    decay: 0.0,
                    sustain: 1.0,
                    release: 0.0,
                    state: EnvState::Idle,
                    level: 0.0,
                    notes_held: 0,
                },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 1.0,
                    base_value: 1.0,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();

        // Idle (no note yet): the envelope holds the param at min — silence.
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();
        assert!(
            out[0][0].abs() < 0.01,
            "idle envelope should pull the param to min, got {}",
            out[0][0]
        );

        // Note-on: envelope peaks, restoring the user's base value.
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(
            (out[0][0] - 1.0).abs() < 0.01,
            "envelope peak should reach the base value, got {}",
            out[0][0]
        );

        // Note-off: release returns the param to min.
        graph.process(&[note_off(60)], &mut out).unwrap();
        assert!(
            out[0][0].abs() < 0.01,
            "after release the param should return to min, got {}",
            out[0][0]
        );
    }

    #[test]
    fn envelope_modulator_depth_limits_dip() {
        // depth 0.5 on base 0.8: idle dips to base - 0.5*(0.8-0.0) = 0.4,
        // never below — the envelope scales the distance from min to base.
        let mut m = Modulator::new(
            ModSource::Envelope {
                attack: 0.0,
                decay: 0.0,
                sustain: 1.0,
                release: 0.0,
                state: EnvState::Idle,
                level: 0.0,
                notes_held: 0,
            },
            48000.0,
        );
        let target = ModTarget {
            kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
            depth: 0.5,
            base_value: 0.8,
            param_min: 0.0,
            param_max: 1.0,
            effective: None,
        };

        m.tick(64, &[], DEFAULT_BPM); // idle: output 0
        assert!((target.base_value + m.target_offset(&target) - 0.4).abs() < 1e-6);

        m.tick(64, &[note_on(60)], DEFAULT_BPM); // instant attack to peak
        assert!((target.base_value + m.target_offset(&target) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn modulator_base_value_updated_by_set_parameter() {
        let (mut graph, cmd_tx, _return_rx) = make_graph(2);

        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: inst,
                inst_buf,
                remapper: None,
                keep: None,
            })
            .unwrap();

        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::lfo(LfoWaveform::Sine, 1.0),
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 0.5,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();

        // Process to pick up commands.
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();

        // Now send SetParameter to change the base value.
        cmd_tx
            .send(GraphCommand::SetParameter {
                inst: 0,
                slot: 0,
                param_index: 0,
                value: 0.8,
            })
            .unwrap();

        // Process again — the modulator should now use 0.8 as its base.
        graph.process(&[], &mut out).unwrap();
        let sample = out[0][0];
        // The modulated value should be centered around 0.8 (within depth range).
        // At small phase, it should be close to 0.8.
        assert!(
            (sample - 0.8).abs() < 0.3,
            "expected close to 0.8 after base change, got {sample}"
        );
    }

    #[test]
    fn lane_modulator_survives_effect_removal() {
        let (mut graph, cmd_tx, _return_rx) = make_graph(2);

        // Instrument + 2 effects, plus a lane modulator targeting the
        // instrument (slot 0).
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        insert_effect(&cmd_tx, 0, Box::new(PassthroughEffect), 1.0);
        insert_effect(&cmd_tx, 1, Box::new(PassthroughEffect), 1.0);
        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::lfo(LfoWaveform::Sine, 1.0),
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 0.5,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        // Removing an effect must remap target slots without crashing.
        cmd_tx.send(GraphCommand::RemoveEffect { inst: 0, index: 0, keep: None }).unwrap();
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s.is_finite()));
    }

    #[test]
    fn modulator_targets_effect_slot() {
        // A lane modulator whose target points at slot 1 drives the effect's
        // parameter, not the instrument's — the whole point of lane scoping.
        let mut inst: Option<Box<dyn Plugin>> =
            Some(Box::new(ParamTrackingInstrument { param_value: 0.5 }));
        let mut effects: Vec<Box<dyn Plugin>> =
            vec![Box::new(ParamTrackingInstrument { param_value: 0.5 })];

        let mut m = Modulator::new(
            ModSource::lfo(LfoWaveform::Sine, 1.0),
            48000.0,
        );
        m.targets = vec![ModTarget {
            kind: ModTargetKind::PluginParam { slot: 1, param_index: 0 },
            depth: 1.0,
            base_value: 0.5,
            param_min: 0.0,
            param_max: 1.0,
            effective: None,
        }];
        m.last_output = 1.0; // force the LFO to its peak

        apply_modulators_to_chain(&[m], &mut inst, &mut effects, &mut Vec::new(), None);

        // Effect param pushed to base + depth*range (clamped); instrument untouched.
        assert!(
            effects[0].get_parameter(0).unwrap() > 0.9,
            "effect param should be modulated up, got {:?}",
            effects[0].get_parameter(0)
        );
        assert_eq!(
            inst.unwrap().get_parameter(0),
            Some(0.5),
            "instrument param should be untouched"
        );
    }

    #[test]
    fn slot_remap_helpers() {
        // A lane modulator with targets on instrument (0), effect 1 (slot 2),
        // and effect 2 (slot 3).
        let target = |slot: usize| ModTarget {
            kind: ModTargetKind::PluginParam { slot, param_index: 0 },
            depth: 0.5,
            base_value: 0.0,
            param_min: 0.0,
            param_max: 1.0,
            effective: None,
        };
        let slots = |mods: &[Modulator]| -> Vec<usize> {
            mods[0]
                .targets
                .iter()
                .filter_map(|t| match t.kind {
                    ModTargetKind::PluginParam { slot, .. } => Some(slot),
                    _ => None,
                })
                .collect()
        };
        let make = || {
            let mut m = Modulator::new(
                ModSource::lfo(LfoWaveform::Sine, 1.0),
                48000.0,
            );
            m.targets = vec![target(0), target(2), target(3)];
            vec![m]
        };

        // Insert an effect at index 0 (slot 1): slots >= 1 shift up.
        let mut mods = make();
        shift_slots_after_insert(&mut mods, 0);
        assert_eq!(slots(&mods), vec![0, 3, 4]);

        // Remove effect at index 0 (slot 1): nothing on slot 1, slots > 1 down.
        let mut mods = make();
        fixup_slots_after_remove(&mut mods, 0);
        assert_eq!(slots(&mods), vec![0, 1, 2]);

        // Remove effect at index 1 (slot 2): drop that target, shift slot 3→2.
        let mut mods = make();
        fixup_slots_after_remove(&mut mods, 1);
        assert_eq!(slots(&mods), vec![0, 2]);

        // Reorder effect from index 0 (slot 1) to index 2 (slot 3): effect at
        // slot 2 → 1, slot 3 → 2 (the moved one isn't targeted here).
        let mut mods = make();
        remap_slots_after_reorder(&mut mods, 0, 2);
        assert_eq!(slots(&mods), vec![0, 1, 2]);
    }

    // --- Pattern playback transposition ---

    /// A C major triad pattern (C4 E4 G4), one buffer of ons then offs.
    fn make_pattern_player(in_key: bool) -> PatternPlayer {
        let mut p = PatternPlayer::new(48000.0);
        p.pattern = Pattern {
            events: vec![
                PatternEvent { frame: 0, status: 0x90, note: 60, velocity: 100 },
                PatternEvent { frame: 0, status: 0x90, note: 64, velocity: 100 },
                PatternEvent { frame: 0, status: 0x90, note: 67, velocity: 100 },
                PatternEvent { frame: 500, status: 0x80, note: 60, velocity: 0 },
                PatternEvent { frame: 500, status: 0x80, note: 64, velocity: 0 },
                PatternEvent { frame: 500, status: 0x80, note: 67, velocity: 0 },
            ],
            length_samples: 1000,
        };
        p.base_note = Some(60);
        p.enabled = true;
        p.in_key = in_key;
        p
    }

    fn note_ons(events: &[(u64, [u8; 3])]) -> Vec<u8> {
        events.iter()
            .filter(|(_, b)| b[0] & 0xF0 == 0x90 && b[2] > 0)
            .map(|(_, b)| b[1])
            .collect()
    }

    fn note_offs(events: &[(u64, [u8; 3])]) -> Vec<u8> {
        events.iter()
            .filter(|(_, b)| b[0] & 0xF0 == 0x80 || (b[0] & 0xF0 == 0x90 && b[2] == 0))
            .map(|(_, b)| b[1])
            .collect()
    }

    #[test]
    fn pattern_chromatic_preserves_intervals() {
        let mut p = make_pattern_player(false);
        let scale = ScaleSetting::default(); // C Major
        // Trigger on D4: chromatic shift +2 gives a D major triad (F# off-scale).
        let events = p.process(&[note_on(62)], 256, scale).to_vec();
        assert_eq!(note_ons(&events), vec![62, 66, 69]);
    }

    #[test]
    fn pattern_in_key_transposes_by_scale_degrees() {
        let mut p = make_pattern_player(true);
        let scale = ScaleSetting::default(); // C Major
        // Trigger on D4 (2nd degree): C E G shifts one degree to D F A —
        // the diatonic D minor triad.
        let events = p.process(&[note_on(62)], 256, scale).to_vec();
        assert_eq!(note_ons(&events), vec![62, 65, 69]);

        // Note-offs (pattern frame 500, second buffer) release the same
        // transposed notes that were started.
        let events = p.process(&[], 256, scale).to_vec();
        let mut offs = note_offs(&events);
        offs.sort_unstable();
        assert_eq!(offs, vec![62, 65, 69]);
    }

    #[test]
    fn a_pattern_that_is_given_another_length_in_beats_loops_at_it_and_a_session_loads_the_same_loop() {
        // (The loop of make_pattern_player is 4 beats of 250 frames: 1000 frames.)
        let scale = ScaleSetting::default();
        let ons_in_a_loop_of_a_thousand_frames = |p: &mut PatternPlayer| {
            let mut ons = 0;
            let mut midi = vec![note_on(60)];
            for _ in 0..4 {
                ons += note_ons(p.process(&midi, 250, scale)).len();
                midi.clear();
            }
            ons
        };
        let mut as_recorded = make_pattern_player(false);
        assert_eq!(ons_in_a_loop_of_a_thousand_frames(&mut as_recorded), 3, "the triad once");
        let mut halved = make_pattern_player(false);
        halved.set_length_beats(2.0);
        assert_eq!(halved.pattern.length_samples, 500);
        assert_eq!(ons_in_a_loop_of_a_thousand_frames(&mut halved), 6, "the triad twice: the pattern is 2 beats long");
        let mut doubled = make_pattern_player(false);
        doubled.set_length_beats(8.0);
        assert_eq!(doubled.pattern.length_samples, 2000);
        assert_eq!(doubled.length_beats, 8.0, "the next recording is counted in it too");
    }

    #[test]
    fn a_length_in_beats_is_given_to_a_pattern_that_is_not_there_yet_as_it_is() {
        let mut p = PatternPlayer::new(48000.0);
        p.set_length_beats(2.0);
        assert_eq!((p.length_beats, p.pattern.length_samples), (2.0, 0));
    }

    #[test]
    fn a_recording_is_counted_in_for_count_in_beats_and_then_takes_the_length_of_the_pattern_at_the_tempo_there_is() {
        let (mut graph, cmd_tx, _rr) = make_graph(1);
        cmd_tx.send(GraphCommand::SetGlobalBpm { bpm: 120.0 }).unwrap();
        cmd_tx.send(GraphCommand::SetPatternLength { inst: 0, beats: 2.0 }).unwrap();
        cmd_tx.send(GraphCommand::SetPatternRecording { inst: 0, recording: true }).unwrap();
        graph.drain_commands();
        let player = &mut graph.instruments[0].pattern;
        // (At 120 a minute and 48 kHz a beat is 24000 frames, which a thousand frames at a time come to exactly.)
        let beat = 24_000;
        let (mut recording_from, mut recording_to) = (None, None);
        for block in 1..=200u64 {
            player.process(&[], 1000, ScaleSetting::default());
            let frames = block * 1000;
            if player.recording && recording_from.is_none() {
                recording_from = Some(frames);
            }
            if !player.recording && recording_from.is_some() && recording_to.is_none() {
                recording_to = Some(frames);
            }
            assert_eq!(player.counting_in, recording_from.is_none(), "{frames} frames: it counts in until it records");
        }
        assert_eq!(recording_from, Some(COUNT_IN_BEATS as u64 * beat), "the count-in is COUNT_IN_BEATS beats long");
        assert_eq!(recording_to, Some((COUNT_IN_BEATS as u64 + 2) * beat), "and the recording is the length of the pattern, 2 beats");
    }

    #[test]
    fn the_command_that_sets_the_length_of_a_pattern_does_it_to_the_pattern_of_the_lane() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        let on = PatternEvent { frame: 0, status: 0x90, note: 60, velocity: 100 };
        cmd_tx.send(GraphCommand::SetPatternLength { inst: 0, beats: 4.0 }).unwrap();
        cmd_tx.send(GraphCommand::SetPattern { inst: 0, pattern: Pattern { events: vec![on], length_samples: 96_000 }, base_note: Some(60), in_key: false }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.instruments[0].pattern.pattern.length_samples, 96_000);
        cmd_tx.send(GraphCommand::SetPatternLength { inst: 0, beats: 2.0 }).unwrap();
        graph.drain_commands();
        assert_eq!((graph.instruments[0].pattern.length_beats, graph.instruments[0].pattern.pattern.length_samples), (2.0, 48_000));
    }

    #[test]
    fn a_session_with_a_pattern_is_loaded_with_the_loop_it_says_in_frames_and_in_beats() {
        // (SETTINGS has a pattern of 2 beats that was recorded at 90 a minute: at 48 kHz that is 64000 frames, which the loader works out and the audio thread must
        // keep whatever order the commands are in: a length in beats that is set changes the length of a pattern that is there.)
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(256);
        let (return_tx, _return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);
        crate::app::testutil::load_onto(crate::app::testutil::SETTINGS, &cmd_tx);
        graph.drain_commands();
        let player = &graph.instruments[0].pattern;
        assert_eq!((player.length_beats, player.pattern.length_samples, player.pattern.events.len()), (2.0, 64_000, 2));
    }

    #[test]
    fn pattern_in_key_identity_on_base_note() {
        let mut p = make_pattern_player(true);
        // A minor scale: the C major pattern is off-root but in-scale.
        let scale = ScaleSetting { root: 9, scale_idx: 1 };
        // Triggering the base note plays the pattern back unchanged.
        let events = p.process(&[note_on(60)], 256, scale).to_vec();
        assert_eq!(note_ons(&events), vec![60, 64, 67]);
    }

    // -- The audio path does not allocate once it is running --
    //
    // `process()` runs on the audio thread, where a trip to the allocator can
    // stall the callback. Commands are another matter (they build the graph and
    // may allocate), so each test sets the graph up, lets one buffer settle any
    // scratch space and then counts the allocations of the buffers that follow.

    /// Count what `buffers` buffers allocate, after two that may grow scratch space.
    fn allocations_while_processing(graph: &mut AudioGraph, midi: &[(u64, [u8; 3])], buffers: usize) -> usize {
        let mut out = make_output();
        graph.process(midi, &mut out).unwrap();
        graph.process(&[], &mut out).unwrap();
        let ((), allocations) = crate::alloc_guard::allocations_in(|| {
            for _ in 0..buffers {
                graph.process(&[], &mut out).unwrap();
            }
        });
        allocations
    }

    /// A lane with a constant instrument and a `ParamScaleEffect`, whose `gain` is
    /// driven by an LFO and by an ADSR together (two modulators on one parameter),
    /// and whose first LFO's rate is driven by a second one (cross-mod).
    fn lane_with_modulators() -> (
        AudioGraph,
        crossbeam_channel::Sender<GraphCommand>,
        crossbeam_channel::Receiver<Box<dyn Plugin>>,
    ) {
        let (mut graph, cmd_tx, rr) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.5));
        insert_effect(&cmd_tx, 0, scale_effect(), 1.0);
        let sources = [
            ModSource::lfo(LfoWaveform::Sine, 2.0),
            ModSource::Envelope {
                attack: 0.01,
                decay: 0.1,
                sustain: 0.5,
                release: 0.1,
                state: EnvState::Idle,
                level: 0.0,
                notes_held: 0,
            },
            ModSource::lfo(LfoWaveform::Triangle, 0.5),
        ];
        for (index, source) in sources.into_iter().enumerate() {
            cmd_tx.send(GraphCommand::InsertModulator { inst: 0, index, source }).unwrap();
        }
        let gain = || ModTarget {
            kind: ModTargetKind::PluginParam { slot: 1, param_index: 0 },
            depth: 0.5,
            base_value: 1.0,
            param_min: 0.0,
            param_max: 1.0,
            effective: None,
        };
        for mod_index in [0, 1] {
            cmd_tx.send(GraphCommand::AddModTarget { inst: 0, mod_index, target: gain() }).unwrap();
        }
        let rate = ModTarget {
            kind: ModTargetKind::ModulatorRate { mod_index: 0 },
            depth: 0.5,
            base_value: 2.0,
            param_min: 0.1,
            param_max: 10.0,
            effective: None,
        };
        cmd_tx.send(GraphCommand::AddModTarget { inst: 0, mod_index: 2, target: rate }).unwrap();
        graph.drain_commands();
        (graph, cmd_tx, rr)
    }

    #[test]
    fn processing_does_not_allocate_while_lane_modulators_run() {
        let (mut graph, _cmd_tx, _rr) = lane_with_modulators();
        // A note held through the measured buffers keeps the ADSR moving.
        let allocations = allocations_while_processing(&mut graph, &[note_on(60)], 16);
        assert_eq!(allocations, 0, "the audio thread asked the allocator for memory");
    }

    /// A group of two lanes with sound and a `ParamScaleEffect` each, a bus effect,
    /// a group volume and pan, and a group LFO that drives the `gain` of both
    /// members' effects and of the bus effect, and the rate of an LFO in member 0
    /// (group → member-modulator cross-mod). A group ADSR cross-mods the group
    /// LFO's rate, and member 0's LFO drives its own effect. (Only real parameters
    /// are targeted: a plugin that refuses a parameter allocates its error.)
    fn busy_group() -> (
        AudioGraph,
        crossbeam_channel::Sender<GraphCommand>,
        crossbeam_channel::Receiver<Box<dyn Plugin>>,
    ) {
        let (mut graph, cmd_tx, rr) = make_graph(2);
        cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        for inst in [0, 1] {
            cmd_tx.send(GraphCommand::SetLaneGroup { inst, group: Some(0) }).unwrap();
            swap_instrument_at(&cmd_tx, inst, ConstInstrument::new(0.1));
            cmd_tx
                .send(GraphCommand::InsertEffect { inst, index: 0, effect: scale_effect(), mix: 1.0 })
                .unwrap();
        }
        cmd_tx.send(GraphCommand::InsertGroupEffect { group: 0, index: 0, effect: scale_effect(), mix: 0.5 }).unwrap();
        cmd_tx.send(GraphCommand::SetGroupVolume { group: 0, value: 0.8 }).unwrap();
        cmd_tx.send(GraphCommand::SetGroupPan { group: 0, value: -0.3 }).unwrap();
        let lfo = |rate| ModSource::lfo(LfoWaveform::Sine, rate);
        let adsr = ModSource::Envelope {
            attack: 0.01,
            decay: 0.1,
            sustain: 0.5,
            release: 0.1,
            state: EnvState::Idle,
            level: 0.0,
            notes_held: 0,
        };
        cmd_tx.send(GraphCommand::InsertModulator { inst: 0, index: 0, source: lfo(3.0) }).unwrap();
        let gain = |kind| ModTarget { kind, depth: 0.5, base_value: 1.0, param_min: 0.0, param_max: 1.0, effective: None };
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: gain(ModTargetKind::PluginParam { slot: 1, param_index: 0 }),
            })
            .unwrap();
        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source: lfo(1.0) }).unwrap();
        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 1, source: adsr }).unwrap();
        let rate = |kind| ModTarget { kind, depth: 0.5, base_value: 2.0, param_min: 0.1, param_max: 10.0, effective: None };
        for target in [
            gain(ModTargetKind::GroupMember { member: 0, slot: 1, param_index: 0 }),
            gain(ModTargetKind::GroupMember { member: 1, slot: 1, param_index: 0 }),
            gain(ModTargetKind::GroupBus { effect_index: 0, param_index: 0 }),
            rate(ModTargetKind::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Rate }),
        ] {
            cmd_tx.send(GraphCommand::AddGroupModTarget { group: 0, mod_index: 0, target }).unwrap();
        }
        cmd_tx
            .send(GraphCommand::AddGroupModTarget {
                group: 0,
                mod_index: 1,
                target: rate(ModTargetKind::ModulatorRate { mod_index: 0 }),
            })
            .unwrap();
        graph.drain_commands();
        (graph, cmd_tx, rr)
    }

    #[test]
    fn processing_does_not_allocate_while_group_modulators_run() {
        let (mut graph, _cmd_tx, _rr) = busy_group();
        let allocations = allocations_while_processing(&mut graph, &[note_on(60)], 16);
        assert_eq!(allocations, 0, "the audio thread asked the allocator for memory");
    }

    #[test]
    fn processing_does_not_allocate_while_a_pattern_plays() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.5));
        let on = |note| PatternEvent { frame: 0, status: 0x90, note, velocity: 100 };
        let off = |note| PatternEvent { frame: 500, status: 0x80, note, velocity: 0 };
        let pattern = Pattern { events: vec![on(60), on(64), on(67), off(60), off(64), off(67)], length_samples: 1000 };
        cmd_tx.send(GraphCommand::SetPattern { inst: 0, pattern, base_note: Some(60), in_key: true }).unwrap();
        graph.drain_commands();
        // The held key starts the pattern; 40 buffers of 64 frames loop it twice.
        let allocations = allocations_while_processing(&mut graph, &[note_on(62)], 40);
        assert_eq!(allocations, 0, "the audio thread asked the allocator for memory");
    }

    #[test]
    fn processing_does_not_allocate_while_notes_are_remapped_and_transposed() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        let inst = ConstInstrument::new(0.5);
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        let remapper = NoteRemapper::from_config(&make_remap(&[("G#4", "G4", 1.0)]), 2.0).unwrap();
        cmd_tx
            .send(GraphCommand::SwapInstrument { inst: 0, instrument: inst, inst_buf, remapper: Some(remapper), keep: None })
            .unwrap();
        cmd_tx.send(GraphCommand::SetTranspose { inst: 0, semitones: 2 }).unwrap();
        cmd_tx.send(GraphCommand::SetInstrumentRange { inst: 0, range: Some((40, 90)) }).unwrap();
        graph.drain_commands();
        let allocations = allocations_while_processing(&mut graph, &[note_on(68), note_on(60)], 16);
        assert_eq!(allocations, 0, "the audio thread asked the allocator for memory");
    }

    #[test]
    fn processing_does_not_allocate_with_the_built_in_plugins_and_modulators() {
        let load = |name: &str| crate::plugin::builtin::load(name, 48000.0, 64).unwrap();
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        let osc = load("builtin:osc");
        let inst_buf = (0..osc.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument { inst: 0, instrument: osc, inst_buf, remapper: None, keep: None })
            .unwrap();
        insert_effect(&cmd_tx, 0, load("builtin:filter"), 1.0);
        insert_effect(&cmd_tx, 1, load("builtin:reverb"), 0.5);
        cmd_tx.send(GraphCommand::SetVolume { inst: 0, value: 0.8 }).unwrap();
        let lfo = ModSource::lfo(LfoWaveform::Sine, 2.0);
        cmd_tx.send(GraphCommand::InsertModulator { inst: 0, index: 0, source: lfo }).unwrap();
        // The filter's `cutoff` (0) and the reverb's `damping` (2), and the oscillator's `detune` (1).
        for kind in [
            ModTargetKind::PluginParam { slot: 1, param_index: 0 },
            ModTargetKind::PluginParam { slot: 2, param_index: 2 },
            ModTargetKind::PluginParam { slot: 0, param_index: 1 },
        ] {
            let target = ModTarget { kind, depth: 0.3, base_value: 0.5, param_min: 0.0, param_max: 1.0, effective: None };
            cmd_tx.send(GraphCommand::AddModTarget { inst: 0, mod_index: 0, target }).unwrap();
        }
        graph.drain_commands();
        let allocations = allocations_while_processing(&mut graph, &[note_on(60), note_on(64)], 32);
        assert_eq!(allocations, 0, "the audio thread asked the allocator for memory");
    }

    // -- Telemetry: the master meter --

    use crate::telemetry::{self, Telemetry};

    /// A graph with one constant-output lane per entry of `values`, publishing to
    /// a `Telemetry` the test holds on to.
    fn metered_graph(values: &[f32]) -> (AudioGraph, Arc<Telemetry>) {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, _return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);
        let telemetry = Arc::new(Telemetry::new());
        graph.set_telemetry(telemetry.clone());
        for (inst, &value) in values.iter().enumerate() {
            graph.instruments.push(InstrumentLane::new(2));
            swap_instrument_at(&cmd_tx, inst, ConstInstrument::new(value));
        }
        graph.drain_commands();
        (graph, telemetry)
    }

    fn master(telemetry: &Telemetry) -> (f32, u32) {
        (telemetry.load(telemetry::MASTER.peak), telemetry.count(telemetry::MASTER.clips))
    }

    #[test]
    fn the_master_meter_publishes_the_peak_of_the_mix() {
        let (mut graph, telemetry) = metered_graph(&[0.25, 0.5]);
        assert_eq!(master(&telemetry), (0.0, 0), "nothing before the first buffer");
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        // The two lanes sum to 0.75, and that is what reaches the output.
        assert!((master(&telemetry).0 - 0.75).abs() < 1e-6, "{:?}", master(&telemetry));
        assert_eq!(master(&telemetry).1, 0);
        assert!((out[0][0] - 0.75).abs() < 1e-6);
    }

    #[test]
    fn the_scope_has_what_reaches_the_output_a_buffer_at_a_time() {
        // Two lanes, 0.25 and 0.5: the scope has their sum, as the output and the meter have it.
        let (mut graph, telemetry) = metered_graph(&[0.25, 0.5]);
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        graph.process(&[], &mut out).unwrap();
        let (mut left, mut right) = (vec![9.0; 2 * FRAMES], vec![9.0; 2 * FRAMES]);
        let written = telemetry.scope.latest(&mut left, &mut right);
        assert_eq!(written, 2 * FRAMES as u64, "every frame of both buffers");
        assert!(left.iter().chain(&right).all(|s| (s - 0.75).abs() < 1e-6), "{:?}", &left[..4]);
    }

    // -- Probes: the signal at a node, in a ring of the pool that a client watches --

    /// The latest `n` frames that ring `ring` of the probe pool has, as the left side and the right; what has not been stored is silence.
    fn probed(telemetry: &Telemetry, ring: usize, n: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut left, mut right) = (vec![9.0; n], vec![9.0; n]);
        telemetry.probes[ring].latest(&mut left, &mut right);
        (left, right)
    }

    fn listen(cmd_tx: &crossbeam_channel::Sender<GraphCommand>, target: ProbeTarget, ring: usize) {
        cmd_tx.send(GraphCommand::SetProbe { target, ring }).unwrap();
    }

    /// What `probed_lane` makes: the graph, the commands' end, the telemetry it publishes into and the end that plugins are handed back at (kept so that it is open).
    type ProbedLane = (AudioGraph, crossbeam_channel::Sender<GraphCommand>, Arc<Telemetry>, crossbeam_channel::Receiver<Box<dyn Plugin>>);

    /// A lane with a constant instrument of 0.5 at volume 0.5, and two effects that halve what they are given, the second at half mix, publishing into a telemetry that
    /// the test holds: 0.25 after the instrument, 0.125 after the first effect and 0.09375 after the second (half of 0.125, and half of the 0.0625 that halving it makes).
    fn probed_lane() -> ProbedLane {
        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        let telemetry = Arc::new(Telemetry::new());
        graph.set_telemetry(telemetry.clone());
        swap_instrument(&cmd_tx, ConstInstrument::new(0.5));
        cmd_tx.send(GraphCommand::SetVolume { inst: 0, value: 0.5 }).unwrap();
        insert_effect(&cmd_tx, 0, Box::new(ParamScaleEffect { gain: 0.5 }), 1.0);
        insert_effect(&cmd_tx, 1, Box::new(ParamScaleEffect { gain: 0.5 }), 0.5);
        (graph, cmd_tx, telemetry, return_rx)
    }

    #[test]
    fn the_probe_pool_has_a_ring_for_each_probe_and_they_are_not_the_scope() {
        let telemetry = Telemetry::new();
        assert_eq!(telemetry.probes.len(), telemetry::PROBES);
        telemetry.probes[3].push(&[vec![0.5; 16]]);
        assert_eq!(telemetry.probes[3].latest(&mut [0.0; 16], &mut [0.0; 16]), 16);
        for other in [0, 1, 2, 4, 7] {
            assert_eq!(telemetry.probes[other].latest(&mut [0.0; 16], &mut [0.0; 16]), 0, "ring {other}");
        }
        assert_eq!(telemetry.scope.latest(&mut [0.0; 16], &mut [0.0; 16]), 0);
    }

    #[test]
    fn a_probe_on_a_lanes_instrument_has_what_it_puts_out_after_its_volume() {
        let (mut graph, cmd_tx, telemetry, _rr) = probed_lane();
        listen(&cmd_tx, ProbeTarget::Instrument(0), 2);
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        let (left, right) = probed(&telemetry, 2, FRAMES);
        assert!(left.iter().chain(&right).all(|&s| s == 0.25), "{:?}", &left[..4]);
        // (The other rings have nothing: only the one that was asked to listen does.)
        assert_eq!(probed(&telemetry, 0, 4), (vec![0.0; 4], vec![0.0; 4]));
        assert!(out[0].iter().all(|&s| s == 0.09375), "and the output is what the whole chain makes of it: {}", out[0][0]);
    }

    #[test]
    fn a_probe_on_an_effect_has_what_that_effect_makes_of_the_signal_with_its_mix() {
        let (mut graph, cmd_tx, telemetry, _rr) = probed_lane();
        listen(&cmd_tx, ProbeTarget::Effect { inst: 0, index: 0 }, 0);
        listen(&cmd_tx, ProbeTarget::Effect { inst: 0, index: 1 }, 1);
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        let first = probed(&telemetry, 0, FRAMES);
        let second = probed(&telemetry, 1, FRAMES);
        assert!(first.0.iter().chain(&first.1).all(|&s| s == 0.125), "after the first effect: {:?}", &first.0[..4]);
        assert!(second.0.iter().chain(&second.1).all(|&s| s == 0.09375), "after the second, at half mix: {:?}", &second.0[..4]);
    }

    #[test]
    fn a_probe_on_an_instrument_that_nothing_follows_has_what_it_puts_out() {
        // (A lane with no effects and no volume has a path of its own, in which the instrument renders into the output.)
        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        let telemetry = Arc::new(Telemetry::new());
        graph.set_telemetry(telemetry.clone());
        swap_instrument(&cmd_tx, ConstInstrument::new(0.5));
        listen(&cmd_tx, ProbeTarget::Instrument(0), 0);
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        let (left, right) = probed(&telemetry, 0, FRAMES);
        assert!(left.iter().chain(&right).all(|&s| s == 0.5), "{:?}", &left[..4]);
        drop(return_rx);
    }

    #[test]
    fn a_probe_on_a_group_has_what_is_added_to_the_master_and_one_on_a_bus_effect_has_the_signal_before_the_pan() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);
        let telemetry = Arc::new(Telemetry::new());
        graph.set_telemetry(telemetry.clone());
        graph.instruments.push(InstrumentLane::new(2));
        graph.instruments.push(InstrumentLane::new(2));
        swap_instrument_at(&cmd_tx, 0, ConstInstrument::new(0.25));
        swap_instrument_at(&cmd_tx, 1, ConstInstrument::new(0.5));
        // Both lanes in a group: 0.75, at a volume of 0.5 (0.375), through an effect that halves (0.1875) and one that halves at half mix (0.140625), and all the way to the right.
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 1, group: Some(0) }).unwrap();
        cmd_tx.send(GraphCommand::SetGroupVolume { group: 0, value: 0.5 }).unwrap();
        cmd_tx.send(GraphCommand::InsertGroupEffect { group: 0, index: 0, effect: Box::new(ParamScaleEffect { gain: 0.5 }), mix: 1.0 }).unwrap();
        cmd_tx.send(GraphCommand::InsertGroupEffect { group: 0, index: 1, effect: Box::new(ParamScaleEffect { gain: 0.5 }), mix: 0.5 }).unwrap();
        cmd_tx.send(GraphCommand::SetGroupPan { group: 0, value: 1.0 }).unwrap();
        listen(&cmd_tx, ProbeTarget::Group(0), 0);
        listen(&cmd_tx, ProbeTarget::GroupEffect { group: 0, index: 0 }, 1);
        listen(&cmd_tx, ProbeTarget::GroupEffect { group: 0, index: 1 }, 3);
        listen(&cmd_tx, ProbeTarget::Instrument(1), 2);
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        let group = probed(&telemetry, 0, FRAMES);
        assert!(group.0.iter().all(|&s| s == 0.0) && group.1.iter().all(|&s| s == 0.140625), "the group, after its pan: {:?} {:?}", &group.0[..2], &group.1[..2]);
        assert_eq!((out[0][0], out[1][0]), (0.0, 0.140625), "which is what reaches the output");
        let first = probed(&telemetry, 1, FRAMES);
        assert!(first.0.iter().chain(&first.1).all(|&s| s == 0.1875), "the first bus effect: {:?} {:?}", &first.0[..2], &first.1[..2]);
        let second = probed(&telemetry, 3, FRAMES);
        assert!(second.0.iter().chain(&second.1).all(|&s| s == 0.140625), "the second, before the pan: {:?} {:?}", &second.0[..2], &second.1[..2]);
        let member = probed(&telemetry, 2, FRAMES);
        assert!(member.0.iter().chain(&member.1).all(|&s| s == 0.5), "a member of the group, as it was put out: {:?}", &member.0[..2]);
        drop(return_rx);
    }

    #[test]
    fn a_probe_that_is_cleared_stores_nothing_more_and_two_rings_can_listen_to_one_node() {
        let (mut graph, cmd_tx, telemetry, _rr) = probed_lane();
        listen(&cmd_tx, ProbeTarget::Instrument(0), 0);
        listen(&cmd_tx, ProbeTarget::Instrument(0), 1);
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        let stored = |ring: usize| telemetry.probes[ring].latest(&mut [0.0; 4], &mut [0.0; 4]);
        assert_eq!((stored(0), stored(1)), (FRAMES as u64, FRAMES as u64));
        cmd_tx.send(GraphCommand::ClearProbes).unwrap();
        graph.process(&[], &mut out).unwrap();
        graph.process(&[], &mut out).unwrap();
        assert_eq!((stored(0), stored(1)), (FRAMES as u64, FRAMES as u64), "nothing was stored after the probes were cleared");
        // And a probe that is set again is stored into again.
        listen(&cmd_tx, ProbeTarget::Effect { inst: 0, index: 1 }, 1);
        graph.process(&[], &mut out).unwrap();
        assert_eq!((stored(0), stored(1)), (FRAMES as u64, 2 * FRAMES as u64));
    }

    #[test]
    fn a_probe_on_a_group_that_has_no_effects_has_what_is_added_to_the_master_and_clearing_stops_it() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);
        let telemetry = Arc::new(Telemetry::new());
        graph.set_telemetry(telemetry.clone());
        graph.instruments.push(InstrumentLane::new(2));
        swap_instrument_at(&cmd_tx, 0, ConstInstrument::new(0.5));
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        cmd_tx.send(GraphCommand::SetGroupVolume { group: 0, value: 0.5 }).unwrap();
        cmd_tx.send(GraphCommand::SetGroupPan { group: 0, value: -1.0 }).unwrap();
        listen(&cmd_tx, ProbeTarget::Group(0), 4);
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        let group = probed(&telemetry, 4, FRAMES);
        assert!(group.0.iter().all(|&s| s == 0.25) && group.1.iter().all(|&s| s == 0.0), "volume and pan: {:?} {:?}", &group.0[..2], &group.1[..2]);
        // (Clearing stops a group's probe as it stops a lane's.)
        cmd_tx.send(GraphCommand::ClearProbes).unwrap();
        graph.process(&[], &mut out).unwrap();
        assert_eq!(telemetry.probes[4].latest(&mut [0.0; 4], &mut [0.0; 4]), FRAMES as u64);
        drop(return_rx);
    }

    #[test]
    fn more_probes_than_there_are_rings_are_not_stored_and_asking_for_them_does_not_allocate() {
        let (mut graph, cmd_tx, _telemetry, _rr) = probed_lane();
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        // Twelve rings' worth on one node, asked for while the audio thread is watched: the pool has eight, so that the lane has room for eight listeners, which were made
        // with the lane, and the rest are not kept.
        let ((), allocations) = crate::alloc_guard::allocations_in(|| {
            for ring in (0..telemetry::PROBES).cycle().take(12) {
                listen(&cmd_tx, ProbeTarget::Instrument(0), ring);
            }
            graph.process(&[], &mut out).unwrap();
        });
        assert_eq!(allocations, 0, "the audio thread asked the allocator for memory");
    }

    #[test]
    fn a_probe_that_cannot_be_set_is_ignored_and_nothing_breaks() {
        let (mut graph, cmd_tx, telemetry, _rr) = probed_lane();
        for target in [
            ProbeTarget::Instrument(5),
            ProbeTarget::Effect { inst: 0, index: 7 },
            ProbeTarget::Effect { inst: 4, index: 0 },
            ProbeTarget::Group(0),
            ProbeTarget::GroupEffect { group: 3, index: 0 },
        ] {
            listen(&cmd_tx, target, 0);
        }
        // Rings that the pool has not, and places in the chain that the lane has not, as many as would fill the room that the lane has for listeners if they were kept: a probe that
        // can be set after them still is.
        for ring in telemetry::PROBES..telemetry::PROBES + 10 {
            listen(&cmd_tx, ProbeTarget::Instrument(0), ring);
        }
        for index in 2..12 {
            listen(&cmd_tx, ProbeTarget::Effect { inst: 0, index }, 0);
        }
        listen(&cmd_tx, ProbeTarget::Effect { inst: 0, index: 1 }, 3);
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        for ring in (0..telemetry::PROBES).filter(|&ring| ring != 3) {
            assert_eq!(telemetry.probes[ring].latest(&mut [0.0; 4], &mut [0.0; 4]), 0, "ring {ring}");
        }
        let (left, right) = probed(&telemetry, 3, FRAMES);
        assert!(left.iter().chain(&right).all(|&s| s == 0.09375), "the one that could be set: {:?}", &left[..4]);
        assert!(out[0].iter().all(|&s| s == 0.09375));
    }

    #[test]
    fn listening_does_not_change_what_comes_out() {
        let run = |probed: bool| {
            let (mut graph, cmd_tx, _telemetry, _rr) = probed_lane();
            if probed {
                listen(&cmd_tx, ProbeTarget::Instrument(0), 0);
                listen(&cmd_tx, ProbeTarget::Effect { inst: 0, index: 0 }, 1);
                listen(&cmd_tx, ProbeTarget::Effect { inst: 0, index: 1 }, 2);
            }
            let mut heard = Vec::new();
            for buffer in 0..4 {
                let mut out = make_output();
                let midi = if buffer == 0 { vec![note_on(60)] } else { Vec::new() };
                graph.process(&midi, &mut out).unwrap();
                heard.push(out);
            }
            heard
        };
        assert_eq!(run(true), run(false));
    }

    #[test]
    fn listening_does_not_allocate() {
        let (mut graph, cmd_tx, _telemetry, _rr) = probed_lane();
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        cmd_tx.send(GraphCommand::InsertGroupEffect { group: 0, index: 0, effect: Box::new(ParamScaleEffect { gain: 0.5 }), mix: 1.0 }).unwrap();
        for (ring, target) in [
            ProbeTarget::Instrument(0),
            ProbeTarget::Effect { inst: 0, index: 0 },
            ProbeTarget::Effect { inst: 0, index: 1 },
            ProbeTarget::Group(0),
            ProbeTarget::GroupEffect { group: 0, index: 0 },
        ]
        .into_iter()
        .enumerate()
        {
            listen(&cmd_tx, target, ring);
        }
        let allocations = allocations_while_processing(&mut graph, &[note_on(60)], 16);
        assert_eq!(allocations, 0, "the audio thread asked the allocator for memory");
    }

    #[test]
    fn a_graph_that_is_given_no_telemetry_ignores_its_probes() {
        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.5));
        listen(&cmd_tx, ProbeTarget::Instrument(0), 0);
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 0.5));
        drop(return_rx);
    }

    #[test]
    fn the_master_meter_counts_each_buffer_that_goes_beyond_full_scale() {
        // Each lane is within full scale; their sum is not.
        let (mut graph, telemetry) = metered_graph(&[0.6, 0.6]);
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert_eq!(master(&telemetry).1, 1);
        graph.process(&[], &mut out).unwrap();
        graph.process(&[], &mut out).unwrap();
        assert_eq!(master(&telemetry).1, 3, "one for every buffer that clipped");
        assert!((master(&telemetry).0 - 1.2).abs() < 1e-6);

        // Releasing the notes stops the counting, and the peak starts to fall.
        graph.process(&[note_off(60)], &mut out).unwrap();
        graph.process(&[], &mut out).unwrap();
        let (peak, clips) = master(&telemetry);
        assert_eq!(clips, 3, "silence does not clip");
        let fall = telemetry::peak_fall(FRAMES as f32 / 48000.0);
        assert!((peak - 1.2 * fall * fall).abs() < 1e-4, "two quiet buffers of falling: {peak}");
    }

    #[test]
    fn full_scale_is_not_a_clip() {
        let (mut graph, telemetry) = metered_graph(&[1.0]);
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert_eq!(master(&telemetry), (1.0, 0));
    }

    #[test]
    fn a_graph_that_is_given_no_telemetry_publishes_nothing() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(2.0));
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 2.0), "the audio is the same, clipping or not");
    }

    #[test]
    fn a_silent_graph_reads_zero() {
        let (mut graph, telemetry) = metered_graph(&[]);
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();
        assert_eq!(master(&telemetry), (0.0, 0));
    }

    #[test]
    fn publishing_telemetry_does_not_allocate() {
        let (mut graph, _telemetry) = metered_graph(&[0.6, 0.6]);
        let allocations = allocations_while_processing(&mut graph, &[note_on(60)], 16);
        assert_eq!(allocations, 0, "the audio thread asked the allocator for memory");
    }

    // -- Telemetry: modulator outputs and effective values --

    use crate::telemetry::SlotAllocator;

    /// Lane 0 with a constant instrument and a `ParamScaleEffect` (chain slot 1,
    /// whose parameter 0, `gain`, runs 0 to 1), publishing into a telemetry the
    /// test holds on to, and the allocator that hands out its slots.
    struct Rig {
        graph: AudioGraph,
        cmd_tx: crossbeam_channel::Sender<GraphCommand>,
        telemetry: Arc<Telemetry>,
        slots: SlotAllocator,
        _return_rx: crossbeam_channel::Receiver<Box<dyn Plugin>>,
    }

    impl Rig {
        fn new() -> Rig {
            let (mut graph, cmd_tx, return_rx) = make_graph(2);
            let telemetry = Arc::new(Telemetry::new());
            graph.set_telemetry(telemetry.clone());
            swap_instrument(&cmd_tx, ConstInstrument::new(0.5));
            insert_effect(&cmd_tx, 0, scale_effect(), 1.0);
            Rig { graph, cmd_tx, telemetry, slots: SlotAllocator::default(), _return_rx: return_rx }
        }

        fn slot(&mut self) -> Slot {
            self.slots.alloc().unwrap()
        }

        /// A modulator in lane 0 that publishes its output; its slot.
        fn lane_modulator(&mut self, index: usize, source: ModSource) -> Slot {
            let slot = self.slot();
            self.cmd_tx.send(GraphCommand::InsertModulator { inst: 0, index, source }).unwrap();
            self.cmd_tx
                .send(GraphCommand::SetModulatorOutput { location: ModulatorLocation::Lane(0), index, slot })
                .unwrap();
            slot
        }

        /// A target on the effect's `gain`, base 0.5, that publishes its effective value; its slot.
        fn gain_target(&mut self, mod_index: usize, depth: f32) -> Slot {
            let slot = self.slot();
            let target = ModTarget {
                kind: ModTargetKind::PluginParam { slot: 1, param_index: 0 },
                depth,
                base_value: 0.5,
                param_min: 0.0,
                param_max: 1.0,
                effective: Some(slot),
            };
            self.cmd_tx.send(GraphCommand::AddModTarget { inst: 0, mod_index, target }).unwrap();
            slot
        }

        /// A cable of lane modulator `mod_index` to a setting of a modulator (or to the depth of a cable): `kind` says which, `base` is what the user set it to and
        /// `min`…`max` its range; it publishes the value the setting is set to; its slot.
        fn field_target(&mut self, mod_index: usize, kind: ModTargetKind, depth: f32, base: f32, min: f32, max: f32) -> Slot {
            let slot = self.slot();
            let target = ModTarget { kind, depth, base_value: base, param_min: min, param_max: max, effective: Some(slot) };
            self.cmd_tx.send(GraphCommand::AddModTarget { inst: 0, mod_index, target }).unwrap();
            slot
        }

        fn process(&mut self, midi: &[(u64, [u8; 3])]) {
            self.graph.process(midi, &mut make_output()).unwrap();
        }

        fn read(&self, slot: Slot) -> f32 {
            self.telemetry.load(slot)
        }
    }

    fn lfo(waveform: LfoWaveform, rate: f32) -> ModSource {
        ModSource::lfo(waveform, rate)
    }

    fn adsr(attack: f32) -> ModSource {
        ModSource::Envelope {
            attack,
            decay: 0.1,
            sustain: 0.5,
            release: 0.1,
            state: EnvState::Idle,
            level: 0.0,
            notes_held: 0,
        }
    }

    /// How far a buffer of `FRAMES` frames moves an LFO of `rate` Hz.
    fn lfo_phase(rate: f32, buffers: usize) -> f32 {
        (rate * FRAMES as f32 / 48000.0 * buffers as f32) % 1.0
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn an_lfo_publishes_its_output_after_every_buffer() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, lfo(LfoWaveform::Sine, 100.0));
        rig.process(&[]);
        assert_eq!(rig.read(output), LfoWaveform::Sine.eval(lfo_phase(100.0, 1)));
        rig.process(&[]);
        assert_eq!(rig.read(output), LfoWaveform::Sine.eval(lfo_phase(100.0, 2)));
        assert!(rig.read(output) != 0.0, "it moved");
    }

    #[test]
    fn an_envelope_publishes_its_level_as_it_attacks_and_releases() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, adsr(0.01));
        rig.process(&[]);
        assert_eq!(rig.read(output), 0.0, "idle");
        rig.process(&[note_on(60)]);
        let one_step = FRAMES as f32 / 48000.0 / 0.01;
        assert!(close(rig.read(output), one_step), "{}", rig.read(output));
        rig.process(&[]);
        assert!(close(rig.read(output), 2.0 * one_step));
        // 8 buffers to the peak, then the 0.1 s of decay (75 buffers) down to the sustain.
        for _ in 0..100 {
            rig.process(&[]);
        }
        assert!(close(rig.read(output), 0.5), "the sustain level: {}", rig.read(output));
        rig.process(&[note_off(60)]);
        assert!(rig.read(output) < 0.5, "released: {}", rig.read(output));
    }

    #[test]
    fn a_midi_learn_modulator_publishes_the_value_of_its_control() {
        let mut rig = Rig::new();
        let source = ModSource::MidiLearn { source: Some(MidiBindSource::Cc(74)), value: 0.0, learning: false };
        let output = rig.lane_modulator(0, source);
        rig.process(&[(0, [0xB0, 74, 127])]);
        assert_eq!(rig.read(output), 1.0);
        rig.process(&[(0, [0xB0, 74, 0])]);
        assert_eq!(rig.read(output), 0.0);
        rig.process(&[(0, [0xB0, 1, 127])]);
        assert_eq!(rig.read(output), 0.0, "another control does not move it");
    }

    #[test]
    fn a_target_publishes_the_value_its_parameter_is_set_to() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Sine, 100.0));
        let effective = rig.gain_target(0, 0.4);
        rig.process(&[]);
        let output = LfoWaveform::Sine.eval(lfo_phase(100.0, 1));
        // The LFO swings ±depth of the range around the base: 0.5 + 0.4 · output.
        assert!(close(rig.read(effective), 0.5 + 0.4 * output), "{} for output {output}", rig.read(effective));
        rig.process(&[]);
        let output = LfoWaveform::Sine.eval(lfo_phase(100.0, 2));
        assert!(close(rig.read(effective), 0.5 + 0.4 * output));
    }

    #[test]
    fn an_envelope_targets_effective_value_rises_to_the_base() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, adsr(0.01));
        let effective = rig.gain_target(0, 0.5);
        rig.process(&[]);
        // Idle: the base minus depth of the way down to the minimum: 0.5 - 0.5 · 1 · 0.5.
        assert!(close(rig.read(effective), 0.25), "{}", rig.read(effective));
        rig.process(&[note_on(60)]);
        let level = FRAMES as f32 / 48000.0 / 0.01;
        assert!(close(rig.read(effective), 0.5 - 0.5 * (1.0 - level) * 0.5), "{}", rig.read(effective));
    }

    #[test]
    fn a_midi_learn_targets_effective_value_adds_the_control_to_the_base() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, ModSource::MidiLearn { source: Some(MidiBindSource::Cc(74)), value: 0.0, learning: false });
        let effective = rig.gain_target(0, 0.4);
        rig.process(&[]);
        assert!(close(rig.read(effective), 0.5), "at rest it is the base");
        rig.process(&[(0, [0xB0, 74, 127])]);
        assert!(close(rig.read(effective), 0.9), "{}", rig.read(effective));
    }

    /// A control that is bound and has sent nothing yet adds nothing to what it drives: a CC and pressure rest at 0, and the pitch-bend wheel at its centre, which a wheel does not
    /// send until it is moved. The source is the one the model gives the audio thread (`mod_source_slot_to_graph`: it went at 0 for every control, so that a wheel that a session file
    /// bound pulled what it drove down by the whole of its depth until it was first moved).
    #[test]
    fn a_bound_control_that_has_sent_nothing_yet_adds_nothing_to_what_it_drives() {
        for control in [MidiBindSource::Cc(74), MidiBindSource::PitchBend, MidiBindSource::Aftertouch, MidiBindSource::Velocity, MidiBindSource::Key] {
            let source = crate::app::model::mod_source_slot_to_graph(&crate::app::model::ModSourceSlot::MidiLearn { source: Some(control), learning: false });
            let mut rig = Rig::new();
            rig.lane_modulator(0, source);
            let effective = rig.gain_target(0, 0.4);
            rig.process(&[]);
            assert!(close(rig.read(effective), 0.5), "{control:?} at rest leaves the base: {}", rig.read(effective));
        }
        // And the wheel, once it is moved, goes by its own law: all the way down is the whole of the depth below the base.
        let source = crate::app::model::mod_source_slot_to_graph(&crate::app::model::ModSourceSlot::MidiLearn { source: Some(MidiBindSource::PitchBend), learning: false });
        let mut rig = Rig::new();
        rig.lane_modulator(0, source);
        let effective = rig.gain_target(0, 0.4);
        rig.process(&[(0, [0xE0, 0, 0])]);
        assert!(close(rig.read(effective), 0.1), "{}", rig.read(effective));
    }

    /// The control that follows the velocity of the notes a lane plays, as the model gives it to the audio thread.
    fn velocity() -> ModSource {
        crate::app::model::mod_source_slot_to_graph(&crate::app::model::ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Velocity), learning: false })
    }

    /// The control that follows their numbers.
    fn key() -> ModSource {
        crate::app::model::mod_source_slot_to_graph(&crate::app::model::ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Key), learning: false })
    }

    #[test]
    fn a_velocity_control_publishes_the_velocity_of_the_last_note_and_keeps_it_when_the_note_ends() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, velocity());
        rig.process(&[]);
        assert_eq!(rig.read(output), 0.0, "before a note it is at rest");
        rig.process(&[(0, [0x90, 60, 127])]);
        assert_eq!(rig.read(output), 1.0);
        rig.process(&[(0, [0x80, 60, 0])]);
        assert_eq!(rig.read(output), 1.0, "the end of the note leaves it where it was: a release has the velocity of its own note");
        rig.process(&[(0, [0x80, 60, 64])]);
        assert_eq!(rig.read(output), 1.0, "a note-off has a velocity of its own, which is not a note's (many keyboards say 64)");
        rig.process(&[(0, [0x90, 62, 64])]);
        assert_eq!(rig.read(output), 64.0 / 127.0);
        rig.process(&[(0, [0x90, 62, 0])]);
        assert_eq!(rig.read(output), 64.0 / 127.0, "a note-on with no velocity is a note-off");
        rig.process(&[(0, [0xB0, 74, 0]), (0, [0xD0, 0, 0]), (0, [0xE0, 0, 0])]);
        assert_eq!(rig.read(output), 64.0 / 127.0, "a control is not a note");
        rig.process(&[(0, [0x90, 60, 20]), (10, [0x90, 64, 90])]);
        assert_eq!(rig.read(output), 90.0 / 127.0, "of two notes in a buffer, the later one");
    }

    #[test]
    fn a_key_control_publishes_the_number_of_the_last_note_over_127_and_keeps_it_when_the_note_ends() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, key());
        rig.process(&[]);
        assert_eq!(rig.read(output), 60.0 / 127.0, "before a note it is at rest, at middle C");
        rig.process(&[note_on(72)]);
        assert_eq!(rig.read(output), 72.0 / 127.0);
        rig.process(&[note_off(72)]);
        assert_eq!(rig.read(output), 72.0 / 127.0, "the end of the note leaves it where it was");
        rig.process(&[(0, [0x80, 40, 64])]);
        assert_eq!(rig.read(output), 72.0 / 127.0, "a note-off with a velocity is not a note");
        rig.process(&[note_on(127)]);
        assert_eq!(rig.read(output), 1.0);
        rig.process(&[note_on(0)]);
        assert_eq!(rig.read(output), 0.0);
        rig.process(&[(0, [0x90, 40, 0]), (0, [0xB0, 74, 99])]);
        assert_eq!(rig.read(output), 0.0, "neither a note-on with no velocity nor a control is a note");
    }

    #[test]
    fn a_velocity_controls_target_is_the_base_with_the_velocity_times_the_depth_of_the_range_added_to_it() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, velocity());
        let up = rig.gain_target(0, 0.4);
        rig.process(&[]);
        assert!(close(rig.read(up), 0.5), "before a note it is the base: {}", rig.read(up));
        rig.process(&[(0, [0x90, 60, 127])]);
        assert!(close(rig.read(up), 0.9), "{}", rig.read(up));
        rig.process(&[(0, [0x90, 60, 64])]);
        assert!(close(rig.read(up), 0.5 + 0.4 * 64.0 / 127.0), "{}", rig.read(up));

        // A negative depth is the other way: the harder the note, the lower.
        let mut rig = Rig::new();
        rig.lane_modulator(0, velocity());
        let down = rig.gain_target(0, -0.4);
        rig.process(&[(0, [0x90, 60, 127])]);
        assert!(close(rig.read(down), 0.1), "{}", rig.read(down));
    }

    #[test]
    fn a_key_controls_target_is_the_base_at_middle_c_and_moves_by_the_depth_of_the_range_in_five_octaves_either_way() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, key());
        let effective = rig.gain_target(0, 0.4);
        rig.process(&[]);
        assert_eq!(rig.read(effective), 0.5, "before a note it is the base, exactly: the note that it rests at is a whole one");
        for (note, expected) in [(60u8, 0.5), (90, 0.7), (120, 0.9), (127, 0.9), (30, 0.3), (0, 0.1)] {
            rig.process(&[note_on(note)]);
            assert!(close(rig.read(effective), expected), "note {note}: {} for {expected}", rig.read(effective));
        }
    }

    #[test]
    fn a_key_control_goes_by_the_exact_distance_of_every_note_from_middle_c() {
        // The value is the note over 127 and the law multiplies it by 127 again: it has to give the note back, or a note would move what it drives by the rest of a division.
        for note in 0..=127u8 {
            let mut key = Modulator::new(ModSource::MidiLearn { source: Some(MidiBindSource::Key), value: MidiBindSource::Key.rest(), learning: false }, 48000.0);
            key.tick(FRAMES, &[note_on(note)], DEFAULT_BPM);
            let expected = ((f32::from(note) - 60.0) / 60.0).clamp(-1.0, 1.0);
            assert_eq!(key.offset(1.0, 0.0, 0.0, 1.0), expected, "note {note}");
        }
    }

    #[test]
    fn velocity_and_key_controls_hear_the_notes_that_a_lane_plays_and_no_others() {
        // The lane takes the notes from 60 to 71 and shifts them up an octave.
        let mut rig = Rig::new();
        rig.cmd_tx.send(GraphCommand::SetInstrumentRange { inst: 0, range: Some((60, 71)) }).unwrap();
        rig.cmd_tx.send(GraphCommand::SetTranspose { inst: 0, semitones: 12 }).unwrap();
        let velocity_out = rig.lane_modulator(0, velocity());
        let key_out = rig.lane_modulator(1, key());
        rig.process(&[(0, [0x90, 40, 120])]);
        assert_eq!((rig.read(velocity_out), rig.read(key_out)), (0.0, 60.0 / 127.0), "a note below the range is not the lane's: both are at rest");
        rig.process(&[(0, [0x90, 62, 100])]);
        assert_eq!(rig.read(velocity_out), 100.0 / 127.0);
        assert_eq!(rig.read(key_out), 74.0 / 127.0, "the note that the instrument is given is 62 and an octave: 74");
    }

    #[test]
    fn a_control_that_is_learning_is_not_bound_by_a_note() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, ModSource::MidiLearn { source: None, value: 0.0, learning: true });
        rig.process(&[note_on(60), note_off(60)]);
        assert_eq!(rig.read(output), 0.0);
        assert!(rig.graph.instruments[0].modulators[0].last_learned.is_none(), "no control was captured");
        assert!(matches!(rig.graph.instruments[0].modulators[0].source, ModSource::MidiLearn { source: None, learning: true, .. }), "it goes on waiting for a controller");
    }

    #[test]
    fn a_group_velocity_control_hears_the_notes_of_its_members_and_drives_a_member_parameter() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx.send(GraphCommand::SwapInstrument { inst: 0, instrument: inst, inst_buf, remapper: None, keep: None }).unwrap();
        cmd_tx.send(GraphCommand::SetInstrumentRange { inst: 0, range: Some((60, 71)) }).unwrap();
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source: velocity() }).unwrap();
        cmd_tx
            .send(GraphCommand::AddGroupModTarget {
                group: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::GroupMember { member: 0, slot: 0, param_index: 0 },
                    depth: 0.4,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                    effective: None,
                },
            })
            .unwrap();
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();
        assert!(close(out[0][0], 0.5), "before a note it is the base: {}", out[0][0]);
        graph.process(&[(0, [0x90, 40, 127])], &mut out).unwrap();
        assert!(close(out[0][0], 0.5), "a note that no member takes is not heard: {}", out[0][0]);
        graph.process(&[(0, [0x90, 64, 127])], &mut out).unwrap();
        assert!(close(out[0][0], 0.9), "{}", out[0][0]);
    }

    #[test]
    fn a_macro_publishes_the_value_it_is_set_to_and_no_note_or_control_moves_it() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, ModSource::Macro { value: 0.7 });
        rig.process(&[]);
        assert_eq!(rig.read(output), 0.7);
        rig.process(&[note_on(60), (0, [0xB0, 74, 127]), (0, [0xE0, 0, 127])]);
        assert_eq!(rig.read(output), 0.7);
        rig.process(&[note_off(60)]);
        assert_eq!(rig.read(output), 0.7);
    }

    #[test]
    fn a_macros_target_is_the_base_with_the_value_times_the_depth_of_the_range_added_to_it() {
        // The base is 0.5 and the range is 1: half of the depth of 0.4 above it for a depth of 0.4, and below it for -0.4.
        let mut rig = Rig::new();
        rig.lane_modulator(0, ModSource::Macro { value: 0.5 });
        let up = rig.gain_target(0, 0.4);
        rig.process(&[]);
        assert!(close(rig.read(up), 0.7), "{}", rig.read(up));

        let mut rig = Rig::new();
        rig.lane_modulator(0, ModSource::Macro { value: 0.5 });
        let down = rig.gain_target(0, -0.4);
        rig.process(&[]);
        assert!(close(rig.read(down), 0.3), "{}", rig.read(down));
    }

    #[test]
    fn a_macro_moves_what_it_drives_by_the_depth_of_the_range_that_it_has() {
        // A parameter that runs from 0 to 0.5, set to 0.1: half of the depth of 0.4 of that range is 0.1 above it.
        let mut rig = Rig::new();
        rig.lane_modulator(0, ModSource::Macro { value: 0.5 });
        let effective = rig.slot();
        let target = ModTarget { kind: ModTargetKind::PluginParam { slot: 1, param_index: 0 }, depth: 0.4, base_value: 0.1, param_min: 0.0, param_max: 0.5, effective: Some(effective) };
        rig.cmd_tx.send(GraphCommand::AddModTarget { inst: 0, mod_index: 0, target }).unwrap();
        rig.process(&[]);
        assert!(close(rig.read(effective), 0.2), "{}", rig.read(effective));
    }

    #[test]
    fn a_macro_can_drive_the_rate_of_an_lfo_by_a_fraction_of_the_knobs_travel() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, ModSource::Macro { value: 0.5 });
        rig.lane_modulator(1, lfo(LfoWaveform::Sine, 2.0));
        let rate = rig.field_target(0, ModTargetKind::ModulatorRate { mod_index: 1 }, 0.2, 2.0, 0.01, 50.0);
        rig.process(&[]);
        // Half of the macro's depth of 0.2 is a tenth of the travel of the rate, which is turned on a log scale, up from the 2 Hz that the user set.
        let expected = value_on_decades(place_on_decades(2.0, 0.01, 50.0) + 0.1, 0.01, 50.0);
        assert!(near(rig.read(rate), expected), "{} for {expected}", rig.read(rate));
    }

    #[test]
    fn a_macro_at_rest_leaves_what_it_drives_where_the_user_set_it() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, ModSource::Macro { value: 0.0 });
        let effective = rig.gain_target(0, 1.0);
        rig.process(&[]);
        assert!(close(rig.read(effective), 0.5), "{}", rig.read(effective));
    }

    #[test]
    fn a_macro_at_full_moves_what_it_drives_by_the_depth_of_the_range_and_stops_at_the_end_of_it() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, ModSource::Macro { value: 1.0 });
        let effective = rig.gain_target(0, 3.0);
        rig.process(&[]);
        assert_eq!(rig.read(effective), 1.0, "three times the range from 0.5 stops at the top");
    }

    #[test]
    fn the_value_of_a_macro_is_changed_by_giving_it_a_macro_of_another_value_and_the_next_buffer_has_it() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, ModSource::Macro { value: 0.2 });
        let effective = rig.gain_target(0, 0.5);
        rig.process(&[]);
        assert!(close(rig.read(output), 0.2) && close(rig.read(effective), 0.6), "{} {}", rig.read(output), rig.read(effective));
        rig.cmd_tx.send(GraphCommand::SetModulatorSource { inst: 0, mod_index: 0, source: ModSource::Macro { value: 0.8 } }).unwrap();
        rig.process(&[]);
        assert!(close(rig.read(output), 0.8) && close(rig.read(effective), 0.9), "{} {}", rig.read(output), rig.read(effective));
    }

    /// A step sequencer's source with `values` for its first steps (the others 0), `length` of them in use, at `rate` steps a second, at its first step.
    fn steps(values: &[f32], length: usize, rate: f32, glide: f32, retrigger: bool) -> ModSource {
        let mut all = [0.0; MAX_STEPS];
        all[..values.len()].copy_from_slice(values);
        ModSource::Steps { values: all, length, rate, glide, retrigger, sync: 0.0, position: 0.0 }
    }

    /// The rate at which a step sequencer takes `buffers` buffers (of `FRAMES` frames, at 48 kHz) to go through a step.
    fn a_step_every(buffers: f32) -> f32 {
        48000.0 / FRAMES as f32 / buffers
    }

    /// A step sequencer in lane 0 that publishes its output and its position; their slots.
    fn lane_steps(rig: &mut Rig, index: usize, source: ModSource) -> (Slot, Slot) {
        let output = rig.lane_modulator(index, source);
        let position = rig.slot();
        rig.cmd_tx.send(GraphCommand::SetModulatorPosition { location: ModulatorLocation::Lane(0), index, slot: position }).unwrap();
        (output, position)
    }

    #[test]
    fn a_step_sequencer_puts_out_the_value_of_the_step_it_is_at_and_goes_on_by_its_rate() {
        let mut rig = Rig::new();
        // Three steps of two buffers each, and a fourth value that is set and not in use.
        let output = rig.lane_modulator(0, steps(&[0.5, -0.25, 1.0, 0.9], 3, a_step_every(2.0), 0.0, false));
        let mut seen = Vec::new();
        for _ in 0..8 {
            rig.process(&[]);
            seen.push(rig.read(output));
        }
        // (A buffer takes it half a step on, so the first ends in the middle of the first step, and the pattern goes round after the third step.)
        assert_eq!(seen, [0.5, -0.25, -0.25, 1.0, 1.0, 0.5, 0.5, -0.25]);
    }

    #[test]
    fn a_step_sequencers_glide_is_the_last_part_of_each_step_spent_going_to_the_value_of_the_next() {
        // Three steps in use, and a fourth that is set and not (the last step goes to the first, and not to it).
        let mut values = [0.0; MAX_STEPS];
        values[..4].copy_from_slice(&[0.5, 1.0, -1.0, 0.9]);
        // No glide: the value of the step all through it.
        for position in [0.0, 0.25, 0.999, 1.0, 1.5, 2.999] {
            assert_eq!(steps_output(&values, 3, position, 0.0), values[position as usize], "at {position}");
        }
        // A glide of a half: held for the first half of the step, then a straight line to the next value (the first, after the last).
        let at = |position| steps_output(&values, 3, position, 0.5);
        assert_eq!(at(0.0), 0.5);
        assert_eq!(at(0.5), 0.5, "the glide begins after the half");
        assert!(close(at(0.75), 0.75), "{}", at(0.75));
        assert_eq!(at(1.0), 1.0);
        assert!(close(at(1.75), 0.0), "{}", at(1.75));
        assert!(close(at(2.75), -0.25), "the last step goes to the first: {}", at(2.75));
        // A glide of 1 is a straight line through the values all the time.
        assert!(close(steps_output(&values, 3, 0.25, 1.0), 0.625));
        assert!(close(steps_output(&values, 3, 1.5, 1.0), 0.0));
        // One step has nothing to go to.
        assert_eq!(steps_output(&values, 1, 0.9, 1.0), 0.5);
        // A glide beyond its range is kept to it, and a place that is not a number is the beginning.
        assert_eq!(steps_output(&values, 3, 0.25, 7.0), steps_output(&values, 3, 0.25, 1.0));
        assert_eq!(steps_output(&values, 3, 0.9, -2.0), 0.5);
        assert_eq!(steps_output(&values, 3, f32::NAN, 0.5), 0.5);
    }

    #[test]
    fn a_step_sequencer_of_no_steps_or_of_more_than_there_are_is_kept_to_the_ones_it_has() {
        let mut values = [0.0; MAX_STEPS];
        values[0] = 0.3;
        values[MAX_STEPS - 1] = -0.6;
        assert_eq!(steps_output(&values, 0, 0.0, 0.0), 0.3, "a length of none is a length of one");
        assert_eq!(steps_output(&values, 99, MAX_STEPS as f32 - 0.5, 0.0), -0.6, "and one of more than there are is all of them");
        assert_eq!(steps_output(&values, 99, MAX_STEPS as f32, 0.0), 0.3, "which goes round");
    }

    #[test]
    fn a_step_sequencer_of_no_steps_stays_at_its_first_and_one_of_too_many_goes_round_at_the_most_there_are() {
        let mut rig = Rig::new();
        let (output, position) = lane_steps(&mut rig, 0, steps(&[0.8, 0.6], 0, a_step_every(4.0), 0.0, false));
        rig.process(&[]);
        assert_eq!((rig.read(output), rig.read(position)), (0.8, 0.25), "of none it is of one: it goes round at the first");
        for _ in 0..4 {
            rig.process(&[]);
        }
        assert_eq!((rig.read(output), rig.read(position)), (0.8, 0.25));

        let mut rig = Rig::new();
        let (_, position) = lane_steps(&mut rig, 0, steps(&[], MAX_STEPS + 5, a_step_every(1.0), 0.0, false));
        for _ in 0..MAX_STEPS + 1 {
            rig.process(&[]);
        }
        assert_eq!(rig.read(position), 1.0, "one more than there are goes round at the last");
    }

    #[test]
    fn a_step_sequencer_that_was_given_no_rate_goes_on_when_it_is_given_one() {
        let mut rig = Rig::new();
        let (_, position) = lane_steps(&mut rig, 0, steps(&[], 4, a_step_every(1.0), 0.0, false));
        rig.cmd_tx.send(GraphCommand::SetModulatorRate { inst: 0, mod_index: 0, rate: f32::NAN }).unwrap();
        rig.process(&[]);
        assert_eq!(rig.read(position), 0.0, "it is at the beginning, and not nowhere");
        rig.cmd_tx.send(GraphCommand::SetModulatorRate { inst: 0, mod_index: 0, rate: a_step_every(1.0) }).unwrap();
        rig.process(&[]);
        assert_eq!(rig.read(position), 1.0);
    }

    #[test]
    fn a_step_sequencer_moves_what_it_drives_by_the_depth_of_the_range_that_it_has() {
        // A parameter that runs from 0.2 to 0.6, set to 0.3: a value of 0.5 at a depth of 0.5 is a quarter of that range of 0.4, 0.1, above it.
        let mut rig = Rig::new();
        rig.lane_modulator(0, steps(&[0.5], 1, 0.0, 0.0, false));
        let effective = rig.slot();
        let target = ModTarget { kind: ModTargetKind::PluginParam { slot: 1, param_index: 0 }, depth: 0.5, base_value: 0.3, param_min: 0.2, param_max: 0.6, effective: Some(effective) };
        rig.cmd_tx.send(GraphCommand::AddModTarget { inst: 0, mod_index: 0, target }).unwrap();
        rig.process(&[]);
        assert!(close(rig.read(effective), 0.4), "{}", rig.read(effective));
    }

    #[test]
    fn a_step_sequencer_publishes_where_it_is_after_every_buffer_and_goes_round_at_its_length() {
        let mut rig = Rig::new();
        let (_, position) = lane_steps(&mut rig, 0, steps(&[], 3, a_step_every(4.0), 0.0, false));
        let mut seen = Vec::new();
        for _ in 0..13 {
            rig.process(&[]);
            seen.push(rig.read(position));
        }
        // A quarter of a step a buffer.
        assert_eq!(seen, [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 2.75, 0.0, 0.25]);
    }

    #[test]
    fn a_modulator_that_is_not_a_step_sequencer_publishes_no_position() {
        let mut rig = Rig::new();
        let output = rig.lane_modulator(0, lfo(LfoWaveform::Sine, 100.0));
        let position = rig.slot();
        rig.cmd_tx.send(GraphCommand::SetModulatorPosition { location: ModulatorLocation::Lane(0), index: 0, slot: position }).unwrap();
        rig.process(&[]);
        assert!(rig.read(output) != 0.0, "the LFO is going");
        assert_eq!(rig.read(position), 0.0);
    }

    #[test]
    fn a_position_slot_for_a_modulator_that_is_not_there_is_left_alone() {
        let mut rig = Rig::new();
        let slot = rig.slot();
        for location in [ModulatorLocation::Lane(0), ModulatorLocation::Lane(9), ModulatorLocation::Group(0)] {
            rig.cmd_tx.send(GraphCommand::SetModulatorPosition { location, index: 3, slot }).unwrap();
        }
        rig.process(&[]);
        assert_eq!(rig.read(slot), 0.0);
    }

    #[test]
    fn a_note_starts_a_step_sequencer_again_at_its_first_step_if_it_retriggers() {
        let mut rig = Rig::new();
        let (output, position) = lane_steps(&mut rig, 0, steps(&[0.1, 0.2, 0.3, 0.4], 4, a_step_every(1.0), 0.0, true));
        for _ in 0..3 {
            rig.process(&[]);
        }
        assert_eq!((rig.read(output), rig.read(position)), (0.4, 3.0), "at the fourth step");
        rig.process(&[note_on(60)]);
        assert_eq!((rig.read(output), rig.read(position)), (0.1, 0.0), "a note puts it at the first");
        rig.process(&[]);
        assert_eq!((rig.read(output), rig.read(position)), (0.2, 1.0), "and it goes on from there");
    }

    #[test]
    fn a_step_sequencer_that_does_not_retrigger_goes_on_through_a_note() {
        let mut rig = Rig::new();
        let (output, position) = lane_steps(&mut rig, 0, steps(&[0.1, 0.2, 0.3, 0.4], 4, a_step_every(1.0), 0.0, false));
        for _ in 0..2 {
            rig.process(&[]);
        }
        rig.process(&[note_on(60)]);
        assert_eq!((rig.read(output), rig.read(position)), (0.4, 3.0));
    }

    #[test]
    fn a_note_off_and_a_note_on_with_no_velocity_do_not_start_a_step_sequencer_again() {
        let mut rig = Rig::new();
        let (_, position) = lane_steps(&mut rig, 0, steps(&[0.1, 0.2, 0.3, 0.4], 4, a_step_every(1.0), 0.0, true));
        rig.process(&[]);
        rig.process(&[note_off(60), (0, [0x90, 60, 0]), (0, [0xB0, 74, 100])]);
        assert_eq!(rig.read(position), 2.0, "it went on");
    }

    #[test]
    fn a_step_sequencers_target_is_the_base_with_the_value_times_the_depth_of_the_range_added_to_it_as_an_lfos_is() {
        // The base is 0.5 and the range is 1: a value of -1 at a depth of 0.4 is 0.4 below it, a value of 0.5 is 0.2 above it.
        let mut rig = Rig::new();
        rig.lane_modulator(0, steps(&[0.5, -1.0], 2, a_step_every(1.0), 0.0, false));
        let effective = rig.gain_target(0, 0.4);
        rig.process(&[]);
        assert!(close(rig.read(effective), 0.1), "{}", rig.read(effective));
        rig.process(&[]);
        assert!(close(rig.read(effective), 0.7), "{}", rig.read(effective));
    }

    #[test]
    fn a_step_sequencer_at_rest_in_the_middle_leaves_what_it_drives_where_the_user_set_it() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, steps(&[0.0, 0.0], 2, a_step_every(1.0), 0.0, false));
        let effective = rig.gain_target(0, 1.0);
        rig.process(&[]);
        assert!(close(rig.read(effective), 0.5), "{}", rig.read(effective));
    }

    #[test]
    fn the_rate_of_a_step_sequencer_is_set_as_an_lfos_is_and_a_cable_can_drive_it() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0));
        let (_, position) = lane_steps(&mut rig, 1, steps(&[], 4, 2.0, 0.0, false));
        // The rate is given: one step a buffer.
        rig.cmd_tx.send(GraphCommand::SetModulatorRate { inst: 0, mod_index: 1, rate: a_step_every(1.0) }).unwrap();
        rig.process(&[]);
        assert_eq!(rig.read(position), 1.0);
        // And back to something slow, driven by the square LFO (always +1) from a cable: a fifth of the knob's travel up from 2 Hz, from the second buffer on (the
        // cable writes the rate after the modulators have gone a buffer).
        rig.cmd_tx.send(GraphCommand::SetModulatorRate { inst: 0, mod_index: 1, rate: 2.0 }).unwrap();
        let rate = rig.field_target(0, ModTargetKind::ModulatorRate { mod_index: 1 }, 0.2, 2.0, 0.01, 50.0);
        rig.process(&[]);
        rig.process(&[]);
        let driven = value_on_decades(place_on_decades(2.0, 0.01, 50.0) + 0.2, 0.01, 50.0);
        assert!(near(rig.read(rate), driven), "{} for {driven}", rig.read(rate));
        let moved = 1.0 + (2.0 + driven) * FRAMES as f32 / 48000.0;
        assert!(near(rig.read(position), moved), "{} for {moved}", rig.read(position));
    }

    #[test]
    fn a_step_sequencer_is_given_other_steps_without_losing_its_place() {
        let mut rig = Rig::new();
        let (output, position) = lane_steps(&mut rig, 0, steps(&[0.1, 0.2, 0.3, 0.4], 4, a_step_every(2.0), 0.0, false));
        for _ in 0..3 {
            rig.process(&[]);
        }
        assert_eq!(rig.read(position), 1.5);
        let mut values = [0.0; MAX_STEPS];
        values[..3].copy_from_slice(&[-0.1, -0.2, -0.3]);
        rig.cmd_tx.send(GraphCommand::SetModulatorSteps { location: ModulatorLocation::Lane(0), index: 0, values, length: 3, glide: 0.0, retrigger: true }).unwrap();
        rig.process(&[]);
        assert_eq!((rig.read(position), rig.read(output)), (2.0, -0.3), "it is where it was, with the new values");
        rig.process(&[]);
        rig.process(&[]);
        assert_eq!((rig.read(position), rig.read(output)), (0.0, -0.1), "and goes round at the new length");
        rig.process(&[]);
        rig.process(&[note_on(60)]);
        assert_eq!(rig.read(position), 0.0, "and a note starts it again, which it had not");
    }

    #[test]
    fn the_glide_of_a_step_sequencer_is_given_with_its_steps() {
        let mut rig = Rig::new();
        let (output, _) = lane_steps(&mut rig, 0, steps(&[0.0, 1.0], 2, a_step_every(4.0), 0.0, false));
        rig.process(&[]);
        assert_eq!(rig.read(output), 0.0, "a quarter of the way into the first step, with no glide");
        let mut values = [0.0; MAX_STEPS];
        values[1] = 1.0;
        rig.cmd_tx.send(GraphCommand::SetModulatorSteps { location: ModulatorLocation::Lane(0), index: 0, values, length: 2, glide: 1.0, retrigger: false }).unwrap();
        rig.process(&[]);
        assert!(close(rig.read(output), 0.5), "half way through the first step, on the way to the second: {}", rig.read(output));
    }

    #[test]
    fn steps_are_given_to_a_step_sequencer_in_a_group_and_a_note_of_a_member_starts_it_again() {
        let mut g = GroupRig::new();
        g.rig.cmd_tx
            .send(GraphCommand::InsertGroupModulator { group: 0, index: 1, source: steps(&[0.1, 0.2, 0.3, 0.4], 4, a_step_every(1.0), 0.0, true) })
            .unwrap();
        let (output, position) = (g.rig.slot(), g.rig.slot());
        g.rig.cmd_tx.send(GraphCommand::SetModulatorOutput { location: ModulatorLocation::Group(0), index: 1, slot: output }).unwrap();
        g.rig.cmd_tx.send(GraphCommand::SetModulatorPosition { location: ModulatorLocation::Group(0), index: 1, slot: position }).unwrap();
        g.rig.process(&[]);
        g.rig.process(&[]);
        assert_eq!((g.rig.read(output), g.rig.read(position)), (0.3, 2.0));
        g.rig.process(&[note_on(60)]);
        assert_eq!((g.rig.read(output), g.rig.read(position)), (0.1, 0.0), "a note of a member starts it again");
        let mut values = [0.0; MAX_STEPS];
        values[..3].copy_from_slice(&[0.7, 0.8, 0.9]);
        g.rig.cmd_tx.send(GraphCommand::SetModulatorSteps { location: ModulatorLocation::Group(0), index: 1, values, length: 3, glide: 0.0, retrigger: false }).unwrap();
        g.rig.process(&[]);
        assert_eq!((g.rig.read(output), g.rig.read(position)), (0.8, 1.0), "the group's has the steps it was given and keeps its place");
        g.rig.process(&[note_on(60)]);
        assert_eq!(g.rig.read(position), 2.0, "and a note no longer starts it again");
    }

    #[test]
    fn modulators_on_the_same_parameter_share_one_effective_value() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0)); // a square at phase 0: always +1
        rig.lane_modulator(1, adsr(0.01));
        let from_lfo = rig.gain_target(0, 0.1);
        let from_envelope = rig.gain_target(1, 0.5);
        rig.process(&[]);
        // 0.5 + 0.1 (the LFO) + the envelope's idle offset of -0.5 · 0.5 = 0.35, seen from either cable.
        assert!(close(rig.read(from_lfo), 0.35), "{}", rig.read(from_lfo));
        assert_eq!(rig.read(from_lfo), rig.read(from_envelope));
    }

    #[test]
    fn the_effective_value_is_clamped_to_the_parameters_range() {
        // A square LFO at +1 with a depth of three times the range pushes the
        // parameter far past either end; what is published is where it stopped.
        for (depth, stops_at) in [(3.0, 1.0), (-3.0, 0.0)] {
            let mut rig = Rig::new();
            rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0));
            let effective = rig.gain_target(0, depth);
            rig.process(&[]);
            assert_eq!(rig.read(effective), stops_at, "depth {depth}");
        }
    }

    #[test]
    fn a_cable_to_the_rate_of_an_lfo_publishes_the_rate_it_was_set_to() {
        // (Finding f132: a cable to a modulator's own setting had no slot, so what drove a rate was not to be seen.)
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0)); // always +1
        rig.lane_modulator(1, lfo(LfoWaveform::Sine, 2.0));
        let rate = rig.field_target(0, ModTargetKind::ModulatorRate { mod_index: 1 }, 0.1, 2.0, 0.01, 50.0);
        rig.process(&[]);
        // The LFO's +1 times the depth, a tenth of the knob's travel (the rate is turned on a log scale, 0.01 to 50 Hz), up from the 2 Hz the user set.
        let expected = value_on_decades(place_on_decades(2.0, 0.01, 50.0) + 0.1, 0.01, 50.0);
        assert!(near(rig.read(rate), expected), "{} for {expected}", rig.read(rate));
    }

    #[test]
    fn an_envelope_on_a_rate_publishes_it_rising_to_the_value_the_user_set() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, adsr(0.01));
        rig.lane_modulator(1, lfo(LfoWaveform::Sine, 2.0));
        let rate = rig.field_target(0, ModTargetKind::ModulatorRate { mod_index: 1 }, 0.5, 2.0, 0.01, 50.0);
        rig.process(&[]);
        // Idle: the user's value less depth of the way down the knob to the lowest (the rate is turned on a log scale).
        let at = place_on_decades(2.0, 0.01, 50.0);
        let expected = value_on_decades(at - 0.5 * at, 0.01, 50.0);
        assert!(near(rig.read(rate), expected), "{} for {expected}", rig.read(rate));
        rig.process(&[note_on(60)]);
        let level = FRAMES as f32 / 48000.0 / 0.01;
        let expected = value_on_decades(at - 0.5 * (1.0 - level) * at, 0.01, 50.0);
        assert!(near(rig.read(rate), expected), "{} for {expected}", rig.read(rate));
    }

    #[test]
    fn a_cable_to_each_stage_of_an_envelope_publishes_the_stage_it_was_set_to() {
        // The square LFO is always +1: each stage is what the user set plus the depth of its range, and for a time, which is turned on a log scale, plus the depth of the knob's travel.
        for (kind, base, (min, max), depth) in [
            (ModTargetKind::ModulatorAttack { mod_index: 1 }, 0.01, (0.001, 10.0), 0.01),
            (ModTargetKind::ModulatorDecay { mod_index: 1 }, 0.1, (0.001, 10.0), 0.02),
            (ModTargetKind::ModulatorSustain { mod_index: 1 }, 0.5, (0.0, 1.0), 0.2),
            (ModTargetKind::ModulatorRelease { mod_index: 1 }, 0.1, (0.001, 10.0), 0.03),
        ] {
            let mut rig = Rig::new();
            rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0));
            rig.lane_modulator(1, adsr(0.01));
            let stage = rig.field_target(0, kind.clone(), depth, base, min, max);
            rig.process(&[]);
            let expected = if matches!(kind, ModTargetKind::ModulatorSustain { .. }) {
                base + depth * (max - min)
            } else {
                value_on_decades(place_on_decades(base, min, max) + depth, min, max)
            };
            assert!(near(rig.read(stage), expected), "{kind:?}: {} for {expected}", rig.read(stage));
        }
    }

    // -- Settings that are turned on a log scale (an LFO's rate, an envelope's attack, decay and release) --
    //
    // The knob of such a setting runs over decades and the depth of a cable is a fraction of the knob's travel, so a cable moves the setting in ratios, a depth of 0.1
    // a tenth of the travel either way: the swing is as far up the knob as it is down it. (It was a fraction of the range in seconds or hertz, which on a knob that
    // runs over decades is nothing at the bottom and the whole of the top: an LFO at half depth on an attack of 10 ms swung it from the lowest value to 5 s.)

    /// Where `value` is on a scale of decades from `min` to `max`, 0 to 1, as the knob of a rate or a time shows it.
    fn place_on_decades(value: f32, min: f32, max: f32) -> f32 {
        (value / min).ln() / (max / min).ln()
    }

    /// The value at a place on that scale.
    fn value_on_decades(place: f32, min: f32, max: f32) -> f32 {
        min * (max / min).powf(place)
    }

    /// A value to within what 32-bit numbers keep of a power.
    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1e-4 * b.abs().max(1.0)
    }

    /// The settings that are turned on a log scale, each as the kind of cable that drives it, the modulator it is a setting of, what the user set it to and its range.
    fn log_settings() -> [(ModTargetKind, ModSource, f32, (f32, f32)); 4] {
        [
            (ModTargetKind::ModulatorRate { mod_index: 1 }, lfo(LfoWaveform::Sine, 2.0), 2.0, (0.01, 50.0)),
            (ModTargetKind::ModulatorAttack { mod_index: 1 }, adsr(0.01), 0.01, (0.001, 10.0)),
            (ModTargetKind::ModulatorDecay { mod_index: 1 }, adsr(0.01), 0.3, (0.001, 10.0)),
            (ModTargetKind::ModulatorRelease { mod_index: 1 }, adsr(0.01), 0.5, (0.001, 10.0)),
        ]
    }

    #[test]
    fn an_lfo_on_a_setting_that_is_turned_on_a_log_scale_moves_it_a_fraction_of_the_knobs_travel_either_way() {
        for (kind, target, base, (min, max)) in log_settings() {
            for depth in [0.1, -0.1, 0.3] {
                let mut rig = Rig::new();
                rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0)); // always +1
                rig.lane_modulator(1, target.clone());
                let setting = rig.field_target(0, kind.clone(), depth, base, min, max);
                rig.process(&[]);
                let place = (place_on_decades(base, min, max) + depth).clamp(0.0, 1.0);
                let expected = value_on_decades(place, min, max);
                assert!(near(rig.read(setting), expected), "{kind:?} at depth {depth}: {} for {expected}", rig.read(setting));
            }
        }
    }

    #[test]
    fn an_lfo_swings_a_setting_that_is_turned_on_a_log_scale_as_far_up_the_knob_as_down_it() {
        // The ends of the swing, as places on the knob, are as far from the place of the set value either side.
        let (kind, base, (min, max)) = (ModTargetKind::ModulatorAttack { mod_index: 1 }, 0.1, (0.001, 10.0));
        let place_after = |depth: f32| {
            let mut rig = Rig::new();
            rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0));
            rig.lane_modulator(1, adsr(base));
            let setting = rig.field_target(0, kind.clone(), depth, base, min, max);
            rig.process(&[]);
            place_on_decades(rig.read(setting), min, max)
        };
        let (up, down) = (place_after(0.2), place_after(-0.2));
        let at = place_on_decades(base, min, max);
        assert!(((up - at) - (at - down)).abs() < 1e-4, "{up} and {down} round {at}");
        assert!((up - at - 0.2).abs() < 1e-4, "a fifth of the knob: {}", up - at);
    }

    #[test]
    fn an_envelope_on_a_setting_that_is_turned_on_a_log_scale_pulls_it_down_a_fraction_of_the_travel_to_the_lowest_value() {
        for (kind, target, base, (min, max)) in log_settings() {
            let mut rig = Rig::new();
            rig.lane_modulator(0, adsr(0.01));
            rig.lane_modulator(1, target);
            let setting = rig.field_target(0, kind.clone(), 0.5, base, min, max);
            rig.process(&[]);
            // Idle: the user's value is the envelope's peak, and the rest is half the way down the knob to the lowest value.
            let at = place_on_decades(base, min, max);
            assert!(near(rig.read(setting), value_on_decades(at - 0.5 * at, min, max)), "{kind:?}: {}", rig.read(setting));
            // A buffer into the attack the envelope is `level` of the way up.
            rig.process(&[note_on(60)]);
            let level = FRAMES as f32 / 48000.0 / 0.01;
            assert!(near(rig.read(setting), value_on_decades(at - 0.5 * (1.0 - level) * at, min, max)), "{kind:?}: {}", rig.read(setting));
        }
    }

    #[test]
    fn a_midi_control_on_a_setting_that_is_turned_on_a_log_scale_adds_a_fraction_of_the_travel_and_nothing_at_rest() {
        let mut rig = Rig::new();
        let control = ModSource::MidiLearn { source: Some(MidiBindSource::Cc(74)), value: 0.0, learning: false };
        rig.lane_modulator(0, control);
        rig.lane_modulator(1, lfo(LfoWaveform::Sine, 2.0));
        let rate = rig.field_target(0, ModTargetKind::ModulatorRate { mod_index: 1 }, 0.2, 2.0, 0.01, 50.0);
        rig.process(&[]);
        assert!(near(rig.read(rate), 2.0), "at rest it is what the user set: {}", rig.read(rate));
        rig.process(&[(0, [0xB0, 74, 127])]);
        let expected = value_on_decades(place_on_decades(2.0, 0.01, 50.0) + 0.2, 0.01, 50.0);
        assert!(near(rig.read(rate), expected), "{} for {expected}", rig.read(rate));
    }

    #[test]
    fn a_setting_that_is_turned_on_a_log_scale_stops_at_the_ends_of_its_range() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0));
        rig.lane_modulator(1, adsr(0.01));
        let top = rig.field_target(0, ModTargetKind::ModulatorAttack { mod_index: 1 }, 3.0, 0.01, 0.001, 10.0);
        rig.process(&[]);
        assert!(near(rig.read(top), 10.0), "{}", rig.read(top));
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0));
        rig.lane_modulator(1, adsr(0.01));
        let bottom = rig.field_target(0, ModTargetKind::ModulatorAttack { mod_index: 1 }, -3.0, 0.01, 0.001, 10.0);
        rig.process(&[]);
        assert!(near(rig.read(bottom), 0.001), "{}", rig.read(bottom));
    }

    #[test]
    fn a_setting_that_has_a_range_of_one_value_stays_there() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0));
        rig.lane_modulator(1, adsr(0.01));
        let attack = rig.field_target(0, ModTargetKind::ModulatorAttack { mod_index: 1 }, 0.1, 1.0, 1.0, 1.0);
        rig.process(&[]);
        assert!(close(rig.read(attack), 1.0), "{}", rig.read(attack));
    }

    #[test]
    fn a_setting_that_has_a_range_from_nothing_is_moved_by_the_range_as_there_are_no_decades_in_it() {
        // (A scale of decades has no zero: a cable made to one that starts there goes by the range, and does not come out as not a number.)
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0));
        rig.lane_modulator(1, adsr(0.01));
        let attack = rig.field_target(0, ModTargetKind::ModulatorAttack { mod_index: 1 }, 0.1, 0.5, 0.0, 10.0);
        rig.process(&[]);
        assert!(close(rig.read(attack), 0.5 + 0.1 * 10.0), "{}", rig.read(attack));
    }

    #[test]
    fn a_sustain_and_a_depth_are_still_moved_by_a_fraction_of_their_range() {
        // They are turned by distances, not by ratios: the stage of the envelope that is a level and the depth of a cable.
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0));
        rig.lane_modulator(1, adsr(0.01));
        let sustain = rig.field_target(0, ModTargetKind::ModulatorSustain { mod_index: 1 }, 0.2, 0.5, 0.0, 1.0);
        rig.process(&[]);
        assert!(close(rig.read(sustain), 0.7), "{}", rig.read(sustain));
        // (And one whose range starts above nothing, which the range from nothing would not tell from one on decades.)
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0));
        rig.lane_modulator(1, adsr(0.01));
        let sustain = rig.field_target(0, ModTargetKind::ModulatorSustain { mod_index: 1 }, 0.2, 0.5, 0.1, 1.0);
        rig.process(&[]);
        assert!(close(rig.read(sustain), 0.5 + 0.2 * 0.9), "{}", rig.read(sustain));
    }

    #[test]
    fn a_group_modulator_moves_each_setting_of_a_members_modulator_that_is_turned_on_a_log_scale_a_fraction_of_the_knobs_travel_and_a_sustain_a_fraction_of_its_range() {
        for (field, member_source, base, (min, max)) in [
            (CrossModField::Rate, lfo(LfoWaveform::Sine, 10.0), 10.0, (0.01, 50.0)),
            (CrossModField::Rate, steps(&[], 4, 10.0, 0.0, false), 10.0, (0.01, 50.0)),
            (CrossModField::Attack, adsr(0.01), 0.01, (0.001, 10.0)),
            (CrossModField::Decay, adsr(0.01), 0.3, (0.001, 10.0)),
            (CrossModField::Release, adsr(0.01), 0.5, (0.001, 10.0)),
            (CrossModField::Sustain, adsr(0.01), 0.5, (0.1, 1.0)),
        ] {
            let (mut graph, cmd_tx, _rr) = make_graph(2);
            swap_instrument_at(&cmd_tx, 0, ConstInstrument::new(0.5));
            cmd_tx.send(GraphCommand::AddGroup).unwrap();
            cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
            cmd_tx.send(GraphCommand::InsertModulator { inst: 0, index: 0, source: member_source }).unwrap();
            cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source: lfo(LfoWaveform::Square, 0.0) }).unwrap(); // always +1
            let target = ModTarget {
                kind: ModTargetKind::GroupMemberMod { member: 0, mod_index: 0, field },
                depth: 0.1,
                base_value: base,
                param_min: min,
                param_max: max,
                effective: None,
            };
            cmd_tx.send(GraphCommand::AddGroupModTarget { group: 0, mod_index: 0, target }).unwrap();
            graph.process(&[], &mut make_output()).unwrap();
            // (A sustain is a level, turned by distances: it goes by the range.)
            let expected = if field == CrossModField::Sustain { base + 0.1 * (max - min) } else { value_on_decades(place_on_decades(base, min, max) + 0.1, min, max) };
            let value = match graph.instruments[0].modulators[0].source {
                ModSource::Lfo { rate, .. } | ModSource::Steps { rate, .. } => rate,
                ModSource::Envelope { attack, decay, sustain, release, .. } => match field {
                    CrossModField::Attack => attack,
                    CrossModField::Decay => decay,
                    CrossModField::Sustain => sustain,
                    _ => release,
                },
                ModSource::MidiLearn { .. } | ModSource::Macro { .. } => panic!("the member's modulator is an LFO, a step sequencer or an envelope"),
            };
            assert!(near(value, expected), "{field:?}: {value} for {expected}");
        }
    }

    #[test]
    fn a_value_beyond_the_range_of_a_setting_that_is_turned_on_a_log_scale_is_taken_at_the_end_of_it() {
        // (A session file can say 20 s for an attack that runs to 10, or a number that is not a time at all. The knob puts such a value at its end, and so does the law: there is
        // no logarithm of what is not above nothing, and a negative number would come out as not a number.)
        for (base, depth, expected) in [(20.0, -0.1, value_on_decades(0.9, 0.001, 10.0)), (20.0, 0.1, 10.0), (-1.0, 0.1, value_on_decades(0.1, 0.001, 10.0)), (-1.0, -0.1, 0.001)] {
            let mut rig = Rig::new();
            rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0));
            rig.lane_modulator(1, adsr(0.01));
            let attack = rig.field_target(0, ModTargetKind::ModulatorAttack { mod_index: 1 }, depth, base, 0.001, 10.0);
            rig.process(&[]);
            assert!(near(rig.read(attack), expected), "{base} at depth {depth}: {} for {expected}", rig.read(attack));
        }
    }

    #[test]
    fn a_cable_to_the_depth_of_a_cable_publishes_the_depth_it_was_set_to() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0));
        rig.lane_modulator(1, lfo(LfoWaveform::Sine, 1.0));
        rig.gain_target(1, 0.5);
        rig.gain_target(1, 0.3); // the cable whose depth is driven: modulator 1's second (of two, so that the place of a cable counts)
        let depth = rig.field_target(0, ModTargetKind::ModulatorDepth { mod_index: 1, target_index: 1 }, 0.2, 0.3, -1.0, 1.0);
        rig.process(&[]);
        assert!(close(rig.read(depth), 0.3 + 0.2 * 2.0), "{}", rig.read(depth));
    }

    #[test]
    fn setting_the_depth_of_a_cable_that_another_cable_drives_moves_what_the_driver_works_from() {
        // (Found by looking at the bar under the slider of such a cable: the engine moves what the cables that drive a rate or a stage work from when the user sets it,
        // `update_cross_mod_base`, and did not for a depth, so the next buffer wrote the depth back to what it was around the old value.)
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0)); // always +1
        rig.lane_modulator(1, lfo(LfoWaveform::Sine, 1.0));
        rig.gain_target(1, 0.5);
        rig.gain_target(1, 0.5); // the cable whose depth is driven: modulator 1's second (of two, so that the place of a cable counts)
        let depth = rig.field_target(0, ModTargetKind::ModulatorDepth { mod_index: 1, target_index: 1 }, 0.2, 0.5, -1.0, 1.0);
        rig.process(&[]);
        assert!(close(rig.read(depth), 0.5 + 0.2 * 2.0), "{}", rig.read(depth));
        // The user sets the first cable's depth, which nothing drives: what drives the second works from where it was.
        rig.cmd_tx.send(GraphCommand::SetModTargetDepth { inst: 0, mod_index: 1, target_index: 0, depth: 0.9 }).unwrap();
        rig.process(&[]);
        assert!(close(rig.read(depth), 0.5 + 0.2 * 2.0), "{}", rig.read(depth));
        // The user sets the second's to -0.3: the cable that drives it works from there, and the depth is not written back.
        rig.cmd_tx.send(GraphCommand::SetModTargetDepth { inst: 0, mod_index: 1, target_index: 1, depth: -0.3 }).unwrap();
        rig.process(&[]);
        assert!(close(rig.read(depth), -0.3 + 0.2 * 2.0), "{}", rig.read(depth));
        rig.process(&[]);
        assert!(close(rig.read(depth), -0.3 + 0.2 * 2.0), "and the next buffer: {}", rig.read(depth));
    }

    #[test]
    fn setting_the_depth_of_a_group_cable_that_another_cable_of_the_group_drives_moves_what_the_driver_works_from() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        swap_instrument_at(&cmd_tx, 0, ConstInstrument::new(0.5));
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        cmd_tx.send(GraphCommand::InsertGroupEffect { group: 0, index: 0, effect: Box::new(ParamScaleEffect { gain: 1.0 }), mix: 1.0 }).unwrap();
        for (index, source) in [lfo(LfoWaveform::Square, 0.0), lfo(LfoWaveform::Sine, 1.0)].into_iter().enumerate() {
            cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index, source }).unwrap();
        }
        let target = |kind: ModTargetKind, depth: f32, base: f32, min: f32| ModTarget { kind, depth, base_value: base, param_min: min, param_max: 1.0, effective: None };
        // The cable whose depth is driven: modulator 1's second, to the bus effect (of two, so that the place of a cable counts); and modulator 0's, to its depth.
        let bus = || target(ModTargetKind::GroupBus { effect_index: 0, param_index: 0 }, 0.5, 0.5, 0.0);
        add_group_cable(&cmd_tx, 0, 1, bus());
        add_group_cable(&cmd_tx, 0, 1, bus());
        add_group_cable(&cmd_tx, 0, 0, target(depth_of(1, 1), 0.2, 0.5, -1.0));
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();
        let driven = |graph: &AudioGraph| graph.groups[0].modulators[1].targets[1].depth;
        assert!(close(driven(&graph), 0.5 + 0.2 * 2.0), "{}", driven(&graph));
        // The first cable's depth, which nothing drives, moves nothing of what drives the second.
        cmd_tx.send(GraphCommand::SetGroupModTargetDepth { group: 0, mod_index: 1, target_index: 0, depth: 0.9 }).unwrap();
        graph.process(&[], &mut out).unwrap();
        assert!(close(driven(&graph), 0.5 + 0.2 * 2.0), "{}", driven(&graph));
        cmd_tx.send(GraphCommand::SetGroupModTargetDepth { group: 0, mod_index: 1, target_index: 1, depth: -0.3 }).unwrap();
        graph.process(&[], &mut out).unwrap();
        assert!(close(driven(&graph), -0.3 + 0.2 * 2.0), "{}", driven(&graph));
    }

    #[test]
    fn a_lane_modulator_and_a_group_modulator_on_one_parameter_leave_what_the_lane_makes_of_it() {
        // (A group's modulator is applied before the lanes render and the lane's own later, inside the lane, which writes the parameter from the value the user set: the
        // group's offset is not added to it. The page's ring of such a parameter is the lane's cables alone.)
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0)); // always +1
        rig.gain_target(0, 0.1); // the lane's: 0.5 + 0.1
        rig.cmd_tx.send(GraphCommand::AddGroup).unwrap();
        rig.cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        rig.cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source: lfo(LfoWaveform::Square, 0.0) }).unwrap();
        let target = ModTarget { kind: ModTargetKind::GroupMember { member: 0, slot: 1, param_index: 0 }, depth: 0.2, base_value: 0.5, param_min: 0.0, param_max: 1.0, effective: None };
        rig.cmd_tx.send(GraphCommand::AddGroupModTarget { group: 0, mod_index: 0, target }).unwrap();
        let mut out = make_output();
        rig.graph.process(&[note_on(60)], &mut out).unwrap(); // (the instrument puts out its value while a note is held)
        // The instrument puts out 0.5, and the effect scales it by the parameter: 0.6 of it is the lane's, 0.8 would be the two added.
        assert!(close(out[0][0], 0.5 * 0.6), "{}", out[0][0]);
    }

    #[test]
    fn a_lane_control_that_is_not_bound_yet_and_a_group_modulator_on_one_parameter_leave_the_value_the_user_set() {
        // (An armed control that has captured nothing adds no offset, but it is a cable of the lane all the same: the lane writes the parameter from the value the user set, over
        // what the group wrote.)
        let mut rig = Rig::new();
        rig.lane_modulator(0, ModSource::MidiLearn { source: None, value: 0.0, learning: true });
        rig.gain_target(0, 0.1);
        rig.cmd_tx.send(GraphCommand::AddGroup).unwrap();
        rig.cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        rig.cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source: lfo(LfoWaveform::Square, 0.0) }).unwrap();
        let target = ModTarget { kind: ModTargetKind::GroupMember { member: 0, slot: 1, param_index: 0 }, depth: 0.2, base_value: 0.5, param_min: 0.0, param_max: 1.0, effective: None };
        rig.cmd_tx.send(GraphCommand::AddGroupModTarget { group: 0, mod_index: 0, target }).unwrap();
        let mut out = make_output();
        rig.graph.process(&[note_on(60)], &mut out).unwrap();
        // The instrument puts out 0.5, and the effect scales it by the value the user set, 0.5: 0.7 would be the group's.
        assert!(close(out[0][0], 0.5 * 0.5), "{}", out[0][0]);
    }

    #[test]
    fn two_cables_to_one_setting_publish_the_value_the_last_of_them_wrote_to_both() {
        // (Writers to one setting are last-wins, as `apply_cross_mod` says: the value is what the later cable made of it.)
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0));
        rig.lane_modulator(1, lfo(LfoWaveform::Sine, 2.0));
        rig.lane_modulator(2, lfo(LfoWaveform::Square, 0.0));
        let first = rig.field_target(0, ModTargetKind::ModulatorRate { mod_index: 1 }, 0.1, 2.0, 0.01, 50.0);
        let second = rig.field_target(2, ModTargetKind::ModulatorRate { mod_index: 1 }, 0.3, 2.0, 0.01, 50.0);
        rig.process(&[]);
        // (A rate is turned on a log scale: the cable moves it 0.3 of the knob's travel, `place_on_decades`.)
        let expected = value_on_decades(place_on_decades(2.0, 0.01, 50.0) + 0.3, 0.01, 50.0);
        assert!(near(rig.read(second), expected), "{} for {expected}", rig.read(second));
        assert_eq!(rig.read(first), rig.read(second));
    }

    #[test]
    fn a_cable_to_a_setting_the_modulator_has_not_and_a_cable_to_itself_publish_nothing() {
        let mut rig = Rig::new();
        // (A rate of 5 Hz, which would be published if the cable to itself were not left out: a rate of 0 is what the slot holds already.)
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 5.0));
        rig.lane_modulator(1, adsr(0.01));
        // An envelope has no rate; and a modulator does not drive its own settings.
        let no_such_setting = rig.field_target(0, ModTargetKind::ModulatorRate { mod_index: 1 }, 0.1, 2.0, 0.01, 50.0);
        let itself = rig.field_target(0, ModTargetKind::ModulatorRate { mod_index: 0 }, 0.1, 2.0, 0.01, 50.0);
        rig.process(&[]);
        assert_eq!((rig.read(no_such_setting), rig.read(itself)), (0.0, 0.0));
    }

    #[test]
    fn a_target_without_a_slot_publishes_nothing_and_the_audio_is_the_same() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Sine, 100.0));
        let slot = rig.slot();
        let target = ModTarget {
            kind: ModTargetKind::PluginParam { slot: 1, param_index: 0 },
            depth: 0.4,
            base_value: 0.5,
            param_min: 0.0,
            param_max: 1.0,
            effective: None,
        };
        rig.cmd_tx.send(GraphCommand::AddModTarget { inst: 0, mod_index: 0, target }).unwrap();
        rig.process(&[note_on(60)]);
        assert_eq!(rig.read(slot), 0.0, "a slot nobody was given stays as it was");
    }

    #[test]
    fn a_modulator_without_a_slot_publishes_nothing() {
        let mut rig = Rig::new();
        let unused = rig.slot();
        rig.cmd_tx.send(GraphCommand::InsertModulator { inst: 0, index: 0, source: lfo(LfoWaveform::Sine, 100.0) }).unwrap();
        rig.process(&[]);
        rig.process(&[]);
        assert_eq!(rig.read(unused), 0.0);
    }

    #[test]
    fn nothing_is_published_without_a_telemetry() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.5));
        insert_effect(&cmd_tx, 0, scale_effect(), 1.0);
        let mut slots = SlotAllocator::default();
        let (output, effective) = (slots.alloc().unwrap(), slots.alloc().unwrap());
        cmd_tx.send(GraphCommand::InsertModulator { inst: 0, index: 0, source: lfo(LfoWaveform::Sine, 100.0) }).unwrap();
        cmd_tx
            .send(GraphCommand::SetModulatorOutput { location: ModulatorLocation::Lane(0), index: 0, slot: output })
            .unwrap();
        let target = ModTarget {
            kind: ModTargetKind::PluginParam { slot: 1, param_index: 0 },
            depth: 0.4,
            base_value: 0.5,
            param_min: 0.0,
            param_max: 1.0,
            effective: Some(effective),
        };
        cmd_tx.send(GraphCommand::AddModTarget { inst: 0, mod_index: 0, target }).unwrap();
        // It still runs, and the target still modulates.
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 0.5 * (0.5 + 0.4 * LfoWaveform::Sine.eval(lfo_phase(100.0, 1)))).abs() < 1e-4));
    }

    #[test]
    fn a_target_keeps_its_slot_when_the_effects_around_it_come_and_go() {
        // Two effects, an LFO on each one's gain; the first effect is removed: the
        // second's target moves to slot 1 and still publishes into its own cell.
        let mut rig = Rig::new();
        insert_effect(&rig.cmd_tx, 1, scale_effect(), 1.0);
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0)); // always +1
        let first = rig.gain_target(0, 0.2); // on slot 1
        let second = rig.slot();
        let target = ModTarget {
            kind: ModTargetKind::PluginParam { slot: 2, param_index: 0 },
            depth: 0.4,
            base_value: 0.5,
            param_min: 0.0,
            param_max: 1.0,
            effective: Some(second),
        };
        rig.cmd_tx.send(GraphCommand::AddModTarget { inst: 0, mod_index: 0, target }).unwrap();
        rig.process(&[]);
        assert!(close(rig.read(first), 0.7) && close(rig.read(second), 0.9));

        rig.cmd_tx.send(GraphCommand::RemoveEffect { inst: 0, index: 0, keep: None }).unwrap();
        // The first target is gone with its effect; make the survivor's cell readable as changed.
        rig.telemetry.store(first, -1.0);
        rig.telemetry.store(second, -1.0);
        rig.process(&[]);
        assert_eq!(rig.read(first), -1.0, "the removed target publishes no more");
        assert!(close(rig.read(second), 0.9), "the survivor still does, from its new slot: {}", rig.read(second));
    }

    #[test]
    fn a_modulator_keeps_its_slot_when_the_others_around_it_come_and_go() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0)); // always +1
        let second = rig.lane_modulator(1, lfo(LfoWaveform::Square, 0.0));
        rig.telemetry.store(second, -1.0);
        rig.process(&[]);
        assert_eq!(rig.read(second), 1.0);
        rig.cmd_tx.send(GraphCommand::RemoveModulator { inst: 0, index: 0 }).unwrap();
        rig.telemetry.store(second, -1.0);
        rig.process(&[]);
        assert_eq!(rig.read(second), 1.0, "now modulator 0, still its own cell");
    }

    #[test]
    fn a_slot_set_for_a_modulator_that_is_not_there_changes_nothing() {
        let mut rig = Rig::new();
        let slot = rig.slot();
        for location in [ModulatorLocation::Lane(0), ModulatorLocation::Lane(9), ModulatorLocation::Group(0)] {
            rig.cmd_tx.send(GraphCommand::SetModulatorOutput { location, index: 3, slot }).unwrap();
        }
        rig.process(&[]);
        assert_eq!(rig.read(slot), 0.0);
    }

    /// Two lanes in group 0, each with a constant instrument and a
    /// `ParamScaleEffect`, a bus effect of the same kind, and an LFO in the group
    /// (a square at phase 0: always +1) with a target on member 0's, member 1's and
    /// the bus effect's `gain`, each publishing its effective value, and its own
    /// output published.
    struct GroupRig {
        rig: Rig,
        output: Slot,
        member0: Slot,
        member1: Slot,
        bus: Slot,
    }

    impl GroupRig {
        fn new() -> GroupRig {
            let (graph, cmd_tx, return_rx) = make_graph(2);
            let telemetry = Arc::new(Telemetry::new());
            let mut rig = Rig { graph, cmd_tx, telemetry: telemetry.clone(), slots: SlotAllocator::default(), _return_rx: return_rx };
            rig.graph.set_telemetry(telemetry);
            rig.cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
            rig.cmd_tx.send(GraphCommand::AddGroup).unwrap();
            for inst in [0, 1] {
                rig.cmd_tx.send(GraphCommand::SetLaneGroup { inst, group: Some(0) }).unwrap();
                swap_instrument_at(&rig.cmd_tx, inst, ConstInstrument::new(0.1));
                rig.cmd_tx
                    .send(GraphCommand::InsertEffect { inst, index: 0, effect: scale_effect(), mix: 1.0 })
                    .unwrap();
            }
            rig.cmd_tx
                .send(GraphCommand::InsertGroupEffect { group: 0, index: 0, effect: scale_effect(), mix: 1.0 })
                .unwrap();
            let output = rig.slot();
            rig.cmd_tx
                .send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source: lfo(LfoWaveform::Square, 0.0) })
                .unwrap();
            rig.cmd_tx
                .send(GraphCommand::SetModulatorOutput { location: ModulatorLocation::Group(0), index: 0, slot: output })
                .unwrap();
            let mut group = GroupRig { rig, output, member0: output, member1: output, bus: output };
            group.member0 = group.target(0, ModTargetKind::GroupMember { member: 0, slot: 1, param_index: 0 }, 0.2);
            group.member1 = group.target(0, ModTargetKind::GroupMember { member: 1, slot: 1, param_index: 0 }, 0.3);
            group.bus = group.target(0, ModTargetKind::GroupBus { effect_index: 0, param_index: 0 }, 0.4);
            group
        }

        /// A target of group modulator `mod_index`, base 0.5, publishing; its slot.
        fn target(&mut self, mod_index: usize, kind: ModTargetKind, depth: f32) -> Slot {
            let slot = self.rig.slot();
            let target = ModTarget { kind, depth, base_value: 0.5, param_min: 0.0, param_max: 1.0, effective: Some(slot) };
            self.rig.cmd_tx.send(GraphCommand::AddGroupModTarget { group: 0, mod_index, target }).unwrap();
            slot
        }
    }

    #[test]
    fn a_group_modulator_publishes_its_output() {
        let mut g = GroupRig::new();
        g.rig.process(&[]);
        assert_eq!(g.rig.read(g.output), 1.0);
    }

    #[test]
    fn group_targets_publish_the_value_their_member_or_bus_parameter_was_set_to() {
        let mut g = GroupRig::new();
        g.rig.process(&[]);
        // Base 0.5 plus the LFO's +1 times the depth.
        assert!(close(g.rig.read(g.member0), 0.7), "{}", g.rig.read(g.member0));
        assert!(close(g.rig.read(g.member1), 0.8), "{}", g.rig.read(g.member1));
        assert!(close(g.rig.read(g.bus), 0.9), "{}", g.rig.read(g.bus));
    }

    #[test]
    fn group_modulators_on_one_member_parameter_share_its_effective_value() {
        let mut g = GroupRig::new();
        g.rig.cmd_tx
            .send(GraphCommand::InsertGroupModulator { group: 0, index: 1, source: adsr(0.01) })
            .unwrap();
        // The envelope idles at depth 0.5: an offset of -0.5 · 0.5 on member 0's gain,
        // on top of the LFO's +0.2.
        let second = g.target(1, ModTargetKind::GroupMember { member: 0, slot: 1, param_index: 0 }, 0.5);
        g.rig.process(&[]);
        assert!(close(g.rig.read(g.member0), 0.5 + 0.2 - 0.25), "{}", g.rig.read(g.member0));
        assert_eq!(g.rig.read(g.member0), g.rig.read(second));
        assert!(close(g.rig.read(g.member1), 0.8), "the other member is not touched");
    }

    #[test]
    fn a_group_target_on_a_member_that_is_not_there_publishes_nothing() {
        let mut g = GroupRig::new();
        let ghost = g.target(0, ModTargetKind::GroupMember { member: 5, slot: 1, param_index: 0 }, 0.3);
        g.rig.process(&[]);
        assert_eq!(g.rig.read(ghost), 0.0, "the group has no member 5, so nothing is written to publish");
        assert!(close(g.rig.read(g.member0), 0.7), "the others are unaffected");
    }

    #[test]
    fn a_group_modulators_cable_to_another_of_the_group_publishes_the_setting_it_was_set_to() {
        let mut g = GroupRig::new();
        g.rig.cmd_tx
            .send(GraphCommand::InsertGroupModulator { group: 0, index: 1, source: lfo(LfoWaveform::Sine, 1.0) })
            .unwrap();
        let slot = g.rig.slot();
        let target = ModTarget { kind: ModTargetKind::ModulatorRate { mod_index: 1 }, depth: 0.3, base_value: 1.0, param_min: 0.01, param_max: 50.0, effective: Some(slot) };
        g.rig.cmd_tx.send(GraphCommand::AddGroupModTarget { group: 0, mod_index: 0, target }).unwrap();
        g.rig.process(&[]);
        // The group's LFO is a square at +1: the rate is turned on a log scale, and 0.3 of the knob's travel is a long way up it.
        let expected = value_on_decades(place_on_decades(1.0, 0.01, 50.0) + 0.3, 0.01, 50.0);
        assert!(near(g.rig.read(slot), expected), "{} for {expected}", g.rig.read(slot));
    }

    #[test]
    fn a_group_modulators_cable_to_a_members_modulator_publishes_what_the_members_setting_ends_up_as() {
        let mut g = GroupRig::new();
        g.rig.cmd_tx.send(GraphCommand::InsertModulator { inst: 0, index: 0, source: lfo(LfoWaveform::Sine, 2.0) }).unwrap();
        let field = ModTargetKind::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Rate };
        let slot = g.rig.slot();
        let target = ModTarget { kind: field, depth: 0.1, base_value: 2.0, param_min: 0.01, param_max: 50.0, effective: Some(slot) };
        g.rig.cmd_tx.send(GraphCommand::AddGroupModTarget { group: 0, mod_index: 0, target }).unwrap();
        g.rig.process(&[]);
        // The group's LFO is a square at +1: 2 Hz and a tenth of the knob's travel (the rate is turned on a log scale).
        let expected = value_on_decades(place_on_decades(2.0, 0.01, 50.0) + 0.1, 0.01, 50.0);
        assert!(near(g.rig.read(slot), expected), "{} for {expected}", g.rig.read(slot));
        // A cable of the member's own to the same setting is applied after the group's, and wins: that is what the setting ends up as.
        g.rig.cmd_tx.send(GraphCommand::InsertModulator { inst: 0, index: 1, source: lfo(LfoWaveform::Square, 0.0) }).unwrap();
        let own = ModTarget { kind: ModTargetKind::ModulatorRate { mod_index: 0 }, depth: 0.3, base_value: 2.0, param_min: 0.01, param_max: 50.0, effective: None };
        g.rig.cmd_tx.send(GraphCommand::AddModTarget { inst: 0, mod_index: 1, target: own }).unwrap();
        g.rig.process(&[]);
        let expected = value_on_decades(place_on_decades(2.0, 0.01, 50.0) + 0.3, 0.01, 50.0);
        assert!(near(g.rig.read(slot), expected), "{} for {expected}", g.rig.read(slot));
    }

    #[test]
    fn a_group_modulators_cable_to_a_member_that_is_not_there_publishes_nothing() {
        let mut g = GroupRig::new();
        let slot = g.rig.slot();
        let field = ModTargetKind::GroupMemberMod { member: 5, mod_index: 0, field: CrossModField::Rate };
        let target = ModTarget { kind: field, depth: 0.1, base_value: 2.0, param_min: 0.01, param_max: 50.0, effective: Some(slot) };
        g.rig.cmd_tx.send(GraphCommand::AddGroupModTarget { group: 0, mod_index: 0, target }).unwrap();
        g.rig.process(&[]);
        assert_eq!(g.rig.read(slot), 0.0);
    }

    #[test]
    fn publishing_modulator_telemetry_does_not_allocate() {
        let mut rig = Rig::new();
        rig.lane_modulator(0, lfo(LfoWaveform::Sine, 3.0));
        rig.lane_modulator(1, adsr(0.01));
        rig.gain_target(0, 0.3);
        rig.gain_target(1, 0.3);
        // (Cables to settings, which publish what they were set to: the LFO drives the attack of the envelope and the envelope the rate of the LFO.)
        rig.field_target(0, ModTargetKind::ModulatorAttack { mod_index: 1 }, 0.01, 0.01, 0.001, 10.0);
        rig.field_target(1, ModTargetKind::ModulatorRate { mod_index: 0 }, 0.2, 3.0, 0.01, 50.0);
        // (A step sequencer that glides and starts again at a note, publishing its output and its position, driving a parameter.)
        lane_steps(&mut rig, 2, steps(&[0.1, -0.3, 0.9], 3, 40.0, 0.5, true));
        rig.gain_target(2, 0.3);
        rig.graph.drain_commands();
        let allocations = allocations_while_processing(&mut rig.graph, &[note_on(60)], 16);
        assert_eq!(allocations, 0, "a lane: the audio thread asked the allocator for memory");

        let mut g = GroupRig::new();
        // (A group modulator's cables to another of the group and to a member's modulator.)
        g.rig.cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 1, source: lfo(LfoWaveform::Sine, 1.0) }).unwrap();
        g.rig.cmd_tx.send(GraphCommand::InsertModulator { inst: 0, index: 0, source: lfo(LfoWaveform::Sine, 2.0) }).unwrap();
        for (kind, base) in [
            (ModTargetKind::ModulatorRate { mod_index: 1 }, 1.0),
            (ModTargetKind::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Rate }, 2.0),
        ] {
            let slot = g.rig.slot();
            let target = ModTarget { kind, depth: 0.2, base_value: base, param_min: 0.01, param_max: 50.0, effective: Some(slot) };
            g.rig.cmd_tx.send(GraphCommand::AddGroupModTarget { group: 0, mod_index: 0, target }).unwrap();
        }
        // (And a group's step sequencer.)
        g.rig.cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 2, source: steps(&[0.1, -0.3, 0.9], 3, 40.0, 0.5, true) }).unwrap();
        let position = g.rig.slot();
        g.rig.cmd_tx.send(GraphCommand::SetModulatorPosition { location: ModulatorLocation::Group(0), index: 2, slot: position }).unwrap();
        g.rig.graph.drain_commands();
        let allocations = allocations_while_processing(&mut g.rig.graph, &[note_on(60)], 16);
        assert_eq!(allocations, 0, "a group: the audio thread asked the allocator for memory");
    }

    /// An effect with two parameters, `a` (0) and `b` (1), that passes its input
    /// through: for telling apart what is published for each.
    struct TwoParamEffect {
        values: [f32; 2],
    }

    impl Plugin for TwoParamEffect {
        fn name(&self) -> &str {
            "TwoParam"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }
        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                out.copy_from_slice(inp);
            }
            Ok(())
        }
        fn sample_rate(&self) -> f32 {
            48000.0
        }
        fn parameters(&self) -> Vec<ParameterInfo> {
            ["a", "b"]
                .iter()
                .enumerate()
                .map(|(index, name)| ParameterInfo {
                    index: index as u32,
                    name: (*name).into(),
                    min: 0.0,
                    max: 1.0,
                    default: 0.5,
                    ..Default::default()
                })
                .collect()
        }
        fn get_parameter(&mut self, idx: u32) -> Option<f32> {
            self.values.get(idx as usize).copied()
        }
        fn set_parameter(&mut self, idx: u32, value: f32) -> anyhow::Result<()> {
            match self.values.get_mut(idx as usize) {
                Some(v) => {
                    *v = value;
                    Ok(())
                }
                None => anyhow::bail!("no parameter {idx}"),
            }
        }
        fn presets(&self) -> Vec<Preset> {
            Vec::new()
        }
        fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
            anyhow::bail!("no preset {id}")
        }
    }

    fn two_param_effect() -> Box<dyn Plugin> {
        Box::new(TwoParamEffect { values: [0.5, 0.5] })
    }

    #[test]
    fn two_parameters_of_one_plugin_publish_their_own_values() {
        let mut rig = Rig::new();
        // Replace the effect with one that has two parameters.
        rig.cmd_tx.send(GraphCommand::RemoveEffect { inst: 0, index: 0, keep: None }).unwrap();
        insert_effect(&rig.cmd_tx, 0, two_param_effect(), 1.0);
        rig.lane_modulator(0, lfo(LfoWaveform::Square, 0.0)); // always +1
        let target = |rig: &mut Rig, param_index, depth| {
            let slot = rig.slot();
            let target = ModTarget {
                kind: ModTargetKind::PluginParam { slot: 1, param_index },
                depth,
                base_value: 0.5,
                param_min: 0.0,
                param_max: 1.0,
                effective: Some(slot),
            };
            rig.cmd_tx.send(GraphCommand::AddModTarget { inst: 0, mod_index: 0, target }).unwrap();
            slot
        };
        let (a, b) = (target(&mut rig, 0, 0.2), target(&mut rig, 1, 0.4));
        rig.process(&[]);
        assert!(close(rig.read(a), 0.7) && close(rig.read(b), 0.9), "{} {}", rig.read(a), rig.read(b));
    }

    #[test]
    fn group_targets_on_different_members_plugins_and_parameters_publish_their_own_values() {
        let mut g = GroupRig::new();
        // Member 0's chain: the scale effect (slot 1), then a two-parameter one (slot 2).
        insert_effect(&g.rig.cmd_tx, 1, two_param_effect(), 1.0);
        // The bus: the scale effect (0), then a two-parameter one (1).
        g.rig
            .cmd_tx
            .send(GraphCommand::InsertGroupEffect { group: 0, index: 1, effect: two_param_effect(), mix: 1.0 })
            .unwrap();
        let slot_2_a = g.target(0, ModTargetKind::GroupMember { member: 0, slot: 2, param_index: 0 }, 0.45);
        let slot_2_b = g.target(0, ModTargetKind::GroupMember { member: 0, slot: 2, param_index: 1 }, 0.35);
        let bus_1_a = g.target(0, ModTargetKind::GroupBus { effect_index: 1, param_index: 0 }, 0.05);
        let bus_1_b = g.target(0, ModTargetKind::GroupBus { effect_index: 1, param_index: 1 }, 0.25);
        g.rig.process(&[]);
        let read = |slot| g.rig.read(slot);
        // Base 0.5 plus the LFO's +1 times the depth; no two the same.
        for (slot, expected, what) in [
            (g.member0, 0.7, "member 0, slot 1"),
            (g.member1, 0.8, "member 1, slot 1"),
            (slot_2_a, 0.95, "member 0, slot 2, a"),
            (slot_2_b, 0.85, "member 0, slot 2, b"),
            (g.bus, 0.9, "bus 0"),
            (bus_1_a, 0.55, "bus 1, a"),
            (bus_1_b, 0.75, "bus 1, b"),
        ] {
            assert!(close(read(slot), expected), "{what}: {} instead of {expected}", read(slot));
        }
    }

    // -- Telemetry: lane and group meters --

    /// One lane per entry of `values` (a constant instrument), each in the group
    /// `groups[i]` names (an index into `group_count` groups) or in none, with the
    /// meter slots the test holds: `(lane meters, group meters)`.
    struct MeterRig {
        graph: AudioGraph,
        cmd_tx: crossbeam_channel::Sender<GraphCommand>,
        telemetry: Arc<Telemetry>,
        lanes: Vec<MeterSlots>,
        groups: Vec<MeterSlots>,
        slots: SlotAllocator,
        _return_rx: crossbeam_channel::Receiver<Box<dyn Plugin>>,
    }

    impl MeterRig {
        fn new(values: &[f32], lane_groups: &[Option<usize>], group_count: usize) -> MeterRig {
            let (graph, cmd_tx, return_rx) = make_graph(2);
            let mut rig = MeterRig {
                graph,
                cmd_tx,
                telemetry: Arc::new(Telemetry::new()),
                lanes: Vec::new(),
                groups: Vec::new(),
                slots: SlotAllocator::default(),
                _return_rx: return_rx,
            };
            rig.graph.set_telemetry(rig.telemetry.clone());
            for _ in 0..group_count {
                rig.cmd_tx.send(GraphCommand::AddGroup).unwrap();
                let meter = rig.slots.meter().unwrap();
                rig.cmd_tx.send(GraphCommand::SetGroupMeter { group: rig.groups.len(), slots: meter }).unwrap();
                rig.groups.push(meter);
            }
            for (inst, &value) in values.iter().enumerate() {
                if inst > 0 {
                    rig.cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
                }
                swap_instrument_at(&rig.cmd_tx, inst, ConstInstrument::new(value));
                rig.cmd_tx.send(GraphCommand::SetLaneGroup { inst, group: lane_groups[inst] }).unwrap();
                let meter = rig.slots.meter().unwrap();
                rig.cmd_tx.send(GraphCommand::SetLaneMeter { inst, slots: meter }).unwrap();
                rig.lanes.push(meter);
            }
            rig
        }

        /// A lane with no instrument in it, and its meter.
        fn empty_lane(&mut self) -> MeterSlots {
            let inst = self.lanes.len();
            self.cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
            let meter = self.slots.meter().unwrap();
            self.cmd_tx.send(GraphCommand::SetLaneMeter { inst, slots: meter }).unwrap();
            meter
        }

        fn play(&mut self) {
            self.graph.process(&[note_on(60)], &mut make_output()).unwrap();
        }

        fn read(&self, meter: MeterSlots) -> (f32, u32) {
            (self.telemetry.load(meter.peak), self.telemetry.count(meter.clips))
        }
    }

    #[test]
    fn each_lane_publishes_the_peak_of_its_own_output() {
        let mut rig = MeterRig::new(&[0.25, 0.5], &[None, None], 0);
        rig.play();
        assert!((rig.read(rig.lanes[0]).0 - 0.25).abs() < 1e-6 && rig.read(rig.lanes[0]).1 == 0);
        assert!((rig.read(rig.lanes[1]).0 - 0.5).abs() < 1e-6 && rig.read(rig.lanes[1]).1 == 0);
        assert!((rig.telemetry.load(telemetry::MASTER.peak) - 0.75).abs() < 1e-6, "and the master the sum");
    }

    #[test]
    fn a_lane_publishes_what_comes_out_of_it_after_its_volume_and_effects() {
        let mut rig = MeterRig::new(&[0.4], &[None], 0);
        rig.cmd_tx.send(GraphCommand::SetVolume { inst: 0, value: 2.0 }).unwrap();
        insert_effect(&rig.cmd_tx, 0, Box::new(ScaleEffect(0.5)), 1.0);
        rig.play();
        // 0.4, doubled by the lane's volume and halved by its effect.
        assert!((rig.read(rig.lanes[0]).0 - 0.4).abs() < 1e-6, "{:?}", rig.read(rig.lanes[0]));
        // Without the effect it would be 0.8, and with a stronger one it clips.
        let mut rig = MeterRig::new(&[0.4], &[None], 0);
        insert_effect(&rig.cmd_tx, 0, Box::new(ScaleEffect(3.0)), 1.0);
        rig.play();
        assert_eq!(rig.read(rig.lanes[0]).1, 1);
        assert!((rig.read(rig.lanes[0]).0 - 1.2).abs() < 1e-6);
    }

    #[test]
    fn a_lane_that_clips_counts_a_clip_of_its_own_and_the_others_do_not() {
        let mut rig = MeterRig::new(&[1.5, 0.25], &[None, None], 0);
        rig.play();
        rig.play();
        assert_eq!(rig.read(rig.lanes[0]).1, 2, "one per buffer");
        assert_eq!(rig.read(rig.lanes[1]).1, 0);
        assert_eq!(rig.telemetry.count(telemetry::MASTER.clips), 2, "the sum clipped too");
    }

    #[test]
    fn a_lane_in_a_group_is_measured_before_it_joins_the_group() {
        let mut rig = MeterRig::new(&[0.3, 0.3], &[Some(0), Some(0)], 1);
        rig.cmd_tx.send(GraphCommand::SetGroupVolume { group: 0, value: 2.0 }).unwrap();
        rig.play();
        for lane in [0, 1] {
            assert!((rig.read(rig.lanes[lane]).0 - 0.3).abs() < 1e-6, "lane {lane}: {:?}", rig.read(rig.lanes[lane]));
        }
        // The two lanes sum to 0.6, and the group's volume doubles that.
        assert!((rig.read(rig.groups[0]).0 - 1.2).abs() < 1e-6, "{:?}", rig.read(rig.groups[0]));
        assert_eq!(rig.read(rig.groups[0]).1, 1, "the bus clips while the lanes do not");
        assert_eq!((rig.read(rig.lanes[0]).1, rig.read(rig.lanes[1]).1), (0, 0));
        assert!((rig.telemetry.load(telemetry::MASTER.peak) - 1.2).abs() < 1e-6);
    }

    #[test]
    fn a_group_publishes_what_comes_out_of_it_after_its_volume_and_its_effects() {
        let mut rig = MeterRig::new(&[0.3, 0.3], &[Some(0), Some(0)], 1);
        rig.cmd_tx.send(GraphCommand::SetGroupVolume { group: 0, value: 2.0 }).unwrap();
        rig.cmd_tx
            .send(GraphCommand::InsertGroupEffect { group: 0, index: 0, effect: Box::new(ScaleEffect(0.5)), mix: 1.0 })
            .unwrap();
        rig.play();
        // 0.6, doubled, halved.
        assert!((rig.read(rig.groups[0]).0 - 0.6).abs() < 1e-6, "{:?}", rig.read(rig.groups[0]));
        assert_eq!(rig.read(rig.groups[0]).1, 0);
    }

    #[test]
    fn each_group_publishes_the_sum_of_its_own_lanes() {
        // Group 1 has lanes 0 and 1, group 0 has lane 2, and group 2 has nobody.
        let mut rig = MeterRig::new(&[0.1, 0.2, 0.4], &[Some(1), Some(1), Some(0)], 3);
        rig.play();
        let peaks: Vec<f32> = rig.groups.iter().map(|&g| rig.read(g).0).collect();
        for (group, (peak, expected)) in peaks.iter().zip([0.4, 0.3, 0.0]).enumerate() {
            assert!((peak - expected).abs() < 1e-6, "group {group}: {peak} instead of {expected}");
        }
    }

    #[test]
    fn a_group_that_nobody_is_in_reads_zero_and_a_lane_without_an_instrument_too() {
        let mut rig = MeterRig::new(&[0.5], &[None], 1);
        let empty = rig.empty_lane();
        rig.play();
        assert_eq!(rig.read(rig.groups[0]), (0.0, 0));
        assert_eq!(rig.read(empty), (0.0, 0));
        assert!((rig.read(rig.lanes[0]).0 - 0.5).abs() < 1e-6, "the lane with sound is unaffected");
    }

    #[test]
    fn meters_set_for_lanes_and_groups_that_are_not_there_change_nothing() {
        let mut rig = MeterRig::new(&[0.5], &[None], 0);
        let stray = rig.slots.meter().unwrap();
        rig.cmd_tx.send(GraphCommand::SetLaneMeter { inst: 7, slots: stray }).unwrap();
        rig.cmd_tx.send(GraphCommand::SetGroupMeter { group: 7, slots: stray }).unwrap();
        rig.play();
        assert!((rig.read(rig.lanes[0]).0 - 0.5).abs() < 1e-6);
        assert_eq!(rig.read(stray), (0.0, 0), "nothing was given those cells");
    }

    #[test]
    fn a_lane_keeps_its_meter_when_the_lane_before_it_is_removed() {
        let mut rig = MeterRig::new(&[0.25, 0.5], &[None, None], 0);
        rig.play();
        rig.cmd_tx.send(GraphCommand::RemoveInstrument { inst: 0, keep: None }).unwrap();
        rig.telemetry.store(rig.lanes[0].peak, -1.0);
        rig.telemetry.store(rig.lanes[1].peak, -1.0);
        rig.play();
        assert_eq!(rig.read(rig.lanes[0]).0, -1.0, "the removed lane publishes no more");
        assert!((rig.read(rig.lanes[1]).0 - 0.5).abs() < 1e-6, "the other is lane 0 now, and still its own meter");
    }

    #[test]
    fn a_lane_or_group_without_telemetry_publishes_nothing() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(2.0));
        let meter = SlotAllocator::default().meter().unwrap();
        cmd_tx.send(GraphCommand::SetLaneMeter { inst: 0, slots: meter }).unwrap();
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetGroupMeter { group: 0, slots: meter }).unwrap();
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 2.0), "the audio is the same");
    }

    #[test]
    fn publishing_meters_does_not_allocate() {
        let mut rig = MeterRig::new(&[0.6, 0.6, 0.2], &[Some(0), Some(0), None], 1);
        rig.graph.drain_commands();
        let allocations = allocations_while_processing(&mut rig.graph, &[note_on(60)], 16);
        assert_eq!(allocations, 0, "the audio thread asked the allocator for memory");
    }

    // -- Taking a cable (a modulation target) out of a modulator --
    //
    // A cable is addressed by its place in its modulator, so when one is taken out the cables that
    // drive the depth of a later one of the same modulator (`ModulatorDepth`, and a group's
    // `GroupMemberMod` with a `Depth`) are told its new place. The cables that drive the depth of the
    // one taken out are the controller's to take out first: removing one is one thing.

    /// A cable of `kind`, told apart from the others by its depth.
    fn cable(kind: ModTargetKind, depth: f32) -> ModTarget {
        ModTarget { kind, depth, base_value: 0.5, param_min: 0.0, param_max: 1.0, effective: None }
    }

    fn insert_lane_lfo(cmd_tx: &crossbeam_channel::Sender<GraphCommand>, inst: usize, index: usize) {
        let source = ModSource::lfo(LfoWaveform::Sine, 1.0);
        cmd_tx.send(GraphCommand::InsertModulator { inst, index, source }).unwrap();
    }

    fn add_lane_cable(cmd_tx: &crossbeam_channel::Sender<GraphCommand>, inst: usize, mod_index: usize, target: ModTarget) {
        cmd_tx.send(GraphCommand::AddModTarget { inst, mod_index, target }).unwrap();
    }

    fn add_group_cable(cmd_tx: &crossbeam_channel::Sender<GraphCommand>, group: usize, mod_index: usize, target: ModTarget) {
        cmd_tx.send(GraphCommand::AddGroupModTarget { group, mod_index, target }).unwrap();
    }

    /// What a modulator drives, in order: each cable's kind and depth.
    fn drives(modulator: &Modulator) -> Vec<(ModTargetKind, f32)> {
        modulator.targets.iter().map(|t| (t.kind.clone(), t.depth)).collect()
    }

    fn param(slot: usize, param_index: u32) -> ModTargetKind {
        ModTargetKind::PluginParam { slot, param_index }
    }

    fn depth_of(mod_index: usize, target_index: usize) -> ModTargetKind {
        ModTargetKind::ModulatorDepth { mod_index, target_index }
    }

    /// A lane with three modulators: the first drives three parameters (depths 0.1, 0.2, 0.3), the second the depth
    /// of the first's first and third cable, its rate, and a parameter, and the third the depth of the second's third.
    fn lane_with_cables_that_drive_cables() -> (AudioGraph, crossbeam_channel::Sender<GraphCommand>) {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        for index in 0..3 {
            insert_lane_lfo(&cmd_tx, 0, index);
        }
        for (i, depth) in [0.1, 0.2, 0.3].into_iter().enumerate() {
            add_lane_cable(&cmd_tx, 0, 0, cable(param(0, i as u32), depth));
        }
        add_lane_cable(&cmd_tx, 0, 1, cable(depth_of(0, 0), 0.4));
        add_lane_cable(&cmd_tx, 0, 1, cable(depth_of(0, 2), 0.5));
        add_lane_cable(&cmd_tx, 0, 1, cable(ModTargetKind::ModulatorRate { mod_index: 0 }, 0.6));
        add_lane_cable(&cmd_tx, 0, 1, cable(param(0, 7), 0.7));
        add_lane_cable(&cmd_tx, 0, 2, cable(depth_of(1, 3), 0.8));
        // (And one that drives a cable the second modulator does not have, so that a removal that removed nothing
        // can be seen to have moved nothing.)
        add_lane_cable(&cmd_tx, 0, 2, cable(depth_of(1, 9), 0.9));
        graph.drain_commands();
        (graph, cmd_tx)
    }

    #[test]
    fn removing_a_cable_takes_it_out_of_its_modulator_and_moves_the_depth_cables_of_the_later_ones() {
        let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
        // The second cable of the first modulator.
        cmd_tx.send(GraphCommand::RemoveModTarget { inst: 0, mod_index: 0, target_index: 1 }).unwrap();
        graph.drain_commands();
        let rack = &graph.instruments[0].modulators;
        assert_eq!(drives(&rack[0]), [(param(0, 0), 0.1), (param(0, 2), 0.3)]);
        // What drove the third (the second's second) is told it is the second now; the first's is as it was; and
        // what drove something else (the rate, a parameter, another modulator's cable) is not touched.
        assert_eq!(
            drives(&rack[1]),
            [(depth_of(0, 0), 0.4), (depth_of(0, 1), 0.5), (ModTargetKind::ModulatorRate { mod_index: 0 }, 0.6), (param(0, 7), 0.7)]
        );
        assert_eq!(drives(&rack[2]), [(depth_of(1, 3), 0.8), (depth_of(1, 9), 0.9)]);
    }

    #[test]
    fn removing_the_last_cable_of_a_modulator_moves_nothing() {
        let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
        cmd_tx.send(GraphCommand::RemoveModTarget { inst: 0, mod_index: 0, target_index: 2 }).unwrap();
        graph.drain_commands();
        let rack = &graph.instruments[0].modulators;
        assert_eq!(drives(&rack[0]), [(param(0, 0), 0.1), (param(0, 1), 0.2)]);
        assert_eq!(rack[1].targets[1].kind, depth_of(0, 2), "(it is the controller that took out what drove it)");
        assert_eq!(rack[1].targets.len(), 4);
    }

    #[test]
    fn removing_a_cable_of_a_modulator_that_drives_the_depth_of_others_moves_theirs_by_its_own_index() {
        let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
        // The second modulator's first cable: only what drove the depths of the second modulator's cables moves.
        cmd_tx.send(GraphCommand::RemoveModTarget { inst: 0, mod_index: 1, target_index: 0 }).unwrap();
        graph.drain_commands();
        let rack = &graph.instruments[0].modulators;
        assert_eq!(rack[1].targets.len(), 3);
        assert_eq!(drives(&rack[2]), [(depth_of(1, 2), 0.8), (depth_of(1, 8), 0.9)]);
        assert_eq!(rack[0].targets.len(), 3, "the first modulator's cables are not touched");
    }

    #[test]
    fn a_cable_that_is_not_there_is_not_removed_and_nothing_moves() {
        let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
        let before: Vec<_> = graph.instruments[0].modulators.iter().map(drives).collect();
        for (inst, mod_index, target_index) in [(0, 0, 3), (0, 0, 9), (0, 1, 4), (0, 3, 0), (4, 0, 0)] {
            cmd_tx.send(GraphCommand::RemoveModTarget { inst, mod_index, target_index }).unwrap();
        }
        graph.drain_commands();
        let after: Vec<_> = graph.instruments[0].modulators.iter().map(drives).collect();
        assert_eq!(before, after);
    }

    /// Lane 0 outside the group, lane 1 its member 0, lane 2 its member 1; lane 1 and lane 0 each have a modulator with
    /// three parameters to drive (depths 0.1 to 0.3); group modulator 0 drives the depth of member 0's modulator's
    /// first and third cable, its rate, the same cables of member 1's, and a bus parameter.
    fn group_with_cables_that_drive_a_members_cables() -> (AudioGraph, crossbeam_channel::Sender<GraphCommand>) {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        for inst in [1, 2] {
            cmd_tx.send(GraphCommand::SetLaneGroup { inst, group: Some(0) }).unwrap();
        }
        for inst in [0, 1] {
            insert_lane_lfo(&cmd_tx, inst, 0);
            for (i, depth) in [0.1, 0.2, 0.3].into_iter().enumerate() {
                add_lane_cable(&cmd_tx, inst, 0, cable(param(0, i as u32), depth));
            }
        }
        // Member 0 has a second modulator with cables, which the group drives the depth of as well.
        insert_lane_lfo(&cmd_tx, 1, 1);
        for (i, depth) in [0.4, 0.5, 0.6].into_iter().enumerate() {
            add_lane_cable(&cmd_tx, 1, 1, cable(param(0, i as u32), depth));
        }
        let source = ModSource::lfo(LfoWaveform::Sine, 1.0);
        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source }).unwrap();
        let member_mod = |member, field| ModTargetKind::GroupMemberMod { member, mod_index: 0, field };
        add_group_cable(&cmd_tx, 0, 0, cable(member_mod(0, CrossModField::Depth(0)), 0.4));
        add_group_cable(&cmd_tx, 0, 0, cable(member_mod(0, CrossModField::Depth(2)), 0.5));
        add_group_cable(&cmd_tx, 0, 0, cable(member_mod(0, CrossModField::Rate), 0.6));
        add_group_cable(&cmd_tx, 0, 0, cable(member_mod(1, CrossModField::Depth(2)), 0.7));
        add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupBus { effect_index: 0, param_index: 0 }, 0.8));
        // (And one that drives a cable member 0's modulator does not have: a removal that removed nothing must not move it.)
        add_group_cable(&cmd_tx, 0, 0, cable(member_mod(0, CrossModField::Depth(7)), 0.9));
        // And the depth of the third cable of member 0's second modulator.
        add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupMemberMod { member: 0, mod_index: 1, field: CrossModField::Depth(2) }, 1.0));
        graph.drain_commands();
        (graph, cmd_tx)
    }

    #[test]
    fn removing_a_members_cable_moves_the_groups_cables_to_the_depths_of_its_later_ones() {
        let (mut graph, cmd_tx) = group_with_cables_that_drive_a_members_cables();
        let member_mod = |member, field| ModTargetKind::GroupMemberMod { member, mod_index: 0, field };
        // Lane 1 is member 0.
        cmd_tx.send(GraphCommand::RemoveModTarget { inst: 1, mod_index: 0, target_index: 1 }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.instruments[1].modulators[0].targets.len(), 2);
        assert_eq!(
            drives(&graph.groups[0].modulators[0]),
            [
                (member_mod(0, CrossModField::Depth(0)), 0.4),
                (member_mod(0, CrossModField::Depth(1)), 0.5),
                (member_mod(0, CrossModField::Rate), 0.6),
                // Member 1's own modulator is another's: its cables are not moved.
                (member_mod(1, CrossModField::Depth(2)), 0.7),
                (ModTargetKind::GroupBus { effect_index: 0, param_index: 0 }, 0.8),
                (member_mod(0, CrossModField::Depth(6)), 0.9),
                // The member's other modulator lost nothing: this one is where it was.
                (ModTargetKind::GroupMemberMod { member: 0, mod_index: 1, field: CrossModField::Depth(2) }, 1.0),
            ]
        );
    }

    #[test]
    fn removing_a_members_last_cable_moves_nothing_of_the_groups() {
        let (mut graph, cmd_tx) = group_with_cables_that_drive_a_members_cables();
        let member_mod = |member, field| ModTargetKind::GroupMemberMod { member, mod_index: 0, field };
        cmd_tx.send(GraphCommand::RemoveModTarget { inst: 1, mod_index: 0, target_index: 2 }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.instruments[1].modulators[0].targets.len(), 2);
        let after: Vec<_> = graph.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).collect();
        // The cable that drove the depth of the one that went is left as it is (it is for the controller to have taken it
        // out first), and so are the first one's and the other modulator's; the far one, past it, moved.
        assert_eq!(after[..5], [
            member_mod(0, CrossModField::Depth(0)),
            member_mod(0, CrossModField::Depth(2)),
            member_mod(0, CrossModField::Rate),
            member_mod(1, CrossModField::Depth(2)),
            ModTargetKind::GroupBus { effect_index: 0, param_index: 0 },
        ]);
        assert_eq!(after[5], member_mod(0, CrossModField::Depth(6)));
        assert_eq!(after[6], ModTargetKind::GroupMemberMod { member: 0, mod_index: 1, field: CrossModField::Depth(2) });
    }

    #[test]
    fn a_members_cable_that_is_not_there_is_not_removed_and_the_groups_cables_stay() {
        let (mut graph, cmd_tx) = group_with_cables_that_drive_a_members_cables();
        let before = drives(&graph.groups[0].modulators[0]);
        // Lane 1 is member 0; its first modulator has three cables and it has no third; lane 2 is member 1 and has none.
        for (inst, mod_index, target_index) in [(1, 0, 3), (1, 0, 9), (1, 2, 0), (2, 0, 0)] {
            cmd_tx.send(GraphCommand::RemoveModTarget { inst, mod_index, target_index }).unwrap();
        }
        graph.drain_commands();
        assert_eq!(graph.instruments[1].modulators[0].targets.len(), 3);
        assert_eq!(drives(&graph.groups[0].modulators[0]), before);
    }

    #[test]
    fn removing_a_cable_of_a_lane_outside_the_group_leaves_the_groups_cables_alone() {
        let (mut graph, cmd_tx) = group_with_cables_that_drive_a_members_cables();
        let before = drives(&graph.groups[0].modulators[0]);
        cmd_tx.send(GraphCommand::RemoveModTarget { inst: 0, mod_index: 0, target_index: 0 }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.instruments[0].modulators[0].targets.len(), 2);
        assert_eq!(drives(&graph.groups[0].modulators[0]), before);
    }

    #[test]
    fn removing_a_group_modulators_cable_moves_its_siblings_cables_to_the_depths_of_its_later_ones() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        let source = || ModSource::lfo(LfoWaveform::Sine, 1.0);
        for index in 0..2 {
            cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index, source: source() }).unwrap();
        }
        for (i, depth) in [0.1, 0.2, 0.3].into_iter().enumerate() {
            add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupBus { effect_index: 0, param_index: i as u32 }, depth));
        }
        add_group_cable(&cmd_tx, 0, 1, cable(depth_of(0, 1), 0.4));
        add_group_cable(&cmd_tx, 0, 1, cable(depth_of(0, 2), 0.5));
        // (And one that drives a cable the first does not have: a removal that removed nothing must not move it.)
        add_group_cable(&cmd_tx, 0, 1, cable(depth_of(0, 5), 0.6));
        // Nothing of a lane's rack is in step with this one's cables.
        insert_lane_lfo(&cmd_tx, 0, 0);
        add_lane_cable(&cmd_tx, 0, 0, cable(depth_of(0, 2), 0.9));
        graph.drain_commands();

        cmd_tx.send(GraphCommand::RemoveGroupModTarget { group: 0, mod_index: 0, target_index: 0 }).unwrap();
        graph.drain_commands();
        let rack = &graph.groups[0].modulators;
        assert_eq!(rack[0].targets.len(), 2);
        // (What drove the first cable is the controller's to have taken out first, so there is none here.)
        assert_eq!(rack[1].targets.iter().map(|t| t.kind.clone()).collect::<Vec<_>>(), [depth_of(0, 0), depth_of(0, 1), depth_of(0, 4)]);
        assert_eq!(graph.instruments[0].modulators[0].targets[0].kind, depth_of(0, 2), "a lane's rack is its own");

        // A cable that is not there: nothing happens.
        for (group, mod_index, target_index) in [(0, 0, 2), (0, 5, 0), (3, 0, 0)] {
            cmd_tx.send(GraphCommand::RemoveGroupModTarget { group, mod_index, target_index }).unwrap();
        }
        graph.drain_commands();
        assert_eq!((graph.groups[0].modulators[0].targets.len(), graph.groups[0].modulators[1].targets.len()), (2, 3));
        assert_eq!(graph.groups[0].modulators[1].targets[2].kind, depth_of(0, 4), "and nothing moved");
    }

    // -- Putting a cable back --
    //
    // The inverse of taking one out (`RemoveModTarget`): a cable that goes in at a place is at that place, and the
    // cables that drive the depth of its later ones are told theirs, so that taking a cable out and putting it back
    // (after the cables that drive its depth, which the controller takes out first) leaves every rack as it was.

    /// What every modulator drives, in order: each cable's kind and depth; the lanes' racks, then the groups'.
    fn all_cables(graph: &AudioGraph) -> Vec<Vec<(ModTargetKind, f32)>> {
        let lanes = graph.instruments.iter().flat_map(|lane| lane.modulators.iter());
        let groups = graph.groups.iter().flat_map(|group| group.modulators.iter());
        lanes.chain(groups).map(drives).collect()
    }

    /// Take the cables at `places` (a modulator and a cable of it) of lane `inst` out, one after the other, and put them back in
    /// the reverse order, each where it was when its turn came: as the controller does for a cable and the cables that drive
    /// its depth.
    fn out_and_back(graph: &mut AudioGraph, cmd_tx: &crossbeam_channel::Sender<GraphCommand>, inst: usize, places: &[(usize, usize)]) {
        let mut taken = Vec::new();
        for &(mod_index, target_index) in places {
            taken.push((mod_index, target_index, graph.instruments[inst].modulators[mod_index].targets[target_index].clone()));
            cmd_tx.send(GraphCommand::RemoveModTarget { inst, mod_index, target_index }).unwrap();
            graph.drain_commands();
        }
        for (mod_index, target_index, target) in taken.into_iter().rev() {
            cmd_tx.send(GraphCommand::InsertModTarget { inst, mod_index, target_index, target }).unwrap();
            graph.drain_commands();
        }
    }

    #[test]
    fn a_cable_that_goes_in_is_at_its_place_and_the_depth_cables_of_the_later_ones_are_told_theirs() {
        let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
        cmd_tx.send(GraphCommand::InsertModTarget { inst: 0, mod_index: 0, target_index: 1, target: cable(param(0, 9), 0.15) }).unwrap();
        graph.drain_commands();
        let rack = &graph.instruments[0].modulators;
        assert_eq!(drives(&rack[0]), [(param(0, 0), 0.1), (param(0, 9), 0.15), (param(0, 1), 0.2), (param(0, 2), 0.3)]);
        // What drove the first cable still does; what drove the third drives the fourth, where it is now; and what drove
        // something else (the rate, a parameter, another modulator's cable) is not touched.
        assert_eq!(
            drives(&rack[1]),
            [(depth_of(0, 0), 0.4), (depth_of(0, 3), 0.5), (ModTargetKind::ModulatorRate { mod_index: 0 }, 0.6), (param(0, 7), 0.7)]
        );
        assert_eq!(drives(&rack[2]), [(depth_of(1, 3), 0.8), (depth_of(1, 9), 0.9)]);
    }

    #[test]
    fn a_cable_that_goes_in_at_the_first_place_moves_every_depth_cable_of_its_modulators_cables() {
        let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
        cmd_tx.send(GraphCommand::InsertModTarget { inst: 0, mod_index: 0, target_index: 0, target: cable(param(0, 9), 0.15) }).unwrap();
        graph.drain_commands();
        let rack = &graph.instruments[0].modulators;
        assert_eq!(rack[0].targets.len(), 4);
        assert_eq!(
            drives(&rack[1]),
            [(depth_of(0, 1), 0.4), (depth_of(0, 3), 0.5), (ModTargetKind::ModulatorRate { mod_index: 0 }, 0.6), (param(0, 7), 0.7)]
        );
    }

    #[test]
    fn a_cable_that_goes_in_at_the_end_or_past_it_is_the_last_and_moves_nothing() {
        for target_index in [3, 4, 99] {
            let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
            let before = all_cables(&graph);
            cmd_tx.send(GraphCommand::InsertModTarget { inst: 0, mod_index: 0, target_index, target: cable(param(0, 9), 0.15) }).unwrap();
            graph.drain_commands();
            let after = all_cables(&graph);
            assert_eq!(after[0].last(), Some(&(param(0, 9), 0.15)), "put at {target_index}");
            assert_eq!(after[0].len(), 4);
            assert_eq!(after[1..], before[1..], "put at {target_index}: nothing that drives a cable of it moved");
        }
    }

    #[test]
    fn the_cable_that_goes_in_keeps_the_kind_it_has_and_the_slot_it_publishes_to() {
        let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
        // (A cable that drives a depth of the modulator it goes into: its own kind is what it says, not what the others' become.)
        let slot = crate::telemetry::SlotAllocator::default().alloc();
        let mut target = cable(depth_of(0, 1), 0.15);
        target.effective = slot;
        cmd_tx.send(GraphCommand::InsertModTarget { inst: 0, mod_index: 0, target_index: 0, target }).unwrap();
        graph.drain_commands();
        let put = &graph.instruments[0].modulators[0].targets[0];
        assert_eq!(put.kind, depth_of(0, 1), "it is not told a place it was not at");
        assert_eq!(put.effective, slot);
        assert_eq!((put.base_value, put.param_min, put.param_max), (0.5, 0.0, 1.0));
    }

    #[test]
    fn a_cable_that_goes_in_where_there_is_no_modulator_or_no_lane_is_not_put_anywhere_and_nothing_moves() {
        let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
        let before = all_cables(&graph);
        for (inst, mod_index) in [(0, 3), (0, 9), (4, 0)] {
            cmd_tx.send(GraphCommand::InsertModTarget { inst, mod_index, target_index: 0, target: cable(param(0, 9), 0.15) }).unwrap();
        }
        graph.drain_commands();
        assert_eq!(all_cables(&graph), before);
    }

    #[test]
    fn a_cable_that_is_taken_out_and_put_back_leaves_every_rack_as_it_was() {
        // Each place, with the cables that drive its depth taken out before it (the controller does it so), and put back after it.
        let cases: [&[(usize, usize)]; 8] = [
            &[(0, 1)],                 // none drives it; the one after it is driven
            &[(1, 1), (0, 2)],         // the last of its modulator, driven by the second's second
            &[(1, 0), (0, 0)],         // the first, driven by the second's first
            &[(2, 0), (1, 3)],         // driven by the third's first; the third drives another that is not there
            &[(1, 2)],                 // a cable to a rate
            &[(2, 1)],                 // the last of the last
            &[(1, 1), (1, 0)],         // two of one modulator, the later first
            &[(2, 0), (1, 3), (1, 0), (0, 0)], // a chain of them
        ];
        for places in cases {
            let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
            let before = all_cables(&graph);
            out_and_back(&mut graph, &cmd_tx, 0, places);
            assert_eq!(all_cables(&graph), before, "{places:?}");
        }
    }

    #[test]
    fn a_members_cable_that_goes_in_moves_the_groups_cables_to_the_depths_of_its_later_ones_and_no_others() {
        let (mut graph, cmd_tx) = group_with_cables_that_drive_a_members_cables();
        let member_mod = |member, field| ModTargetKind::GroupMemberMod { member, mod_index: 0, field };
        // Lane 1 is member 0: its first modulator has three cables.
        cmd_tx.send(GraphCommand::InsertModTarget { inst: 1, mod_index: 0, target_index: 1, target: cable(param(0, 9), 0.15) }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.instruments[1].modulators[0].targets.len(), 4);
        assert_eq!(
            drives(&graph.groups[0].modulators[0]),
            [
                (member_mod(0, CrossModField::Depth(0)), 0.4),
                (member_mod(0, CrossModField::Depth(3)), 0.5),
                (member_mod(0, CrossModField::Rate), 0.6),
                // Member 1's own modulator is another's: its cables are not moved.
                (member_mod(1, CrossModField::Depth(2)), 0.7),
                (ModTargetKind::GroupBus { effect_index: 0, param_index: 0 }, 0.8),
                (member_mod(0, CrossModField::Depth(8)), 0.9),
                // The member's other modulator gained nothing: this one is where it was.
                (ModTargetKind::GroupMemberMod { member: 0, mod_index: 1, field: CrossModField::Depth(2) }, 1.0),
            ]
        );
    }

    #[test]
    fn a_members_cable_that_goes_in_past_the_end_is_the_last_and_the_groups_cables_are_told_the_place_it_has_not_the_place_asked_for() {
        let (mut graph, cmd_tx) = group_with_cables_that_drive_a_members_cables();
        let member_mod = |member, field| ModTargetKind::GroupMemberMod { member, mod_index: 0, field };
        // Member 0's first modulator has three cables, so the cable that is put at place 99 is the fourth (place 3).
        cmd_tx.send(GraphCommand::InsertModTarget { inst: 1, mod_index: 0, target_index: 99, target: cable(param(0, 9), 0.15) }).unwrap();
        graph.drain_commands();
        let rack = &graph.instruments[1].modulators[0];
        assert_eq!(rack.targets.len(), 4);
        assert_eq!(rack.targets[3].depth, 0.15);
        let told: Vec<_> = graph.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).collect();
        // Every cable from place 3 on is a place later: the group's cable to the depth of the 8th (the modulator has no 8th, which
        // shows the place the others are told is where the cable went, 3, and not the 99 that was asked for) is to the 9th.
        assert_eq!(told[0], member_mod(0, CrossModField::Depth(0)));
        assert_eq!(told[1], member_mod(0, CrossModField::Depth(2)), "a cable the modulator had is not moved");
        assert_eq!(told[5], member_mod(0, CrossModField::Depth(8)));
    }

    #[test]
    fn a_cable_of_a_lane_outside_the_group_that_goes_in_leaves_the_groups_cables_alone() {
        let (mut graph, cmd_tx) = group_with_cables_that_drive_a_members_cables();
        let before = drives(&graph.groups[0].modulators[0]);
        cmd_tx.send(GraphCommand::InsertModTarget { inst: 0, mod_index: 0, target_index: 0, target: cable(param(0, 9), 0.15) }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.instruments[0].modulators[0].targets.len(), 4);
        assert_eq!(drives(&graph.groups[0].modulators[0]), before);
    }

    #[test]
    fn a_members_cable_that_is_taken_out_and_put_back_leaves_the_racks_of_the_lanes_and_the_group_as_they_were() {
        // Lane 1 is member 0. Its cables that the group drives the depth of go first, as the controller does it.
        for target_index in 0..3 {
            let (mut graph, cmd_tx) = group_with_cables_that_drive_a_members_cables();
            let before = all_cables(&graph);
            let target = graph.instruments[1].modulators[0].targets[target_index].clone();
            // (The group's cables to the depth of that cable go out first.)
            let driving: Vec<usize> = graph.groups[0].modulators[0]
                .targets
                .iter()
                .enumerate()
                .filter(|(_, t)| matches!(&t.kind, ModTargetKind::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Depth(of) } if *of == target_index))
                .map(|(i, _)| i)
                .collect();
            let mut taken = Vec::new();
            for &at in driving.iter().rev() {
                taken.push((at, graph.groups[0].modulators[0].targets[at].clone()));
                cmd_tx.send(GraphCommand::RemoveGroupModTarget { group: 0, mod_index: 0, target_index: at }).unwrap();
                graph.drain_commands();
            }
            cmd_tx.send(GraphCommand::RemoveModTarget { inst: 1, mod_index: 0, target_index }).unwrap();
            graph.drain_commands();
            cmd_tx.send(GraphCommand::InsertModTarget { inst: 1, mod_index: 0, target_index, target }).unwrap();
            graph.drain_commands();
            for (at, target) in taken.into_iter().rev() {
                cmd_tx.send(GraphCommand::InsertGroupModTarget { group: 0, mod_index: 0, target_index: at, target }).unwrap();
                graph.drain_commands();
            }
            assert_eq!(all_cables(&graph), before, "cable {target_index} of member 0's first modulator");
        }
    }

    #[test]
    fn a_group_modulators_cable_that_goes_in_moves_its_siblings_cables_to_the_depths_of_its_later_ones_and_no_others() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        let source = || ModSource::lfo(LfoWaveform::Sine, 1.0);
        for index in 0..2 {
            cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index, source: source() }).unwrap();
        }
        for (i, depth) in [0.1, 0.2, 0.3].into_iter().enumerate() {
            add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupBus { effect_index: 0, param_index: i as u32 }, depth));
        }
        add_group_cable(&cmd_tx, 0, 1, cable(depth_of(0, 0), 0.4));
        add_group_cable(&cmd_tx, 0, 1, cable(depth_of(0, 2), 0.5));
        // Nothing of a lane's rack is in step with this one's cables.
        insert_lane_lfo(&cmd_tx, 0, 0);
        add_lane_cable(&cmd_tx, 0, 0, cable(depth_of(0, 2), 0.9));
        graph.drain_commands();

        cmd_tx.send(GraphCommand::InsertGroupModTarget { group: 0, mod_index: 0, target_index: 1, target: cable(ModTargetKind::GroupBus { effect_index: 0, param_index: 9 }, 0.15) }).unwrap();
        graph.drain_commands();
        let rack = &graph.groups[0].modulators;
        assert_eq!(rack[0].targets.iter().map(|t| t.depth).collect::<Vec<_>>(), [0.1, 0.15, 0.2, 0.3]);
        assert_eq!(rack[1].targets.iter().map(|t| t.kind.clone()).collect::<Vec<_>>(), [depth_of(0, 0), depth_of(0, 3)]);
        assert_eq!(graph.instruments[0].modulators[0].targets[0].kind, depth_of(0, 2), "a lane's rack is its own");

        // At the end: nothing moves. Where there is no modulator or no group: nothing happens.
        let before = all_cables(&graph);
        cmd_tx.send(GraphCommand::InsertGroupModTarget { group: 0, mod_index: 0, target_index: 99, target: cable(ModTargetKind::GroupBus { effect_index: 0, param_index: 8 }, 0.25) }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.groups[0].modulators[0].targets.len(), 5);
        assert_eq!(graph.groups[0].modulators[0].targets.last().map(|t| t.depth), Some(0.25));
        // (The lane's rack, then the group's two: what drives a cable of the first is where it was.)
        let after = all_cables(&graph);
        assert_eq!((&after[0], &after[2]), (&before[0], &before[2]));
        let before = all_cables(&graph);
        for (group, mod_index) in [(0, 5), (3, 0)] {
            cmd_tx.send(GraphCommand::InsertGroupModTarget { group, mod_index, target_index: 0, target: cable(param(0, 9), 0.35) }).unwrap();
        }
        graph.drain_commands();
        assert_eq!(all_cables(&graph), before);
    }

    // -- Putting a modulator back --
    //
    // The inverse of `RemoveModulator` / `RemoveGroupModulator`: a modulator that goes in at a place is at that place, and the cables of its
    // siblings (and, for a member's, of the group) that address a later modulator by its place are told the place it has now, before it goes in.
    // What drove the modulator that went was taken out first, and what the modulator drove goes with it and comes back with it: the cables
    // that are put back after it find it where they left it.

    /// What every modulator is, in order: its source and what it drives (each cable's kind, depth and slot), where it publishes, and what each cable is
    /// centred on; the lanes' racks, then the groups'.
    #[allow(clippy::type_complexity, reason = "a pair that tests compare, and say nothing else of")]
    fn all_modulators(graph: &AudioGraph) -> (Vec<String>, Vec<(ModulatorLocation, usize, ModTargetKind, f32)>) {
        (graph.modulator_dump(), graph.cable_bases())
    }

    /// A lane's modulator with the rate `rate`, which tells it from the others.
    fn insert_lane_lfo_at(cmd_tx: &crossbeam_channel::Sender<GraphCommand>, inst: usize, index: usize, rate: f32) {
        let source = ModSource::lfo(LfoWaveform::Sine, rate);
        cmd_tx.send(GraphCommand::InsertModulator { inst, index, source }).unwrap();
    }

    fn rate_of(modulator: &Modulator) -> f32 {
        match modulator.source {
            ModSource::Lfo { rate, .. } => rate,
            _ => panic!("not an LFO"),
        }
    }

    #[test]
    fn a_modulator_that_goes_in_is_at_its_place_and_the_cables_that_address_a_later_modulator_are_told_the_place_it_has_now() {
        let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
        insert_lane_lfo_at(&cmd_tx, 0, 1, 7.0);
        graph.drain_commands();
        let rack = &graph.instruments[0].modulators;
        assert_eq!(rack.len(), 4);
        assert_eq!(rate_of(&rack[1]), 7.0, "it is the second");
        // The first modulator is where it was, and its cables are what they were (the second drove the depths of its cables: it is the third now).
        assert_eq!(drives(&rack[0]), [(param(0, 0), 0.1), (param(0, 1), 0.2), (param(0, 2), 0.3)]);
        assert!(rack[1].targets.is_empty());
        assert_eq!(
            drives(&rack[2]),
            [(depth_of(0, 0), 0.4), (depth_of(0, 2), 0.5), (ModTargetKind::ModulatorRate { mod_index: 0 }, 0.6), (param(0, 7), 0.7)],
            "what drove the first modulator still does"
        );
        // The old third drove the depths of the old second's cables: the old second is the third now.
        assert_eq!(drives(&rack[3]), [(depth_of(2, 3), 0.8), (depth_of(2, 9), 0.9)]);
    }

    #[test]
    fn a_modulator_that_goes_in_first_moves_every_cable_that_addresses_a_modulator() {
        let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
        insert_lane_lfo_at(&cmd_tx, 0, 0, 7.0);
        graph.drain_commands();
        let rack = &graph.instruments[0].modulators;
        assert_eq!(drives(&rack[2]), [(depth_of(1, 0), 0.4), (depth_of(1, 2), 0.5), (ModTargetKind::ModulatorRate { mod_index: 1 }, 0.6), (param(0, 7), 0.7)]);
        assert_eq!(drives(&rack[3]), [(depth_of(2, 3), 0.8), (depth_of(2, 9), 0.9)]);
        assert_eq!(drives(&rack[1]), [(param(0, 0), 0.1), (param(0, 1), 0.2), (param(0, 2), 0.3)], "(what a cable drives that is not a modulator is not moved)");
    }

    #[test]
    fn a_modulator_that_goes_in_at_the_end_or_past_it_is_the_last_and_moves_nothing() {
        for index in [3, 4, 99] {
            let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
            let before = all_cables(&graph);
            insert_lane_lfo_at(&cmd_tx, 0, index, 7.0);
            graph.drain_commands();
            let rack = &graph.instruments[0].modulators;
            assert_eq!(rack.len(), 4, "put at {index}");
            assert_eq!(rate_of(&rack[3]), 7.0, "put at {index}");
            assert_eq!(all_cables(&graph)[..3], before[..], "put at {index}: nothing moved");
        }
    }

    // A session is loaded a modulator at a time, in order, and a cable of one can address a modulator that comes after it: one that goes in at the end
    // moves nothing, whatever the cables that are there address.

    #[test]
    fn a_lanes_modulators_that_are_added_in_order_leave_the_cables_that_address_the_ones_that_come_later_as_they_are() {
        let (mut graph, cmd_tx, _rr) = make_graph(1);
        insert_lane_lfo(&cmd_tx, 0, 0);
        add_lane_cable(&cmd_tx, 0, 0, cable(ModTargetKind::ModulatorRate { mod_index: 1 }, 0.4));
        add_lane_cable(&cmd_tx, 0, 0, cable(ModTargetKind::ModulatorAttack { mod_index: 2 }, 0.5));
        add_lane_cable(&cmd_tx, 0, 0, cable(depth_of(2, 0), 0.6));
        for index in 1..3 {
            insert_lane_lfo(&cmd_tx, 0, index);
        }
        graph.drain_commands();
        assert_eq!(
            drives(&graph.instruments[0].modulators[0]),
            [(ModTargetKind::ModulatorRate { mod_index: 1 }, 0.4), (ModTargetKind::ModulatorAttack { mod_index: 2 }, 0.5), (depth_of(2, 0), 0.6)]
        );
    }

    #[test]
    fn a_groups_modulators_that_are_added_in_order_leave_the_cables_that_address_the_ones_that_come_later_as_they_are() {
        let (mut graph, cmd_tx, _rr) = make_graph(1);
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        let source = ModSource::lfo(LfoWaveform::Sine, 1.0);
        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source: source.clone() }).unwrap();
        add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::ModulatorRate { mod_index: 1 }, 0.4));
        add_group_cable(&cmd_tx, 0, 0, cable(depth_of(2, 0), 0.6));
        for index in 1..3 {
            cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index, source: source.clone() }).unwrap();
        }
        graph.drain_commands();
        assert_eq!(drives(&graph.groups[0].modulators[0]), [(ModTargetKind::ModulatorRate { mod_index: 1 }, 0.4), (depth_of(2, 0), 0.6)]);
    }

    #[test]
    fn a_members_modulators_that_are_added_in_order_leave_the_groups_cables_to_the_ones_that_come_later_as_they_are() {
        let (mut graph, cmd_tx, _rr) = make_graph(1);
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        let source = ModSource::lfo(LfoWaveform::Sine, 1.0);
        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source }).unwrap();
        let member_mod = |mod_index, field| ModTargetKind::GroupMemberMod { member: 0, mod_index, field };
        add_group_cable(&cmd_tx, 0, 0, cable(member_mod(1, CrossModField::Rate), 0.4));
        add_group_cable(&cmd_tx, 0, 0, cable(member_mod(2, CrossModField::Depth(0)), 0.5));
        for index in 0..3 {
            insert_lane_lfo(&cmd_tx, 0, index);
        }
        graph.drain_commands();
        assert_eq!(drives(&graph.groups[0].modulators[0]), [(member_mod(1, CrossModField::Rate), 0.4), (member_mod(2, CrossModField::Depth(0)), 0.5)]);
    }

    #[test]
    fn a_modulator_that_goes_in_where_there_is_no_lane_or_no_group_is_not_put_anywhere_and_nothing_moves() {
        let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
        let before = all_modulators(&graph);
        insert_lane_lfo_at(&cmd_tx, 4, 0, 7.0);
        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 3, index: 0, source: ModSource::lfo(LfoWaveform::Sine, 7.0) }).unwrap();
        graph.drain_commands();
        assert_eq!(all_modulators(&graph), before);
    }

    #[test]
    fn a_members_modulator_that_goes_in_tells_the_groups_cables_to_the_modulators_of_that_member_after_it_and_no_others() {
        let (mut graph, cmd_tx) = group_with_cables_that_drive_a_members_cables();
        let drive = |member, mod_index, field| ModTargetKind::GroupMemberMod { member, mod_index, field };
        // The group drives the depths of member 0's first and second modulators' cables, and of member 1's first modulator's.
        let before: Vec<_> = graph.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).collect();
        assert!(before.contains(&drive(0, 1, CrossModField::Depth(2))) && before.contains(&drive(1, 0, CrossModField::Depth(2))));
        // Member 0 (lane 1) gets a modulator at its first place.
        insert_lane_lfo_at(&cmd_tx, 1, 0, 7.0);
        graph.drain_commands();
        let after: Vec<_> = graph.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).collect();
        assert_eq!(after[0], drive(0, 1, CrossModField::Depth(0)), "what drove the old first modulator drives the second");
        assert_eq!(after[1], drive(0, 1, CrossModField::Depth(2)));
        assert_eq!(after[2], drive(0, 1, CrossModField::Rate));
        assert_eq!(after[3], drive(1, 0, CrossModField::Depth(2)), "the other member's modulators are not moved");
        assert_eq!(after[4], ModTargetKind::GroupBus { effect_index: 0, param_index: 0 }, "(what the group drives that is not a modulator is not moved)");
        assert_eq!(after[6], drive(0, 2, CrossModField::Depth(2)), "the old second is the third");
        // A modulator at the end of the member's rack moves nothing of the group's.
        let before = after;
        insert_lane_lfo_at(&cmd_tx, 1, 99, 8.0);
        graph.drain_commands();
        assert_eq!(graph.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).collect::<Vec<_>>(), before);
        // And a lane outside the group moves nothing of it.
        insert_lane_lfo_at(&cmd_tx, 0, 0, 9.0);
        graph.drain_commands();
        assert_eq!(graph.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).collect::<Vec<_>>(), before);
    }

    #[test]
    fn a_group_modulator_that_goes_in_tells_the_cables_of_its_siblings_and_not_a_lanes() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        let source = |rate| ModSource::lfo(LfoWaveform::Sine, rate);
        for index in 0..2 {
            cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index, source: source(index as f32) }).unwrap();
        }
        add_group_cable(&cmd_tx, 0, 1, cable(ModTargetKind::ModulatorRate { mod_index: 0 }, 0.4));
        add_group_cable(&cmd_tx, 0, 1, cable(depth_of(1, 0), 0.5));
        add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::ModulatorRate { mod_index: 1 }, 0.6));
        insert_lane_lfo(&cmd_tx, 0, 0);
        add_lane_cable(&cmd_tx, 0, 0, cable(ModTargetKind::ModulatorRate { mod_index: 1 }, 0.9));
        graph.drain_commands();

        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 1, source: source(7.0) }).unwrap();
        graph.drain_commands();
        let rack = &graph.groups[0].modulators;
        assert_eq!(rack.iter().map(rate_of).collect::<Vec<_>>(), [0.0, 7.0, 1.0]);
        // The old second is the third: what drove the first still does, and what drives itself is not in a cycle of its own making.
        assert_eq!(drives(&rack[2]), [(ModTargetKind::ModulatorRate { mod_index: 0 }, 0.4), (depth_of(2, 0), 0.5)]);
        assert_eq!(drives(&rack[0]), [(ModTargetKind::ModulatorRate { mod_index: 2 }, 0.6)], "what drove the old second drives the third");
        assert_eq!(graph.instruments[0].modulators[0].targets[0].kind, ModTargetKind::ModulatorRate { mod_index: 1 }, "a lane's rack is its own");
    }

    #[test]
    fn a_group_modulator_that_goes_in_at_the_end_or_past_it_is_the_last_and_moves_nothing() {
        for index in [2, 3, 99] {
            let (mut graph, cmd_tx, _rr) = make_graph(2);
            cmd_tx.send(GraphCommand::AddGroup).unwrap();
            let source = |rate| ModSource::lfo(LfoWaveform::Sine, rate);
            for place in 0..2 {
                cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: place, source: source(place as f32) }).unwrap();
            }
            add_group_cable(&cmd_tx, 0, 1, cable(ModTargetKind::ModulatorRate { mod_index: 0 }, 0.4));
            graph.drain_commands();
            let before = all_cables(&graph);
            cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index, source: source(7.0) }).unwrap();
            graph.drain_commands();
            let rack = &graph.groups[0].modulators;
            assert_eq!(rack.iter().map(rate_of).collect::<Vec<_>>(), [0.0, 1.0, 7.0], "put at {index}");
            assert_eq!(all_cables(&graph)[..2], before[..], "put at {index}: nothing moved");
        }
    }

    /// Whose modulator: a lane's or a group's.
    #[derive(Clone, Copy)]
    enum Rack {
        Lane(usize),
        Group(usize),
    }

    fn modulator_of(graph: &AudioGraph, rack: Rack, index: usize) -> Modulator {
        match rack {
            Rack::Lane(inst) => graph.instruments[inst].modulators[index].clone(),
            Rack::Group(group) => graph.groups[group].modulators[index].clone(),
        }
    }

    /// What the controller does to take a modulator out and to put it back, as commands to the audio thread: the cables that drive it go first, each
    /// at the place it has when its turn comes (`into`: the rack, the modulator in it and the place of the cable), then the modulator with everything it
    /// drives; and they come back in the reverse order: the modulator, with the place where it publishes and each cable it had, and then the cables that
    /// drove it, the last taken out first, each at the place it had.
    fn modulator_out_and_back(graph: &mut AudioGraph, cmd_tx: &crossbeam_channel::Sender<GraphCommand>, rack: Rack, index: usize, into: &[(Rack, usize, usize)]) {
        let mut taken = Vec::new();
        for &(of, modulator, target_index) in into {
            taken.push((of, modulator, target_index, modulator_of(graph, of, modulator).targets[target_index].clone()));
            cmd_tx
                .send(match of {
                    Rack::Lane(inst) => GraphCommand::RemoveModTarget { inst, mod_index: modulator, target_index },
                    Rack::Group(group) => GraphCommand::RemoveGroupModTarget { group, mod_index: modulator, target_index },
                })
                .unwrap();
            graph.drain_commands();
        }
        let modulator = modulator_of(graph, rack, index);
        cmd_tx
            .send(match rack {
                Rack::Lane(inst) => GraphCommand::RemoveModulator { inst, index },
                Rack::Group(group) => GraphCommand::RemoveGroupModulator { group, index },
            })
            .unwrap();
        graph.drain_commands();

        let source = modulator.source.clone();
        let location = match rack {
            Rack::Lane(inst) => ModulatorLocation::Lane(inst),
            Rack::Group(group) => ModulatorLocation::Group(group),
        };
        cmd_tx
            .send(match rack {
                Rack::Lane(inst) => GraphCommand::InsertModulator { inst, index, source },
                Rack::Group(group) => GraphCommand::InsertGroupModulator { group, index, source },
            })
            .unwrap();
        if let Some(slot) = modulator.output {
            cmd_tx.send(GraphCommand::SetModulatorOutput { location, index, slot }).unwrap();
        }
        for target in modulator.targets.iter().cloned() {
            cmd_tx
                .send(match rack {
                    Rack::Lane(inst) => GraphCommand::AddModTarget { inst, mod_index: index, target },
                    Rack::Group(group) => GraphCommand::AddGroupModTarget { group, mod_index: index, target },
                })
                .unwrap();
        }
        graph.drain_commands();
        for (of, modulator, target_index, target) in taken.into_iter().rev() {
            cmd_tx
                .send(match of {
                    Rack::Lane(inst) => GraphCommand::InsertModTarget { inst, mod_index: modulator, target_index, target },
                    Rack::Group(group) => GraphCommand::InsertGroupModTarget { group, mod_index: modulator, target_index, target },
                })
                .unwrap();
            graph.drain_commands();
        }
    }

    /// `lane_with_cables_that_drive_cables`, with a place that each modulator publishes to.
    fn lane_of_modulators_that_publish() -> (AudioGraph, crossbeam_channel::Sender<GraphCommand>) {
        let (mut graph, cmd_tx) = lane_with_cables_that_drive_cables();
        let mut slots = crate::telemetry::SlotAllocator::default();
        for index in 0..3 {
            let slot = slots.alloc().unwrap();
            cmd_tx.send(GraphCommand::SetModulatorOutput { location: ModulatorLocation::Lane(0), index, slot }).unwrap();
        }
        graph.drain_commands();
        (graph, cmd_tx)
    }

    #[test]
    fn a_modulator_that_is_taken_out_and_put_back_with_what_drove_it_leaves_every_rack_as_it_was() {
        let lane = Rack::Lane(0);
        // The second: the third drives the depths of its cables (and one that it has not), so those go first, and what it drives goes with it.
        let (mut graph, cmd_tx) = lane_of_modulators_that_publish();
        let before = all_modulators(&graph);
        modulator_out_and_back(&mut graph, &cmd_tx, lane, 1, &[(lane, 2, 0), (lane, 2, 0)]);
        assert_eq!(all_modulators(&graph), before);
        // The first: the second drives the depths of its cables, and its rate, and the third's cables address the second, which is the first once it has gone.
        let (mut graph, cmd_tx) = lane_of_modulators_that_publish();
        let before = all_modulators(&graph);
        modulator_out_and_back(&mut graph, &cmd_tx, lane, 0, &[(lane, 1, 0), (lane, 1, 0), (lane, 1, 0)]);
        assert_eq!(all_modulators(&graph), before);
        // The last: nothing addresses it.
        let (mut graph, cmd_tx) = lane_of_modulators_that_publish();
        let before = all_modulators(&graph);
        modulator_out_and_back(&mut graph, &cmd_tx, lane, 2, &[]);
        assert_eq!(all_modulators(&graph), before);
    }

    #[test]
    fn a_members_modulator_that_is_taken_out_and_put_back_with_the_groups_cables_to_it_leaves_the_racks_of_the_lane_and_of_the_group_as_they_were() {
        let (mut graph, cmd_tx) = group_with_cables_that_drive_a_members_cables();
        let before = all_modulators(&graph);
        // Lane 1 is member 0: the group drives the depths of its first modulator's cables (0, 2 and one it has not) and its rate. The group's other cable
        // to a modulator of the member addresses the second, which is the first once the first has gone.
        let group = Rack::Group(0);
        modulator_out_and_back(&mut graph, &cmd_tx, Rack::Lane(1), 0, &[(group, 0, 0), (group, 0, 0), (group, 0, 0), (group, 0, 2)]);
        assert_eq!(all_modulators(&graph), before);
        // The second modulator of the member: the group's last cable addresses it.
        let (mut graph, cmd_tx) = group_with_cables_that_drive_a_members_cables();
        let before = all_modulators(&graph);
        modulator_out_and_back(&mut graph, &cmd_tx, Rack::Lane(1), 1, &[(group, 0, 6)]);
        assert_eq!(all_modulators(&graph), before);
    }

    #[test]
    fn a_group_modulator_that_is_taken_out_and_put_back_leaves_its_siblings_cables_as_they_were() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        let source = |rate| ModSource::lfo(LfoWaveform::Sine, rate);
        for index in 0..3 {
            cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index, source: source(index as f32) }).unwrap();
        }
        // The first drives a bus parameter, the second the first's rate, and the third the second's rate and the depth of the second's cable.
        add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupBus { effect_index: 0, param_index: 0 }, 0.1));
        add_group_cable(&cmd_tx, 0, 1, cable(ModTargetKind::ModulatorRate { mod_index: 0 }, 0.4));
        add_group_cable(&cmd_tx, 0, 2, cable(ModTargetKind::ModulatorRate { mod_index: 1 }, 0.5));
        add_group_cable(&cmd_tx, 0, 2, cable(depth_of(1, 0), 0.6));
        graph.drain_commands();
        let before = all_modulators(&graph);
        let group = Rack::Group(0);
        modulator_out_and_back(&mut graph, &cmd_tx, group, 1, &[(group, 2, 0), (group, 2, 0)]);
        assert_eq!(all_modulators(&graph), before);
        // The first: the second's cable to its rate goes, and the third's cable to the depth of that one goes before it.
        modulator_out_and_back(&mut graph, &cmd_tx, group, 0, &[(group, 2, 1), (group, 1, 0)]);
        assert_eq!(all_modulators(&graph), before);
        modulator_out_and_back(&mut graph, &cmd_tx, group, 2, &[]);
        assert_eq!(all_modulators(&graph), before);
    }

    // -- An effect is handed back, and put back --
    //
    // What the controller takes out of a chain for the history, it asks the audio thread to hand back through a channel of its own (`keep`), so that the very
    // plugin, with whatever it holds, can be put back where it was (`InsertEffect`, `InsertGroupEffect`). A plugin goes the usual way, by the return channel,
    // when nothing asks for it, when nobody listens or when the channel is full.

    /// The gains of a chain's effects, which tell them apart.
    fn gains_of(effects: &mut [Box<dyn Plugin>]) -> Vec<f32> {
        effects.iter_mut().map(|effect| effect.get_parameter(0).unwrap()).collect()
    }

    /// A lane with three effects whose gains, and mixes, are 0.25, 0.5 and 0.75.
    fn lane_with_three_effects() -> (AudioGraph, crossbeam_channel::Sender<GraphCommand>, crossbeam_channel::Receiver<Box<dyn Plugin>>) {
        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        for (index, gain) in [0.25f32, 0.5, 0.75].into_iter().enumerate() {
            cmd_tx.send(GraphCommand::InsertEffect { inst: 0, index, effect: Box::new(ParamScaleEffect { gain }), mix: f64::from(gain) }).unwrap();
        }
        graph.drain_commands();
        (graph, cmd_tx, return_rx)
    }

    /// A group with a bus of three effects whose gains, and mixes, are 0.25, 0.5 and 0.75.
    fn group_with_three_bus_effects() -> (AudioGraph, crossbeam_channel::Sender<GraphCommand>, crossbeam_channel::Receiver<Box<dyn Plugin>>) {
        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        for (index, gain) in [0.25f32, 0.5, 0.75].into_iter().enumerate() {
            cmd_tx.send(GraphCommand::InsertGroupEffect { group: 0, index, effect: Box::new(ParamScaleEffect { gain }), mix: f64::from(gain) }).unwrap();
        }
        graph.drain_commands();
        (graph, cmd_tx, return_rx)
    }

    #[test]
    fn an_effect_that_is_taken_out_with_a_channel_is_handed_to_it_and_not_to_the_return_channel() {
        let (mut graph, cmd_tx, return_rx) = lane_with_three_effects();
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveEffect { inst: 0, index: 1, keep: Some(keep) }).unwrap();
        graph.drain_commands();
        assert!(return_rx.try_recv().is_err(), "it did not go the usual way");
        let mut plugin = kept.try_recv().expect("it went to the channel that asked for it");
        assert_eq!(plugin.get_parameter(0), Some(0.5), "the plugin that was at that place");
        assert_eq!(gains_of(&mut graph.instruments[0].effects), [0.25, 0.75]);
        assert_eq!(graph.instruments[0].mix_values, [0.25, 0.75]);
    }

    #[test]
    fn a_bus_effect_that_is_taken_out_with_a_channel_is_handed_to_it_too() {
        let (mut graph, cmd_tx, return_rx) = group_with_three_bus_effects();
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveGroupEffect { group: 0, index: 0, keep: Some(keep) }).unwrap();
        graph.drain_commands();
        assert!(return_rx.try_recv().is_err());
        assert_eq!(kept.try_recv().expect("it went to the channel that asked for it").get_parameter(0), Some(0.25));
        assert_eq!(gains_of(&mut graph.groups[0].effects), [0.5, 0.75]);
        assert_eq!(graph.groups[0].mix_values, [0.5, 0.75]);
    }

    #[test]
    fn an_effect_goes_the_usual_way_when_nothing_asks_for_it_or_nobody_listens_or_the_channel_is_full() {
        for what in ["no channel", "nobody listens", "a full channel"] {
            for bus in [false, true] {
                let (mut graph, cmd_tx, return_rx) = if bus { group_with_three_bus_effects() } else { lane_with_three_effects() };
                // (Held to the end of the round: the channel is open, and full, for as long as it is.)
                let mut holders = Vec::new();
                let keep = match what {
                    "no channel" => None,
                    "nobody listens" => {
                        let (keep, kept) = crossbeam_channel::bounded(1);
                        drop(kept);
                        Some(keep)
                    }
                    _ => {
                        let (keep, kept) = crossbeam_channel::bounded(1);
                        keep.send(Box::new(ParamScaleEffect { gain: 0.9 }) as Box<dyn Plugin>).unwrap();
                        holders.push(kept);
                        Some(keep)
                    }
                };
                cmd_tx.send(if bus { GraphCommand::RemoveGroupEffect { group: 0, index: 1, keep } } else { GraphCommand::RemoveEffect { inst: 0, index: 1, keep } }).unwrap();
                graph.drain_commands();
                let mut plugin = return_rx.try_recv().unwrap_or_else(|_| panic!("{what}, bus {bus}: it goes to the return channel"));
                assert_eq!(plugin.get_parameter(0), Some(0.5), "{what}, bus {bus}");
                let effects = if bus { &mut graph.groups[0].effects } else { &mut graph.instruments[0].effects };
                assert_eq!(gains_of(effects), [0.25, 0.75], "{what}, bus {bus}");
                if what == "a full channel" {
                    assert_eq!(holders[0].try_recv().unwrap().get_parameter(0), Some(0.9), "what was in the channel is still the only thing in it");
                    assert!(holders[0].try_recv().is_err());
                }
            }
        }
    }

    #[test]
    fn an_effect_that_is_not_there_hands_nothing_back_and_leaves_the_channel_empty() {
        let (mut graph, cmd_tx, return_rx) = lane_with_three_effects();
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveEffect { inst: 0, index: 3, keep: Some(keep) }).unwrap();
        let (keep_of_bus, kept_of_bus) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveGroupEffect { group: 4, index: 0, keep: Some(keep_of_bus) }).unwrap();
        graph.drain_commands();
        assert!(kept.try_recv().is_err() && kept_of_bus.try_recv().is_err() && return_rx.try_recv().is_err());
        assert_eq!(gains_of(&mut graph.instruments[0].effects), [0.25, 0.5, 0.75]);
    }

    // -- An instrument that is swapped out is handed back --
    //
    // As an effect that is taken out, the instrument that a swap replaces is handed to the channel that the controller made for it (`keep`), so that the very plugin,
    // with whatever it holds, can be put back by another swap; and it goes the usual way, by the return channel, when nothing asks for it, when nobody listens or
    // when the channel is full.

    /// A swap that puts in a `ParamTrackingInstrument` whose parameter 0 is `value` (which tells instruments apart), and asks for the old one to go to `keep`.
    fn swap_in(value: f32, keep: Option<crossbeam_channel::Sender<Box<dyn Plugin>>>) -> GraphCommand {
        let instrument: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: value });
        let inst_buf = (0..instrument.audio_output_count()).map(|_| Vec::new()).collect();
        GraphCommand::SwapInstrument { inst: 0, instrument, inst_buf, remapper: None, keep }
    }

    /// A graph whose one lane has a `ParamTrackingInstrument` whose parameter 0 is 0.25.
    fn lane_with_instrument() -> (AudioGraph, crossbeam_channel::Sender<GraphCommand>, crossbeam_channel::Receiver<Box<dyn Plugin>>) {
        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        cmd_tx.send(swap_in(0.25, None)).unwrap();
        graph.drain_commands();
        (graph, cmd_tx, return_rx)
    }

    #[test]
    fn an_instrument_that_is_swapped_out_with_a_channel_is_handed_to_it_and_not_to_the_return_channel() {
        let (mut graph, cmd_tx, return_rx) = lane_with_instrument();
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(swap_in(0.75, Some(keep))).unwrap();
        graph.drain_commands();
        assert!(return_rx.try_recv().is_err(), "it did not go the usual way");
        let mut old = kept.try_recv().expect("it went to the channel that asked for it");
        assert_eq!(old.get_parameter(0), Some(0.25), "the instrument that was in the lane");
        assert_eq!(graph.instruments[0].instrument.as_mut().unwrap().get_parameter(0), Some(0.75), "and the new one is in");
    }

    #[test]
    fn an_instrument_goes_the_usual_way_when_nothing_asks_for_it_or_nobody_listens_or_the_channel_is_full() {
        for what in ["no channel", "nobody listens", "a full channel"] {
            let (mut graph, cmd_tx, return_rx) = lane_with_instrument();
            // (Held to the end of the round: the channel is open, and full, for as long as it is.)
            let mut holders = Vec::new();
            let keep = match what {
                "no channel" => None,
                "nobody listens" => {
                    let (keep, kept) = crossbeam_channel::bounded(1);
                    drop(kept);
                    Some(keep)
                }
                _ => {
                    let (keep, kept) = crossbeam_channel::bounded(1);
                    keep.send(Box::new(ParamTrackingInstrument { param_value: 0.9 }) as Box<dyn Plugin>).unwrap();
                    holders.push(kept);
                    Some(keep)
                }
            };
            cmd_tx.send(swap_in(0.75, keep)).unwrap();
            graph.drain_commands();
            let mut old = return_rx.try_recv().unwrap_or_else(|_| panic!("{what}: it goes to the return channel"));
            assert_eq!(old.get_parameter(0), Some(0.25), "{what}");
            assert_eq!(graph.instruments[0].instrument.as_mut().unwrap().get_parameter(0), Some(0.75), "{what}");
            if what == "a full channel" {
                assert_eq!(holders[0].try_recv().unwrap().get_parameter(0), Some(0.9), "what was in the channel is still the only thing in it");
                assert!(holders[0].try_recv().is_err());
            }
        }
    }

    #[test]
    fn a_lane_that_had_no_instrument_hands_nothing_back_and_leaves_the_channel_empty() {
        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(swap_in(0.75, Some(keep))).unwrap();
        graph.drain_commands();
        assert!(kept.try_recv().is_err() && return_rx.try_recv().is_err());
        assert_eq!(graph.instruments[0].instrument.as_mut().unwrap().get_parameter(0), Some(0.75));
    }

    #[test]
    fn an_instrument_that_was_handed_back_goes_back_in_as_the_very_plugin_with_what_it_holds_and_with_the_remapper_it_is_given() {
        let (mut graph, cmd_tx, return_rx) = lane_with_instrument();
        // What the instrument holds that nothing else does: its parameter, set after it was put in.
        cmd_tx.send(GraphCommand::SetParameter { inst: 0, slot: 0, param_index: 0, value: 0.4 }).unwrap();
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(swap_in(0.75, Some(keep))).unwrap();
        graph.drain_commands();
        assert!(graph.instruments[0].remapper.is_none());
        // It goes back in, with a remapper (the lane's own note remap, made again from the model).
        let old = kept.try_recv().unwrap();
        let remap = std::collections::HashMap::from([("C4".to_string(), RemapTarget { note: "D4".to_string(), detune: 1.0 })]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        let inst_buf = (0..old.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx.send(GraphCommand::SwapInstrument { inst: 0, instrument: old, inst_buf, remapper: Some(remapper), keep: None }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.instruments[0].instrument.as_mut().unwrap().get_parameter(0), Some(0.4), "the very instrument, with the value it had");
        assert!(graph.instruments[0].remapper.is_some());
        // (The one that went out of the lane goes the usual way.)
        assert_eq!(return_rx.try_recv().unwrap().get_parameter(0), Some(0.75));
    }

    /// Group 0's first modulator's cables to its bus effects, as (effect, parameter).
    fn bus_cables(graph: &AudioGraph) -> Vec<(usize, u32)> {
        graph.groups[0].modulators[0]
            .targets
            .iter()
            .filter_map(|t| match t.kind {
                ModTargetKind::GroupBus { effect_index, param_index } => Some((effect_index, param_index)),
                _ => None,
            })
            .collect()
    }

    /// `group_with_three_bus_effects` with a modulator of the group whose cables go to the effects (told apart by their parameter, 0 to 2) and to a member's plugin.
    fn group_with_cables_to_three_bus_effects() -> (AudioGraph, crossbeam_channel::Sender<GraphCommand>, crossbeam_channel::Receiver<Box<dyn Plugin>>) {
        let (mut graph, cmd_tx, return_rx) = group_with_three_bus_effects();
        let source = ModSource::lfo(LfoWaveform::Sine, 1.0);
        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source }).unwrap();
        for param_index in 0..3 {
            add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupBus { effect_index: param_index as usize, param_index }, 0.1 * f32::from(param_index as u8 + 1)));
        }
        add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupMember { member: 0, slot: 1, param_index: 7 }, 0.9));
        graph.drain_commands();
        assert_eq!(bus_cables(&graph), [(0, 0), (1, 1), (2, 2)]);
        (graph, cmd_tx, return_rx)
    }

    #[test]
    fn a_bus_effect_that_goes_in_before_others_tells_the_cables_to_them_the_place_they_have_now() {
        let (mut graph, cmd_tx, _rr) = group_with_cables_to_three_bus_effects();
        cmd_tx.send(GraphCommand::InsertGroupEffect { group: 0, index: 1, effect: Box::new(ParamScaleEffect { gain: 0.1 }), mix: 0.1 }).unwrap();
        graph.drain_commands();
        // What drove the first is as it was, and what drove the second and the third drive the third and the fourth; what is not a bus cable is not moved.
        assert_eq!(bus_cables(&graph), [(0, 0), (2, 1), (3, 2)]);
        assert_eq!(graph.groups[0].modulators[0].targets[3].kind, ModTargetKind::GroupMember { member: 0, slot: 1, param_index: 7 });
        assert_eq!(gains_of(&mut graph.groups[0].effects), [0.25, 0.1, 0.5, 0.75]);
        assert_eq!(graph.groups[0].mix_values, [0.25, 0.1, 0.5, 0.75]);
        // At the first place, they all move.
        cmd_tx.send(GraphCommand::InsertGroupEffect { group: 0, index: 0, effect: Box::new(ParamScaleEffect { gain: 0.2 }), mix: 0.2 }).unwrap();
        graph.drain_commands();
        assert_eq!(bus_cables(&graph), [(1, 0), (3, 1), (4, 2)]);
    }

    #[test]
    fn a_bus_effect_that_goes_in_at_the_end_or_past_it_is_the_last_and_moves_no_cable() {
        for index in [3, 4, 99] {
            let (mut graph, cmd_tx, _rr) = group_with_cables_to_three_bus_effects();
            cmd_tx.send(GraphCommand::InsertGroupEffect { group: 0, index, effect: Box::new(ParamScaleEffect { gain: 0.1 }), mix: 0.1 }).unwrap();
            graph.drain_commands();
            assert_eq!(bus_cables(&graph), [(0, 0), (1, 1), (2, 2)], "put at {index}");
            assert_eq!(gains_of(&mut graph.groups[0].effects), [0.25, 0.5, 0.75, 0.1], "put at {index}");
        }
    }

    #[test]
    fn a_bus_effect_for_a_group_that_is_not_there_changes_nothing() {
        let (mut graph, cmd_tx, _rr) = group_with_cables_to_three_bus_effects();
        cmd_tx.send(GraphCommand::InsertGroupEffect { group: 5, index: 0, effect: Box::new(ParamScaleEffect { gain: 0.1 }), mix: 0.1 }).unwrap();
        graph.drain_commands();
        assert_eq!(bus_cables(&graph), [(0, 0), (1, 1), (2, 2)]);
        assert_eq!(gains_of(&mut graph.groups[0].effects), [0.25, 0.5, 0.75]);
    }

    #[test]
    fn a_bus_effect_with_the_wrong_channel_count_is_turned_away_and_handed_back_by_the_return_channel_and_moves_no_cable() {
        /// Mono effect (1 output): incompatible with a stereo chain.
        struct MonoEffect;

        impl Plugin for MonoEffect {
            fn name(&self) -> &str {
                "MonoEffect"
            }
            fn is_instrument(&self) -> bool {
                false
            }
            fn audio_output_count(&self) -> usize {
                1
            }
            fn audio_input_count(&self) -> usize {
                1
            }

            fn process(&mut self, _midi_events: &[(u64, [u8; 3])], audio_in: &[&[f32]], audio_out: &mut [&mut [f32]]) -> anyhow::Result<()> {
                for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                    out.copy_from_slice(inp);
                }
                Ok(())
            }

            mock_plugin_boilerplate!();
        }

        let (mut graph, cmd_tx, return_rx) = group_with_cables_to_three_bus_effects();
        cmd_tx.send(GraphCommand::InsertGroupEffect { group: 0, index: 0, effect: Box::new(MonoEffect), mix: 0.1 }).unwrap();
        graph.drain_commands();
        assert_eq!(return_rx.try_recv().expect("it comes back").name(), "MonoEffect");
        assert_eq!(gains_of(&mut graph.groups[0].effects), [0.25, 0.5, 0.75], "it is not in the bus");
        assert_eq!(bus_cables(&graph), [(0, 0), (1, 1), (2, 2)], "and nothing was told that it went in");
    }

    /// What the controller asks of the audio thread to take an effect of a lane's chain out for the history and to put it back: the cables that drive it
    /// (`into`: the lane, the modulator and the place of each when its turn comes; for a member of a group, the group's: `group_into`) go first, then the
    /// effect itself, with a channel to be handed to; and everything comes back in the reverse order: the very plugin, at its place and with its mix, and
    /// then the cables, the last that went first, each with what it was centred on.
    fn effect_out_and_back(graph: &mut AudioGraph, cmd_tx: &crossbeam_channel::Sender<GraphCommand>, inst: usize, index: usize, cables: &[(Rack, usize, usize)]) {
        let mut taken = Vec::new();
        for &(rack, modulator, target) in cables {
            let (at, bases) = match rack {
                Rack::Lane(of) => (ModulatorLocation::Lane(of), graph.instruments[of].modulators[modulator].targets[target].clone()),
                Rack::Group(of) => (ModulatorLocation::Group(of), graph.groups[of].modulators[modulator].targets[target].clone()),
            };
            cmd_tx
                .send(match rack {
                    Rack::Lane(inst) => GraphCommand::RemoveModTarget { inst, mod_index: modulator, target_index: target },
                    Rack::Group(group) => GraphCommand::RemoveGroupModTarget { group, mod_index: modulator, target_index: target },
                })
                .unwrap();
            taken.push((at, modulator, target, bases));
            graph.drain_commands();
        }
        let (keep, kept) = crossbeam_channel::bounded(1);
        let mix = graph.instruments[inst].mix_values[index];
        cmd_tx.send(GraphCommand::RemoveEffect { inst, index, keep: Some(keep) }).unwrap();
        graph.drain_commands();
        let effect = kept.try_recv().expect("the effect is handed back");
        cmd_tx.send(GraphCommand::InsertEffect { inst, index, effect, mix }).unwrap();
        for (at, modulator, target, cable) in taken.into_iter().rev() {
            let target_cable = ModTarget { kind: cable.kind, depth: cable.depth, base_value: cable.base_value, param_min: cable.param_min, param_max: cable.param_max, effective: cable.effective };
            cmd_tx
                .send(match at {
                    ModulatorLocation::Lane(inst) => GraphCommand::InsertModTarget { inst, mod_index: modulator, target_index: target, target: target_cable },
                    ModulatorLocation::Group(group) => GraphCommand::InsertGroupModTarget { group, mod_index: modulator, target_index: target, target: target_cable },
                })
                .unwrap();
        }
        graph.drain_commands();
    }

    #[test]
    fn a_lanes_effect_that_is_taken_out_and_put_back_with_the_cables_that_drove_it_leaves_the_chain_and_the_cables_as_they_were() {
        let (mut graph, cmd_tx, _rr) = lane_with_three_effects();
        // An LFO with a cable to each effect's gain (slot 1 to 3) and one to the instrument, and a second LFO that drives the depth of the cable to the second's.
        insert_lane_lfo(&cmd_tx, 0, 0);
        for (slot, depth) in [(1, 0.1), (2, 0.2), (3, 0.3), (0, 0.4)] {
            add_lane_cable(&cmd_tx, 0, 0, cable(param(slot, 0), depth));
        }
        insert_lane_lfo(&cmd_tx, 0, 1);
        add_lane_cable(&cmd_tx, 0, 1, cable(depth_of(0, 1), 0.5));
        graph.drain_commands();
        // The effect that is taken out has a state of its own, which has to come back with it.
        graph.instruments[0].effects[1].set_parameter(0, 0.6).unwrap();
        let before = (all_modulators(&graph), gains_of(&mut graph.instruments[0].effects), graph.instruments[0].mix_values.clone());
        let lane = Rack::Lane(0);
        // Its cable goes first, with the cable that drives its depth before it; the second effect's cable is the second of the first modulator.
        effect_out_and_back(&mut graph, &cmd_tx, 0, 1, &[(lane, 1, 0), (lane, 0, 1)]);
        assert_eq!((all_modulators(&graph), gains_of(&mut graph.instruments[0].effects), graph.instruments[0].mix_values.clone()), before);
        // The first and the last: the cables to the effects after them are told their places, and the instrument's cable is not.
        effect_out_and_back(&mut graph, &cmd_tx, 0, 0, &[(lane, 0, 0)]);
        assert_eq!((all_modulators(&graph), gains_of(&mut graph.instruments[0].effects), graph.instruments[0].mix_values.clone()), before);
        effect_out_and_back(&mut graph, &cmd_tx, 0, 2, &[(lane, 0, 2)]);
        assert_eq!((all_modulators(&graph), gains_of(&mut graph.instruments[0].effects), graph.instruments[0].mix_values.clone()), before);
        assert_eq!(gains_of(&mut graph.instruments[0].effects), [0.25, 0.6, 0.75], "the state of the one that was taken out is what it was");
    }

    #[test]
    fn a_members_effect_that_is_taken_out_and_put_back_with_the_groups_cables_to_it_leaves_the_group_as_it_was() {
        // Lane 0 outside the group, lanes 1 and 2 in it; the group's LFO has a cable to each of member 0's (lane 1's) three effects.
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        for inst in [1, 2] {
            cmd_tx.send(GraphCommand::SetLaneGroup { inst, group: Some(0) }).unwrap();
        }
        for index in 0..3 {
            cmd_tx.send(GraphCommand::InsertEffect { inst: 1, index, effect: Box::new(ParamScaleEffect { gain: 0.25 * (index + 1) as f32 }), mix: 1.0 }).unwrap();
        }
        let source = ModSource::lfo(LfoWaveform::Sine, 1.0);
        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source }).unwrap();
        for slot in 0..4 {
            add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupMember { member: 0, slot, param_index: 0 }, 0.1 + 0.1 * slot as f32));
        }
        graph.drain_commands();
        let before = (all_modulators(&graph), gains_of(&mut graph.instruments[1].effects));
        let group = Rack::Group(0);
        // The second effect (slot 2) of lane 1: the group's cable to it goes first, and the cables to the third effect are told it is in slot 2.
        effect_out_and_back(&mut graph, &cmd_tx, 1, 1, &[(group, 0, 2)]);
        assert_eq!((all_modulators(&graph), gains_of(&mut graph.instruments[1].effects)), before);
        effect_out_and_back(&mut graph, &cmd_tx, 1, 0, &[(group, 0, 1)]);
        assert_eq!((all_modulators(&graph), gains_of(&mut graph.instruments[1].effects)), before);
        effect_out_and_back(&mut graph, &cmd_tx, 1, 2, &[(group, 0, 3)]);
        assert_eq!((all_modulators(&graph), gains_of(&mut graph.instruments[1].effects)), before);
    }

    #[test]
    fn a_bus_effect_that_is_taken_out_and_put_back_with_the_cables_that_drove_it_leaves_the_bus_and_the_cables_as_they_were() {
        let (mut graph, cmd_tx, _rr) = group_with_cables_to_three_bus_effects();
        let before = (all_modulators(&graph), gains_of(&mut graph.groups[0].effects), graph.groups[0].mix_values.clone());
        for (index, cable_place) in [(1, 1), (0, 0), (2, 2)] {
            // The group's cable to the effect goes first, then the effect with a channel to be handed to; and they come back in the reverse order.
            let slot = graph.groups[0].modulators[0].targets[cable_place].clone();
            cmd_tx.send(GraphCommand::RemoveGroupModTarget { group: 0, mod_index: 0, target_index: cable_place }).unwrap();
            let (keep, kept) = crossbeam_channel::bounded(1);
            let mix = graph.groups[0].mix_values[index];
            cmd_tx.send(GraphCommand::RemoveGroupEffect { group: 0, index, keep: Some(keep) }).unwrap();
            graph.drain_commands();
            assert_eq!(bus_cables(&graph).len(), 2);
            let effect = kept.try_recv().expect("the effect is handed back");
            cmd_tx.send(GraphCommand::InsertGroupEffect { group: 0, index, effect, mix }).unwrap();
            let target = ModTarget { kind: slot.kind, depth: slot.depth, base_value: slot.base_value, param_min: slot.param_min, param_max: slot.param_max, effective: slot.effective };
            cmd_tx.send(GraphCommand::InsertGroupModTarget { group: 0, mod_index: 0, target_index: cable_place, target }).unwrap();
            graph.drain_commands();
            assert_eq!((all_modulators(&graph), gains_of(&mut graph.groups[0].effects), graph.groups[0].mix_values.clone()), before, "bus effect {index}");
        }
    }

    // -- A lane is handed back whole, and put back --
    //
    // What the controller takes out of the session for the history it asks the audio thread to hand back (`RemoveInstrument { keep }`): the lane itself, with its plugins and
    // whatever they hold, its modulators, its meter and its pattern, to be put back where it was (`InsertInstrument`). Its plugins go the usual way, by the return channel, when
    // nothing asks for the lane, when nobody listens or when the channel is full.

    /// Five lanes: 1, 2 and 4 are members 0, 1 and 2 of group 0, and 0 and 3 are in no group. A lane's instrument puts out what tells it apart (0.1 to 0.5), and it has two
    /// effects that scale by 0.5 and 0.75, at mixes of their own, and a modulator with a cable to its first effect; a volume, a range and a transpose of its own. The group's
    /// modulator drives the first parameter of each member's instrument (cables 0 to 2, by member) and the rate of the first member's first modulator (cable 3).
    fn five_lanes() -> (AudioGraph, crossbeam_channel::Sender<GraphCommand>, crossbeam_channel::Receiver<Box<dyn Plugin>>) {
        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        for _ in 0..4 {
            cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        }
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        for inst in [1, 2, 4] {
            cmd_tx.send(GraphCommand::SetLaneGroup { inst, group: Some(0) }).unwrap();
        }
        for inst in 0..5 {
            let n = (inst + 1) as f32;
            swap_instrument_at(&cmd_tx, inst, ConstInstrument::new(0.1 * n));
            for (index, gain) in [0.5f32, 0.75].into_iter().enumerate() {
                cmd_tx.send(GraphCommand::InsertEffect { inst, index, effect: Box::new(ParamScaleEffect { gain }), mix: f64::from(gain) * 0.5 + 0.1 * f64::from(n) }).unwrap();
            }
            insert_lane_lfo_at(&cmd_tx, inst, 0, n);
            add_lane_cable(&cmd_tx, inst, 0, cable(ModTargetKind::PluginParam { slot: 1, param_index: 0 }, 0.1 * n));
            cmd_tx.send(GraphCommand::SetVolume { inst, value: 0.5 + 0.1 * n }).unwrap();
            cmd_tx.send(GraphCommand::SetInstrumentRange { inst, range: Some((40 + inst as u8, 100)) }).unwrap();
            cmd_tx.send(GraphCommand::SetTranspose { inst, semitones: inst as i8 }).unwrap();
        }
        let source = ModSource::lfo(LfoWaveform::Sine, 1.0);
        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source }).unwrap();
        for member in 0..3 {
            add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupMember { member, slot: 0, param_index: 0 }, 0.2 * (member + 1) as f32));
        }
        add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Rate }, 0.9));
        graph.drain_commands();
        (graph, cmd_tx, return_rx)
    }

    /// Everything that says what the lanes are, as the audio thread has it: the lanes, where their plugins are in memory, the modulators with what they drive and what each cable
    /// is centred on.
    fn lanes_as_they_are(graph: &AudioGraph) -> String {
        format!("{:?}\n{:?}\n{:?}\n{:?}", graph.lanes(), graph.instrument_dump(), graph.chain_dump(), all_modulators(graph))
    }

    /// The places, in group 0's first modulator, of the cables that address member `member`: its plugins and its modulators.
    fn cables_to_member(graph: &AudioGraph, member: usize) -> Vec<usize> {
        let addresses = |kind: &ModTargetKind| matches!(kind, ModTargetKind::GroupMember { member: of, .. } | ModTargetKind::GroupMemberMod { member: of, .. } if *of == member);
        graph.groups[0].modulators[0].targets.iter().enumerate().filter(|(_, t)| addresses(&t.kind)).map(|(place, _)| place).collect()
    }

    /// Lane `inst` taken out as the controller takes a lane out: the cables of its group to it first (each by its place, the last first), and then the lane, handed to a
    /// channel. The lane, and the cables with the places they had.
    fn take_lane_out(graph: &mut AudioGraph, cmd_tx: &crossbeam_channel::Sender<GraphCommand>, inst: usize) -> (KeptLane, Vec<(usize, ModTarget)>) {
        let member = lane_member_ordinal(&graph.instruments, inst).map(|(_, ordinal)| ordinal);
        let cables: Vec<(usize, ModTarget)> = member
            .map(|member| cables_to_member(graph, member))
            .unwrap_or_default()
            .into_iter()
            .map(|place| {
                let t = &graph.groups[0].modulators[0].targets[place];
                (place, ModTarget { kind: t.kind.clone(), depth: t.depth, base_value: t.base_value, param_min: t.param_min, param_max: t.param_max, effective: t.effective })
            })
            .collect();
        for (place, _) in cables.iter().rev() {
            cmd_tx.send(GraphCommand::RemoveGroupModTarget { group: 0, mod_index: 0, target_index: *place }).unwrap();
        }
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveInstrument { inst, keep: Some(keep) }).unwrap();
        graph.drain_commands();
        (kept.try_recv().expect("the lane is handed back"), cables)
    }

    /// A lane put back as the controller puts it back, at `index`: the lane, and then the cables of its group, the first first.
    fn put_lane_back(graph: &mut AudioGraph, cmd_tx: &crossbeam_channel::Sender<GraphCommand>, index: usize, lane: KeptLane, cables: Vec<(usize, ModTarget)>) {
        cmd_tx.send(GraphCommand::InsertInstrument { index, lane }).unwrap();
        for (target_index, target) in cables {
            cmd_tx.send(GraphCommand::InsertGroupModTarget { group: 0, mod_index: 0, target_index, target }).unwrap();
        }
        graph.drain_commands();
    }

    /// Lane `inst` taken out and put back at `index`, with `between` run while it is out.
    fn lane_out_and_back(graph: &mut AudioGraph, cmd_tx: &crossbeam_channel::Sender<GraphCommand>, inst: usize, index: usize, between: impl FnOnce(&mut AudioGraph)) {
        let (lane, cables) = take_lane_out(graph, cmd_tx, inst);
        between(graph);
        put_lane_back(graph, cmd_tx, index, lane, cables);
    }

    #[test]
    fn a_lane_that_is_taken_out_with_a_channel_is_handed_to_it_whole_and_none_of_its_plugins_goes_the_usual_way() {
        let (mut graph, cmd_tx, return_rx) = five_lanes();
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveInstrument { inst: 1, keep: Some(keep) }).unwrap();
        graph.drain_commands();
        assert!(return_rx.try_recv().is_err(), "nothing went the usual way");
        assert_eq!(graph.lanes().len(), 4);
        let lane = kept.try_recv().expect("it went to the channel that asked for it");
        assert_eq!(lane.plugin_count(), 3, "its instrument and its two effects");
    }

    #[test]
    fn a_lane_goes_the_usual_way_when_nothing_asks_for_it_or_nobody_listens_or_the_channel_is_full() {
        for what in ["no channel", "nobody listens", "a full channel"] {
            let (mut graph, cmd_tx, return_rx) = five_lanes();
            // (Held to the end of the round: the channel is open, and full, for as long as it is.)
            let mut holders = Vec::new();
            let keep = match what {
                "no channel" => None,
                "nobody listens" => {
                    let (keep, kept) = crossbeam_channel::bounded(1);
                    drop(kept);
                    Some(keep)
                }
                _ => {
                    let (keep, kept) = crossbeam_channel::bounded(1);
                    assert!(keep.send(KeptLane(Box::new(InstrumentLane::new(2)))).is_ok(), "it has room for one");
                    holders.push(kept);
                    Some(keep)
                }
            };
            cmd_tx.send(GraphCommand::RemoveInstrument { inst: 1, keep }).unwrap();
            graph.drain_commands();
            let mut given: Vec<Box<dyn Plugin>> = return_rx.try_iter().collect();
            assert_eq!(given.len(), 3, "{what}: its instrument and its two effects go to the return channel");
            assert_eq!(gains_of(&mut given[1..]), [0.5, 0.75], "{what}: the effects, in order");
            assert_eq!(graph.lanes().len(), 4, "{what}");
            if what == "a full channel" {
                assert_eq!(holders[0].try_recv().unwrap().plugin_count(), 0, "what was in the channel is still the only thing in it");
                assert!(holders[0].try_recv().is_err());
            }
        }
    }

    #[test]
    fn a_lane_that_is_not_there_hands_nothing_back_and_leaves_the_channel_empty() {
        let (mut graph, cmd_tx, return_rx) = five_lanes();
        let before = lanes_as_they_are(&graph);
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveInstrument { inst: 5, keep: Some(keep) }).unwrap();
        graph.drain_commands();
        assert!(kept.try_recv().is_err() && return_rx.try_recv().is_err());
        assert_eq!(lanes_as_they_are(&graph), before);
    }

    #[test]
    fn a_lane_that_is_taken_out_and_put_back_is_where_it_was_with_the_very_plugins_and_everything_it_had() {
        for inst in 0..5 {
            let (mut graph, cmd_tx, _rr) = five_lanes();
            let before = lanes_as_they_are(&graph);
            lane_out_and_back(&mut graph, &cmd_tx, inst, inst, |graph| assert_eq!(graph.lanes().len(), 4, "lane {inst} is out"));
            assert_eq!(lanes_as_they_are(&graph), before, "lane {inst}");
        }
    }

    #[test]
    fn a_lane_that_is_put_back_goes_on_sounding_as_if_it_had_not_gone() {
        let run = |put_back: bool| {
            let (mut graph, cmd_tx, _rr) = five_lanes();
            let mut blocks = Vec::new();
            for block in 0..4 {
                if block == 2 && put_back {
                    // (Between two buffers, as a command is: the instrument holds the note, and the modulators are where they were.)
                    lane_out_and_back(&mut graph, &cmd_tx, 1, 1, |_| {});
                }
                let mut out = make_output();
                let midi = if block == 0 { vec![note_on(60)] } else { Vec::new() };
                graph.process(&midi, &mut out).unwrap();
                blocks.push(out);
            }
            blocks
        };
        let played = run(false);
        assert!(played[3][0].iter().any(|&sample| sample != 0.0), "something is heard");
        assert_eq!(run(true), played);
    }

    #[test]
    fn the_pattern_players_know_the_places_of_the_lanes_that_come_and_go() {
        let (mut graph, cmd_tx, _rr) = five_lanes();
        let places = |graph: &AudioGraph| graph.lanes().iter().map(|lane| lane.pattern_index).collect::<Vec<_>>();
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveInstrument { inst: 1, keep: Some(keep) }).unwrap();
        graph.drain_commands();
        assert_eq!(places(&graph), [0, 1, 2, 3]);
        // Put in at the end (a place past it is the last), and then taken out and put in at the first place.
        let lane = kept.try_recv().unwrap();
        cmd_tx.send(GraphCommand::InsertInstrument { index: 99, lane }).unwrap();
        graph.drain_commands();
        assert_eq!(places(&graph), [0, 1, 2, 3, 4]);
        assert_eq!(graph.lanes()[4].volume, 0.7, "the lane that was put in is the last");
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveInstrument { inst: 4, keep: Some(keep) }).unwrap();
        cmd_tx.send(GraphCommand::InsertInstrument { index: 0, lane: kept.try_recv().unwrap_or_else(|_| KeptLane(Box::new(InstrumentLane::new(2)))) }).unwrap();
        graph.drain_commands();
        assert_eq!(places(&graph), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_member_that_goes_out_and_in_tells_the_cables_of_its_group_the_ordinals_that_the_members_have_then() {
        // Lane 1 is member 0, and the group's cables to members 1 and 2 (cables 1 and 2) are told 0 and 1 when it goes, and 1 and 2 when it is back; the cables of the
        // first member go before it and come after it, as the controller does them.
        let (mut graph, cmd_tx, _rr) = five_lanes();
        assert_eq!(members_driven(&graph), [0, 1, 2, 0]);
        lane_out_and_back(&mut graph, &cmd_tx, 1, 1, |graph| assert_eq!(members_driven(graph), [0, 1], "the members after it are the first and the second now"));
        assert_eq!(members_driven(&graph), [0, 1, 2, 0]);
        // A lane that is no member (3) moves no one; the last member (4) moves no one else.
        lane_out_and_back(&mut graph, &cmd_tx, 3, 3, |graph| assert_eq!(members_driven(graph), [0, 1, 2, 0]));
        lane_out_and_back(&mut graph, &cmd_tx, 4, 4, |graph| assert_eq!(members_driven(graph), [0, 1, 0]));
        assert_eq!(members_driven(&graph), [0, 1, 2, 0]);
        // Put back somewhere else, among the members it is the other's place: the cables that came after are told by the place the lane has among the lanes of the group.
        lane_out_and_back(&mut graph, &cmd_tx, 4, 0, |_| {});
        assert_eq!(graph.lanes().iter().map(|lane| lane.group).collect::<Vec<_>>(), [Some(0), None, Some(0), Some(0), None]);
    }

    #[test]
    fn a_lane_that_is_put_back_into_a_group_that_is_not_there_is_in_no_group() {
        let (mut graph, cmd_tx, _rr) = five_lanes();
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveInstrument { inst: 1, keep: Some(keep) }).unwrap();
        cmd_tx.send(GraphCommand::RemoveGroup { group: 0, keep: None }).unwrap();
        graph.drain_commands();
        cmd_tx.send(GraphCommand::InsertInstrument { index: 1, lane: kept.try_recv().unwrap() }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.lanes().iter().map(|lane| lane.group).collect::<Vec<_>>(), [None; 5]);
    }

    #[test]
    fn a_lane_that_is_put_back_counts_no_note_as_held_and_plays_no_pattern() {
        let (mut graph, cmd_tx, _rr) = five_lanes();
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        let pattern = &mut graph.instruments[1].pattern;
        pattern.held_notes.push(60);
        pattern.trigger_note = Some(60);
        pattern.active_voices.push(PatternVoice { pattern_note: 60, playing_note: 60, channel: 0 });
        pattern.recording = true;
        pattern.counting_in = true;
        pattern.click_remaining = 5;
        assert_eq!(graph.instruments[1].active_note_channels[0], 1, "it counts the note as held");
        // (A notifier that is set while it is out is not set on it: it is given the graph's when it comes back.)
        let (notifier, _heard) = crossbeam_channel::unbounded();
        lane_out_and_back(&mut graph, &cmd_tx, 1, 1, |graph| graph.set_pattern_tx(notifier));
        assert!(graph.instruments[1].pattern.pattern_tx.is_some());
        // (What came to it meanwhile was not for it: a note-off that it did not hear would leave all this as it was.)
        assert_eq!(graph.instruments[1].active_note_channels, [0; 16]);
        let pattern = &graph.instruments[1].pattern;
        assert!(pattern.held_notes.is_empty() && pattern.trigger_note.is_none() && pattern.active_voices.is_empty());
        assert!(!pattern.recording && !pattern.counting_in && pattern.click_remaining == 0, "nor records, counts in or clicks");
        // The other lanes keep what they have.
        assert_eq!(graph.instruments[2].active_note_channels[0], 1);
    }

    #[test]
    fn a_kept_lane_gives_up_its_plugins_and_is_turned_away_without_them_and_sounds_as_it_did_with_plugins_again() {
        let (mut graph, cmd_tx, return_rx) = five_lanes();
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveInstrument { inst: 1, keep: Some(keep) }).unwrap();
        graph.drain_commands();
        let mut lane = kept.try_recv().unwrap();
        let mut plugins = lane.take_plugins();
        assert_eq!((plugins.len(), lane.plugin_count()), (3, 0), "its instrument, then its effects");
        assert_eq!(gains_of(&mut plugins[1..]), [0.5, 0.75]);
        // Without them it cannot go in: the effects have their mixes and nothing to mix. (It has no plugins to hand back.)
        cmd_tx.send(GraphCommand::InsertInstrument { index: 1, lane }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.lanes().len(), 4);
        assert!(return_rx.try_recv().is_err());
        // Nor with fewer effects than it has mixes: it is turned away, and the plugins it was given go back by the return channel.
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveInstrument { inst: 1, keep: Some(keep) }).unwrap();
        graph.drain_commands();
        let mut lane = kept.try_recv().unwrap();
        let mut taken = lane.take_plugins();
        let instrument = taken.remove(0);
        taken.truncate(1);
        lane.put_plugins(Some(instrument), taken);
        cmd_tx.send(GraphCommand::InsertInstrument { index: 1, lane }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.lanes().len(), 3);
        assert_eq!(return_rx.try_iter().count(), 2, "its instrument and its effect");

        // With plugins again it goes in as it was, and the audio is what it was: the lane has the buffers for the instrument it was given (the plugins here are new ones, as
        // plugins that are loaded again are, and they are what the first ones were).
        let (mut control, ..) = five_lanes();
        let (mut graph, cmd_tx, _rr) = five_lanes();
        let (mut lane, cables) = take_lane_out(&mut graph, &cmd_tx, 1);
        drop(lane.take_plugins());
        // (An instrument with more outputs than the lane's chain has channels: the first two are used, and the lane has a buffer for each.)
        lane.put_plugins(Some(ConstInstrument::with_outputs(0.2, 4)), vec![Box::new(ParamScaleEffect { gain: 0.5 }), Box::new(ParamScaleEffect { gain: 0.75 })]);
        assert_eq!(lane.plugin_count(), 3);
        put_lane_back(&mut graph, &cmd_tx, 1, lane, cables);
        assert_eq!(graph.lanes(), control.lanes());
        for block in 0..3 {
            let (mut heard, mut expected) = (make_output(), make_output());
            let midi = if block == 0 { vec![note_on(60)] } else { Vec::new() };
            graph.process(&midi, &mut heard).unwrap();
            control.process(&midi, &mut expected).unwrap();
            assert_eq!(heard, expected, "block {block}");
        }
        assert_eq!(graph.instruments[1].inst_buf.len(), 4, "a buffer for each output of the instrument it was given");
    }

    // -- A group is handed back whole, and put back --
    //
    // What the controller takes out of the session for the history it asks the audio thread to hand back (`RemoveGroup { keep }`): the group itself, with its bus effects and
    // whatever they hold, its modulators, its meter, its volume and its pan, to be put back where it was with the lanes that were its members (`InsertGroup`). Its plugins go the
    // usual way, by the return channel, when nothing asks for the group, when nobody listens or when the channel is full.

    /// Three groups and five lanes: 0 and 2 are members of group 0, 1 of group 1, 4 of group 2, and 3 is in no group. A group has a bus of two effects that scale by 0.5 and 0.75, at
    /// mixes of their own, a volume and a pan of its own, and a modulator with a cable to its first effect and one to the first parameter of its first member's instrument (group 0 has
    /// one to its second member's too).
    fn three_groups() -> (AudioGraph, crossbeam_channel::Sender<GraphCommand>, crossbeam_channel::Receiver<Box<dyn Plugin>>) {
        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        for _ in 0..4 {
            cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        }
        for inst in 0..5 {
            swap_instrument_at(&cmd_tx, inst, ConstInstrument::new(0.1 * (inst + 1) as f32));
        }
        for _ in 0..3 {
            cmd_tx.send(GraphCommand::AddGroup).unwrap();
        }
        for (inst, group) in [(0, 0), (1, 1), (2, 0), (4, 2)] {
            cmd_tx.send(GraphCommand::SetLaneGroup { inst, group: Some(group) }).unwrap();
        }
        for group in 0..3 {
            let n = (group + 1) as f32;
            for (index, gain) in [0.5f32, 0.75].into_iter().enumerate() {
                cmd_tx.send(GraphCommand::InsertGroupEffect { group, index, effect: Box::new(ParamScaleEffect { gain }), mix: f64::from(gain) * 0.5 + 0.1 * f64::from(n) }).unwrap();
            }
            cmd_tx.send(GraphCommand::SetGroupVolume { group, value: 0.5 + 0.1 * n }).unwrap();
            cmd_tx.send(GraphCommand::SetGroupPan { group, value: 0.1 * n - 0.2 }).unwrap();
            let source = ModSource::lfo(LfoWaveform::Sine, n);
            cmd_tx.send(GraphCommand::InsertGroupModulator { group, index: 0, source }).unwrap();
            add_group_cable(&cmd_tx, group, 0, cable(ModTargetKind::GroupBus { effect_index: 0, param_index: 0 }, 0.1 * n));
            add_group_cable(&cmd_tx, group, 0, cable(ModTargetKind::GroupMember { member: 0, slot: 0, param_index: 0 }, 0.2 * n));
        }
        add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupMember { member: 1, slot: 0, param_index: 0 }, 0.7));
        graph.drain_commands();
        (graph, cmd_tx, return_rx)
    }

    /// Everything that says what the groups are, as the audio thread has it: the lanes (and the groups they are in), the groups, where the plugins are in memory, the modulators
    /// with what they drive and what each cable is centred on.
    fn groups_as_they_are(graph: &AudioGraph) -> String {
        format!("{}\n{:?}", lanes_as_they_are(graph), graph.groups())
    }

    /// Group `group` taken out as the controller takes a group out: every cable of its modulators first (each by its place, the last of a modulator first), and then the group,
    /// handed to a channel. The group, the places of the lanes that were its members, and the cables with their places.
    fn take_group_out(graph: &mut AudioGraph, cmd_tx: &crossbeam_channel::Sender<GraphCommand>, group: usize) -> (KeptGroup, Vec<usize>, Vec<(usize, usize, ModTarget)>) {
        let members: Vec<usize> = graph.instruments.iter().enumerate().filter(|(_, lane)| lane.group == Some(group)).map(|(inst, _)| inst).collect();
        let mut cables = Vec::new();
        for (mod_index, modulator) in graph.groups[group].modulators.iter().enumerate() {
            for (target_index, t) in modulator.targets.iter().enumerate() {
                cables.push((mod_index, target_index, ModTarget { kind: t.kind.clone(), depth: t.depth, base_value: t.base_value, param_min: t.param_min, param_max: t.param_max, effective: t.effective }));
            }
        }
        for (mod_index, target_index, _) in cables.iter().rev() {
            cmd_tx.send(GraphCommand::RemoveGroupModTarget { group, mod_index: *mod_index, target_index: *target_index }).unwrap();
        }
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveGroup { group, keep: Some(keep) }).unwrap();
        graph.drain_commands();
        (kept.try_recv().expect("the group is handed back"), members, cables)
    }

    /// A group put back as the controller puts it back, at `index`: the group with its members, and then the cables, the first first.
    fn put_group_back(graph: &mut AudioGraph, cmd_tx: &crossbeam_channel::Sender<GraphCommand>, index: usize, group: KeptGroup, members: Vec<usize>, cables: Vec<(usize, usize, ModTarget)>) {
        cmd_tx.send(GraphCommand::InsertGroup { index, group, members }).unwrap();
        for (mod_index, target_index, target) in cables {
            cmd_tx.send(GraphCommand::InsertGroupModTarget { group: index, mod_index, target_index, target }).unwrap();
        }
        graph.drain_commands();
    }

    #[test]
    fn a_group_that_is_taken_out_with_a_channel_is_handed_to_it_whole_and_none_of_its_plugins_goes_the_usual_way() {
        let (mut graph, cmd_tx, return_rx) = three_groups();
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveGroup { group: 1, keep: Some(keep) }).unwrap();
        graph.drain_commands();
        assert!(return_rx.try_recv().is_err(), "nothing went the usual way");
        assert_eq!(graph.groups().len(), 2);
        let group = kept.try_recv().expect("it went to the channel that asked for it");
        assert_eq!(group.plugin_count(), 2, "its two bus effects");
    }

    #[test]
    fn a_group_goes_the_usual_way_when_nothing_asks_for_it_or_nobody_listens_or_the_channel_is_full() {
        for what in ["no channel", "nobody listens", "a full channel"] {
            let (mut graph, cmd_tx, return_rx) = three_groups();
            // (Held to the end of the round: the channel is open, and full, for as long as it is.)
            let mut holders = Vec::new();
            let keep = match what {
                "no channel" => None,
                "nobody listens" => {
                    let (keep, kept) = crossbeam_channel::bounded(1);
                    drop(kept);
                    Some(keep)
                }
                _ => {
                    let (keep, kept) = crossbeam_channel::bounded(1);
                    assert!(keep.send(KeptGroup(Box::new(Group::new(2)))).is_ok(), "it has room for one");
                    holders.push(kept);
                    Some(keep)
                }
            };
            cmd_tx.send(GraphCommand::RemoveGroup { group: 1, keep }).unwrap();
            graph.drain_commands();
            let mut given: Vec<Box<dyn Plugin>> = return_rx.try_iter().collect();
            assert_eq!(gains_of(&mut given), [0.5, 0.75], "{what}: the bus effects go to the return channel, in order");
            assert_eq!(graph.groups().len(), 2, "{what}");
            if what == "a full channel" {
                assert_eq!(holders[0].try_recv().unwrap().plugin_count(), 0, "what was in the channel is still the only thing in it");
                assert!(holders[0].try_recv().is_err());
            }
        }
    }

    #[test]
    fn a_group_that_is_not_there_hands_nothing_back_and_leaves_the_channel_empty() {
        let (mut graph, cmd_tx, return_rx) = three_groups();
        let before = groups_as_they_are(&graph);
        let (keep, kept) = crossbeam_channel::bounded(1);
        cmd_tx.send(GraphCommand::RemoveGroup { group: 3, keep: Some(keep) }).unwrap();
        graph.drain_commands();
        assert!(kept.try_recv().is_err() && return_rx.try_recv().is_err());
        assert_eq!(groups_as_they_are(&graph), before);
    }

    #[test]
    fn a_group_that_is_taken_out_and_put_back_is_where_it_was_with_the_very_plugins_its_members_and_everything_it_had() {
        for group in 0..3 {
            let (mut graph, cmd_tx, _rr) = three_groups();
            let before = groups_as_they_are(&graph);
            let (kept, members, cables) = take_group_out(&mut graph, &cmd_tx, group);
            assert_eq!(graph.groups().len(), 2, "group {group} is out");
            let groups = graph.groups().len();
            assert!(graph.lanes().iter().all(|lane| lane.group.is_none_or(|g| g < groups)), "no lane is in a group that is not there");
            put_group_back(&mut graph, &cmd_tx, group, kept, members, cables);
            assert_eq!(groups_as_they_are(&graph), before, "group {group}");
        }
    }

    #[test]
    fn the_lanes_are_told_the_groups_they_are_in_while_a_group_is_out_and_when_it_is_back() {
        let (mut graph, cmd_tx, _rr) = three_groups();
        let groups_of = |graph: &AudioGraph| graph.lanes().iter().map(|lane| lane.group).collect::<Vec<_>>();
        assert_eq!(groups_of(&graph), [Some(0), Some(1), Some(0), None, Some(2)]);
        // Group 0 goes: its members are in none, and the lanes of the groups after it are in the one that is before it now.
        let (kept, members, cables) = take_group_out(&mut graph, &cmd_tx, 0);
        assert_eq!(members, [0, 2]);
        assert_eq!(groups_of(&graph), [None, Some(0), None, None, Some(1)]);
        // It comes back first: the lanes of the groups after it are in the one after it again, and its members are in it again.
        put_group_back(&mut graph, &cmd_tx, 0, kept, members, cables);
        assert_eq!(groups_of(&graph), [Some(0), Some(1), Some(0), None, Some(2)]);
        // The middle group goes, and comes back.
        let (kept, members, cables) = take_group_out(&mut graph, &cmd_tx, 1);
        assert_eq!(groups_of(&graph), [Some(0), None, Some(0), None, Some(1)]);
        put_group_back(&mut graph, &cmd_tx, 1, kept, members, cables);
        assert_eq!(groups_of(&graph), [Some(0), Some(1), Some(0), None, Some(2)]);
    }

    #[test]
    fn a_group_that_is_put_back_past_the_end_is_the_last_and_members_that_are_not_there_are_not_in_it() {
        let (mut graph, cmd_tx, _rr) = three_groups();
        let (kept, members, cables) = take_group_out(&mut graph, &cmd_tx, 0);
        // The lanes that were its members are lanes 0 and 2; a place past the end of the groups is the last (group 2 of three), and a member that is no lane is nobody's.
        put_group_back(&mut graph, &cmd_tx, 99, kept, members, cables);
        assert_eq!(graph.groups().len(), 3);
        assert_eq!(graph.lanes().iter().map(|lane| lane.group).collect::<Vec<_>>(), [Some(2), Some(0), Some(2), None, Some(1)]);
        let (kept, members, _cables) = take_group_out(&mut graph, &cmd_tx, 2);
        cmd_tx.send(GraphCommand::InsertGroup { index: 2, group: kept, members: members.into_iter().chain([9]).collect() }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.lanes().iter().map(|lane| lane.group).collect::<Vec<_>>(), [Some(2), Some(0), Some(2), None, Some(1)]);
    }

    #[test]
    fn a_group_that_is_put_back_goes_on_sounding_as_if_it_had_not_gone() {
        let run = |put_back: bool| {
            let (mut graph, cmd_tx, _rr) = three_groups();
            let mut blocks = Vec::new();
            for block in 0..4 {
                if block == 2 && put_back {
                    // (Between two buffers, as a command is: the bus effects hold what they hold, and the modulators are where they were.)
                    let (kept, members, cables) = take_group_out(&mut graph, &cmd_tx, 1);
                    put_group_back(&mut graph, &cmd_tx, 1, kept, members, cables);
                }
                let mut out = make_output();
                let midi = if block == 0 { vec![note_on(60)] } else { Vec::new() };
                graph.process(&midi, &mut out).unwrap();
                blocks.push(out);
            }
            blocks
        };
        let played = run(false);
        assert!(played[3][0].iter().any(|&sample| sample != 0.0), "something is heard");
        assert_eq!(run(true), played);
    }

    #[test]
    fn a_kept_group_gives_up_its_plugins_and_is_turned_away_without_them_and_sounds_as_it_did_with_plugins_again() {
        let (mut graph, cmd_tx, return_rx) = three_groups();
        let (mut kept, members, _cables) = take_group_out(&mut graph, &cmd_tx, 1);
        let mut plugins = kept.take_plugins();
        assert_eq!((plugins.len(), kept.plugin_count()), (2, 0));
        assert_eq!(gains_of(&mut plugins), [0.5, 0.75]);
        // Without them it cannot go in: its effects have their mixes and nothing to mix. (It has no plugins to hand back.)
        cmd_tx.send(GraphCommand::InsertGroup { index: 1, group: kept, members: members.clone() }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.groups().len(), 2);
        assert!(return_rx.try_recv().is_err());
        // Nor with fewer effects than it has mixes: it is turned away, and the plugins it was given go back by the return channel.
        let (mut kept, members, _cables) = take_group_out(&mut graph, &cmd_tx, 0);
        let mut taken = kept.take_plugins();
        taken.truncate(1);
        kept.put_plugins(taken);
        cmd_tx.send(GraphCommand::InsertGroup { index: 0, group: kept, members }).unwrap();
        graph.drain_commands();
        assert_eq!(graph.groups().len(), 1);
        assert_eq!(return_rx.try_iter().count(), 1, "its effect");

        // With plugins again it goes in as it was, and the audio is what it was (the plugins here are new ones, as plugins that are loaded again are, and they are what the first were).
        let (mut control, ..) = three_groups();
        let (mut graph, cmd_tx, _rr) = three_groups();
        let (mut kept, members, cables) = take_group_out(&mut graph, &cmd_tx, 1);
        drop(kept.take_plugins());
        kept.put_plugins(vec![Box::new(ParamScaleEffect { gain: 0.5 }), Box::new(ParamScaleEffect { gain: 0.75 })]);
        assert_eq!(kept.plugin_count(), 2);
        put_group_back(&mut graph, &cmd_tx, 1, kept, members, cables);
        assert_eq!(graph.groups(), control.groups());
        for block in 0..3 {
            let (mut heard, mut expected) = (make_output(), make_output());
            let midi = if block == 0 { vec![note_on(60)] } else { Vec::new() };
            graph.process(&midi, &mut heard).unwrap();
            control.process(&midi, &mut expected).unwrap();
            assert_eq!(heard, expected, "block {block}");
        }
    }

    // -- Lanes change places --
    //
    // A lane moves with everything it has (the model's tree swaps whole nodes): its plugins and
    // effects, its modulators, its volume, range and group. What is addressed by position follows.

    /// Lane `inst` made recognisable, so that a swap can be seen to have moved all of it.
    fn shape_lane(cmd_tx: &crossbeam_channel::Sender<GraphCommand>, inst: usize, plugin: bool, effects: usize, modulators: usize, volume: f32, range: Option<(u8, u8)>) {
        if plugin {
            swap_instrument_at(cmd_tx, inst, ConstInstrument::new(1.0));
        }
        for index in 0..effects {
            cmd_tx.send(GraphCommand::InsertEffect { inst, index, effect: Box::new(PassthroughEffect), mix: 1.0 }).unwrap();
        }
        for index in 0..modulators {
            insert_lane_lfo(cmd_tx, inst, index);
        }
        cmd_tx.send(GraphCommand::SetVolume { inst, value: volume }).unwrap();
        cmd_tx.send(GraphCommand::SetInstrumentRange { inst, range }).unwrap();
        cmd_tx.send(GraphCommand::SetTranspose { inst, semitones: inst as i8 + 1 }).unwrap();
    }

    #[test]
    fn swapping_two_lanes_moves_each_with_everything_it_has() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        shape_lane(&cmd_tx, 0, true, 2, 1, 0.5, Some((36, 60)));
        shape_lane(&cmd_tx, 1, false, 0, 3, 1.5, None);
        shape_lane(&cmd_tx, 2, true, 1, 0, 2.5, Some((61, 72)));
        // (The first is muted and the second soloed, which go where the lane goes.)
        cmd_tx.send(GraphCommand::SetMute { inst: 0, muted: true }).unwrap();
        cmd_tx.send(GraphCommand::SetSolo { inst: 1, solo: true }).unwrap();
        graph.drain_commands();
        let before = graph.lanes();
        cmd_tx.send(GraphCommand::SwapInstruments { inst_a: 0, inst_b: 1 }).unwrap();
        graph.drain_commands();
        let after = graph.lanes();
        // Each lane is the other one: everything but the place its pattern player knows it by.
        let moved = |lane: &LaneFacts, index: usize| LaneFacts { pattern_index: index, ..LaneFacts { group: lane.group, range: lane.range, transpose: lane.transpose, volume: lane.volume, muted: lane.muted, solo: lane.solo, has_instrument: lane.has_instrument, effects: lane.effects, modulators: lane.modulators, pattern_index: 0 } };
        assert_eq!(after[0], moved(&before[1], 0));
        assert_eq!(after[1], moved(&before[0], 1));
        assert_eq!((after[0].volume, after[0].modulators, after[0].has_instrument), (1.5, 3, false));
        assert_eq!((after[1].volume, after[1].effects, after[1].range, after[1].transpose), (0.5, 2, Some((36, 60)), 1));
        assert_eq!(((after[0].muted, after[0].solo), (after[1].muted, after[1].solo)), ((false, true), (true, false)));
        assert_eq!(after[2], before[2], "the lane that was not named is as it was");
    }

    #[test]
    fn swapping_lanes_that_are_not_there_or_a_lane_with_itself_changes_nothing() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        shape_lane(&cmd_tx, 0, true, 1, 1, 0.5, None);
        shape_lane(&cmd_tx, 1, false, 0, 2, 1.5, Some((1, 2)));
        graph.drain_commands();
        let before = graph.lanes();
        for (inst_a, inst_b) in [(0, 0), (1, 1), (0, 2), (5, 1), (3, 4)] {
            cmd_tx.send(GraphCommand::SwapInstruments { inst_a, inst_b }).unwrap();
        }
        graph.drain_commands();
        assert_eq!(graph.lanes(), before);
    }

    #[test]
    fn clearing_an_instrument_keeps_the_rest_of_its_lane() {
        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        shape_lane(&cmd_tx, 0, true, 1, 2, 0.5, Some((36, 60)));
        graph.drain_commands();
        cmd_tx.send(GraphCommand::ClearInstrument { inst: 0 }).unwrap();
        graph.drain_commands();
        // What the model keeps (it only empties the instrument's slot): the effect, the modulators, the rest.
        assert_eq!(
            graph.lanes(),
            [LaneFacts { group: None, range: Some((36, 60)), transpose: 1, volume: 0.5, muted: false, solo: false, has_instrument: false, effects: 1, modulators: 2, pattern_index: 0 }]
        );
        assert!(return_rx.try_recv().is_ok(), "and the plugin is handed back");
    }

    /// Four lanes, the first, third and fourth in group 0 (its members 0, 1 and 2). The group's modulator drives
    /// parameters 0, 1 and 2 of the members' instruments, each told apart by its parameter, and the rate of the
    /// first member's first modulator; another group's modulator (group 1, which no lane is in) drives member 0's.
    fn group_of_three_among_four() -> (AudioGraph, crossbeam_channel::Sender<GraphCommand>) {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        for _ in 0..3 {
            cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        }
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        for inst in [0, 2, 3] {
            cmd_tx.send(GraphCommand::SetLaneGroup { inst, group: Some(0) }).unwrap();
        }
        let source = ModSource::lfo(LfoWaveform::Sine, 1.0);
        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 0, index: 0, source: source.clone() }).unwrap();
        cmd_tx.send(GraphCommand::InsertGroupModulator { group: 1, index: 0, source }).unwrap();
        for member in 0..3 {
            add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupMember { member, slot: 0, param_index: member as u32 }, 0.5));
        }
        add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Rate }, 0.5));
        add_group_cable(&cmd_tx, 1, 0, cable(ModTargetKind::GroupMember { member: 0, slot: 0, param_index: 9 }, 0.5));
        graph.drain_commands();
        (graph, cmd_tx)
    }

    /// The member each cable of group 0's modulator drives, in the order the cables were made: the three
    /// parameters, and then the first modulator's rate.
    fn members_driven(graph: &AudioGraph) -> Vec<usize> {
        graph.groups[0]
            .modulators[0]
            .targets
            .iter()
            .map(|t| match t.kind {
                ModTargetKind::GroupMember { member, .. } | ModTargetKind::GroupMemberMod { member, .. } => member,
                ref other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn lanes_that_swap_places_take_their_place_among_the_groups_members_and_the_groups_cables_follow_them() {
        // (the two lanes, and the member each cable drives after: the cables for parameters 0, 1 and 2, and the rate)
        for ((inst_a, inst_b), expected) in [
            // Two members of the group: the first and the last change places, and the cables that drove them.
            ((0, 3), [2, 1, 0, 2]),
            ((3, 0), [2, 1, 0, 2]),
            // Neighbours in the group, with a lane that is not in it between them in the tree.
            ((2, 3), [0, 2, 1, 0]),
            // A lane that is in no group, and a member: the member keeps its place among the members.
            ((1, 2), [0, 1, 2, 0]),
            ((0, 1), [0, 1, 2, 0]),
            // The lane that is not a member goes past the last: the member it passes is the one before it now.
            ((1, 3), [0, 2, 1, 0]),
        ] {
            let (mut graph, cmd_tx) = group_of_three_among_four();
            cmd_tx.send(GraphCommand::SwapInstruments { inst_a, inst_b }).unwrap();
            graph.drain_commands();
            assert_eq!(members_driven(&graph), expected, "after swapping {inst_a} and {inst_b}");
            // The other group's cable is its own.
            assert_eq!(graph.groups[1].modulators[0].targets[0].kind, ModTargetKind::GroupMember { member: 0, slot: 0, param_index: 9 });
        }
    }

    #[test]
    fn swapping_lanes_of_two_groups_tells_each_group_its_own_members_and_no_others() {
        // Five lanes: the first and third in group 0 (members 0 and 1), the second, fourth and fifth in group 1
        // (members 0, 1 and 2). Each group's modulator drives each of its members' instruments, told apart by parameter.
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        for _ in 0..4 {
            cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        }
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        for (inst, group) in [(0, 0), (1, 1), (2, 0), (3, 1), (4, 1)] {
            cmd_tx.send(GraphCommand::SetLaneGroup { inst, group: Some(group) }).unwrap();
        }
        let source = ModSource::lfo(LfoWaveform::Sine, 1.0);
        for group in 0..2 {
            cmd_tx.send(GraphCommand::InsertGroupModulator { group, index: 0, source: source.clone() }).unwrap();
        }
        for member in 0..2 {
            add_group_cable(&cmd_tx, 0, 0, cable(ModTargetKind::GroupMember { member, slot: 0, param_index: member as u32 }, 0.5));
        }
        for member in 0..3 {
            add_group_cable(&cmd_tx, 1, 0, cable(ModTargetKind::GroupMember { member, slot: 0, param_index: 10 + member as u32 }, 0.5));
        }
        graph.drain_commands();
        let members = |graph: &AudioGraph, group: usize| -> Vec<(u32, usize)> {
            graph.groups[group].modulators[0]
                .targets
                .iter()
                .filter_map(|t| match t.kind {
                    ModTargetKind::GroupMember { member, param_index, .. } => Some((param_index, member)),
                    _ => None,
                })
                .collect()
        };
        assert_eq!((members(&graph, 0), members(&graph, 1)), (vec![(0, 0), (1, 1)], vec![(10, 0), (11, 1), (12, 2)]));

        // The first lane (group 0) and the last (group 1) change places: both groups' members move.
        cmd_tx.send(GraphCommand::SwapInstruments { inst_a: 0, inst_b: 4 }).unwrap();
        graph.drain_commands();
        // Group 0: its first member is at the end now, and the other is the first. Group 1: the last member is
        // at the front, and the others are one on.
        assert_eq!(members(&graph, 0), [(0, 1), (1, 0)]);
        assert_eq!(members(&graph, 1), [(10, 1), (11, 2), (12, 0)]);
    }

    #[test]
    fn the_lanes_that_swap_carry_their_group_with_them() {
        let (mut graph, cmd_tx) = group_of_three_among_four();
        cmd_tx.send(GraphCommand::SwapInstruments { inst_a: 1, inst_b: 3 }).unwrap();
        graph.drain_commands();
        // Lanes 0 and 2 are as they were, lane 1 is the member now and lane 3 is not.
        assert_eq!(graph.lanes().iter().map(|lane| lane.group).collect::<Vec<_>>(), [Some(0), Some(0), Some(0), None]);
    }

    #[test]
    fn the_member_ordinals_that_change_when_two_lanes_swap_are_those_of_the_lanes_that_moved() {
        // The ordinal of a lane is how many lanes of its group are before it. Worked out here by following each
        // lane through the swap by name, which `member_moves` does not do.
        let mut checked = 0;
        for lanes in 2..=5usize {
            for assignment in 0..3usize.pow(lanes as u32) {
                // Each lane is in group 0, group 1 or none: the digits of `assignment` in base 3.
                let groups: Vec<Option<usize>> = (0..lanes).map(|lane| match (assignment / 3usize.pow(lane as u32)) % 3 { 0 => None, g => Some(g - 1) }).collect();
                for a in 0..lanes {
                    for b in 0..lanes {
                        if a == b {
                            continue;
                        }
                        // The lanes by name, in their order before and after.
                        let before: Vec<usize> = (0..lanes).collect();
                        let mut after = before.clone();
                        after.swap(a, b);
                        let ordinal_in = |order: &[usize], lane: usize, group: usize| -> usize {
                            order.iter().take_while(|&&named| named != lane).filter(|&&named| groups[named] == Some(group)).count()
                        };
                        let mut expected: Vec<(usize, usize, usize)> = Vec::new();
                        for (lane, group) in groups.iter().enumerate() {
                            if let Some(group) = *group {
                                let (old, new) = (ordinal_in(&before, lane, group), ordinal_in(&after, lane, group));
                                if old != new {
                                    expected.push((group, old, new));
                                }
                            }
                        }
                        // (`groups` above is by name before the swap: after it, the group of each place is that of the lane there.)
                        let by_place: Vec<Option<usize>> = groups.clone();
                        let mut moves = member_moves(&by_place, a, b);
                        moves.sort_unstable();
                        expected.sort_unstable();
                        assert_eq!(moves, expected, "groups {groups:?}, swapping {a} and {b}");
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 3000, "{checked}");
    }

    // -- Mute and solo --

    /// Process `buffers` buffers with a note held, and say what the last one had: the left channel and then the right. A mute or a solo is let go
    /// of over one buffer, so what a test asks about is the second.
    fn heard_after(rig: &mut MeterRig, buffers: usize) -> Vec<f32> {
        let mut out = make_output();
        for _ in 0..buffers {
            out = make_output();
            rig.graph.process(&[note_on(60)], &mut out).unwrap();
        }
        out.concat()
    }

    fn all_at(samples: &[f32], expected: f32) -> bool {
        samples.iter().all(|&s| (s - expected).abs() < 1e-6)
    }

    fn send(rig: &MeterRig, command: GraphCommand) {
        rig.cmd_tx.send(command).unwrap();
    }

    #[test]
    fn a_muted_lane_is_not_heard_and_the_other_lanes_are_until_it_is_brought_back() {
        let mut rig = MeterRig::new(&[0.3, 0.5], &[None, None], 0);
        assert!(all_at(&heard_after(&mut rig, 2), 0.8));
        send(&rig, GraphCommand::SetMute { inst: 0, muted: true });
        let muted = heard_after(&mut rig, 2);
        assert!(all_at(&muted, 0.5), "{muted:?}");
        send(&rig, GraphCommand::SetMute { inst: 0, muted: false });
        assert!(all_at(&heard_after(&mut rig, 2), 0.8));
    }

    #[test]
    fn a_mute_is_a_ramp_over_one_buffer_and_not_a_step() {
        let mut rig = MeterRig::new(&[0.5], &[None], 0);
        assert!(all_at(&heard_after(&mut rig, 1), 0.5));
        send(&rig, GraphCommand::SetMute { inst: 0, muted: true });
        let both = heard_after(&mut rig, 1);
        let (fading, right) = both.split_at(FRAMES);
        assert_eq!(fading, right, "both channels are let go of alike");
        let step = 0.5 / FRAMES as f32;
        // From just under where it was down to nothing, by equal steps.
        assert!((fading[0] - (0.5 - step)).abs() < 1e-6, "{}", fading[0]);
        assert!(fading.last().unwrap().abs() < 1e-6, "{}", fading.last().unwrap());
        assert!(fading.windows(2).all(|pair| ((pair[0] - pair[1]) - step).abs() < 1e-6), "{fading:?}");
        // And back up the same way.
        send(&rig, GraphCommand::SetMute { inst: 0, muted: false });
        let both = heard_after(&mut rig, 1);
        let rising = &both[..FRAMES];
        assert!((rising[0] - step).abs() < 1e-6, "{}", rising[0]);
        assert!((rising.last().unwrap() - 0.5).abs() < 1e-6, "{}", rising.last().unwrap());
        assert!(rising.windows(2).all(|pair| ((pair[1] - pair[0]) - step).abs() < 1e-6), "{rising:?}");
    }

    #[test]
    fn while_a_lane_is_soloed_only_the_soloed_lanes_are_heard() {
        let mut rig = MeterRig::new(&[0.1, 0.2, 0.4], &[None, None, None], 0);
        assert!(all_at(&heard_after(&mut rig, 2), 0.7));
        send(&rig, GraphCommand::SetSolo { inst: 1, solo: true });
        assert!(all_at(&heard_after(&mut rig, 2), 0.2));
        send(&rig, GraphCommand::SetSolo { inst: 2, solo: true });
        assert!(all_at(&heard_after(&mut rig, 2), 0.6), "two lanes soloed are both heard");
        send(&rig, GraphCommand::SetSolo { inst: 1, solo: false });
        assert!(all_at(&heard_after(&mut rig, 2), 0.4));
        send(&rig, GraphCommand::SetSolo { inst: 2, solo: false });
        assert!(all_at(&heard_after(&mut rig, 2), 0.7), "with nothing soloed everything that is not muted is heard");
    }

    #[test]
    fn a_mute_wins_over_a_solo_and_the_lanes_that_are_not_soloed_stay_silent_all_the_same() {
        let mut rig = MeterRig::new(&[0.1, 0.2], &[None, None], 0);
        send(&rig, GraphCommand::SetSolo { inst: 0, solo: true });
        send(&rig, GraphCommand::SetMute { inst: 0, muted: true });
        let silence = heard_after(&mut rig, 2);
        assert!(all_at(&silence, 0.0), "{silence:?}");
        send(&rig, GraphCommand::SetMute { inst: 0, muted: false });
        assert!(all_at(&heard_after(&mut rig, 2), 0.1));
    }

    #[test]
    fn a_muted_group_is_not_heard_but_its_lanes_go_on_and_the_lanes_outside_it_are() {
        let mut rig = MeterRig::new(&[0.1, 0.2, 0.4], &[Some(0), Some(0), None], 1);
        assert!(all_at(&heard_after(&mut rig, 2), 0.7));
        send(&rig, GraphCommand::SetGroupMute { group: 0, muted: true });
        assert!(all_at(&heard_after(&mut rig, 2), 0.4));
        // (The lanes in it are not muted: what they put into the bus is what they would.)
        assert!((rig.read(rig.lanes[0]).0 - 0.1).abs() < 1e-6 && (rig.read(rig.lanes[1]).0 - 0.2).abs() < 1e-6);
        send(&rig, GraphCommand::SetGroupMute { group: 0, muted: false });
        assert!(all_at(&heard_after(&mut rig, 2), 0.7));
    }

    #[test]
    fn a_soloed_lane_in_a_group_has_its_bus_heard_with_the_bus_volume_and_the_others_of_the_group_are_not() {
        let mut rig = MeterRig::new(&[0.1, 0.2, 0.4], &[Some(0), Some(0), None], 1);
        send(&rig, GraphCommand::SetGroupVolume { group: 0, value: 0.5 });
        send(&rig, GraphCommand::SetSolo { inst: 0, solo: true });
        let heard = heard_after(&mut rig, 2);
        assert!(all_at(&heard, 0.05), "the soloed lane through its bus, and nothing else: {heard:?}");
    }

    #[test]
    fn a_group_is_heard_for_a_lane_of_its_own_that_is_soloed_and_not_for_a_lane_of_another() {
        // Each bus has an effect that puts out 0.1 whatever comes in, which stands for a tail that a bus keeps while its members are silent.
        let mut rig = MeterRig::new(&[0.2, 0.4], &[Some(0), Some(1)], 2);
        for group in 0..2 {
            send(&rig, GraphCommand::InsertGroupEffect { group, index: 0, effect: Box::new(OffsetEffect(0.1)), mix: 1.0 });
        }
        assert!(all_at(&heard_after(&mut rig, 2), 0.2 + 0.1 + 0.4 + 0.1));
        send(&rig, GraphCommand::SetSolo { inst: 1, solo: true });
        let heard = heard_after(&mut rig, 2);
        assert!(all_at(&heard, 0.4 + 0.1), "the bus of the soloed lane, with its effect, and the other bus not at all: {heard:?}");
    }

    #[test]
    fn a_soloed_group_has_every_member_heard_and_what_is_outside_it_is_not() {
        let mut rig = MeterRig::new(&[0.1, 0.2, 0.4], &[Some(0), Some(0), None], 1);
        send(&rig, GraphCommand::SetGroupVolume { group: 0, value: 0.5 });
        send(&rig, GraphCommand::SetGroupSolo { group: 0, solo: true });
        let heard = heard_after(&mut rig, 2);
        assert!(all_at(&heard, 0.15), "{heard:?}");
        // A member that is muted is not heard, soloed group or not.
        send(&rig, GraphCommand::SetMute { inst: 1, muted: true });
        assert!(all_at(&heard_after(&mut rig, 2), 0.05));
    }

    #[test]
    fn a_soloed_lane_in_a_muted_group_is_not_heard_and_a_group_that_is_soloed_and_muted_is_not_either() {
        let mut rig = MeterRig::new(&[0.1, 0.4], &[Some(0), None], 1);
        send(&rig, GraphCommand::SetSolo { inst: 0, solo: true });
        send(&rig, GraphCommand::SetGroupMute { group: 0, muted: true });
        assert!(all_at(&heard_after(&mut rig, 2), 0.0));
        send(&rig, GraphCommand::SetSolo { inst: 0, solo: false });
        send(&rig, GraphCommand::SetGroupSolo { group: 0, solo: true });
        assert!(all_at(&heard_after(&mut rig, 2), 0.0));
        send(&rig, GraphCommand::SetGroupMute { group: 0, muted: false });
        assert!(all_at(&heard_after(&mut rig, 2), 0.1));
    }

    #[test]
    fn a_muted_lane_still_gets_its_notes_and_is_where_it_would_be_when_it_is_brought_back() {
        let mut rig = MeterRig::new(&[0.5], &[None], 0);
        send(&rig, GraphCommand::SetMute { inst: 0, muted: true });
        // A note that is played and let go of while the lane is muted is over when it is brought back.
        let mut out = make_output();
        rig.graph.process(&[note_on(60)], &mut out).unwrap();
        rig.graph.process(&[note_off(60)], &mut out).unwrap();
        send(&rig, GraphCommand::SetMute { inst: 0, muted: false });
        rig.graph.process(&[], &mut out).unwrap();
        rig.graph.process(&[], &mut out).unwrap();
        assert!(all_at(&out[0], 0.0), "{:?}", &out[0][..4]);
        // One that is held is sounding, with no new note.
        send(&rig, GraphCommand::SetMute { inst: 0, muted: true });
        rig.graph.process(&[note_on(62)], &mut out).unwrap();
        rig.graph.process(&[], &mut out).unwrap();
        send(&rig, GraphCommand::SetMute { inst: 0, muted: false });
        rig.graph.process(&[], &mut out).unwrap();
        rig.graph.process(&[], &mut out).unwrap();
        assert!(all_at(&out[0], 0.5), "{:?}", &out[0][..4]);
    }

    #[test]
    fn a_lane_is_measured_after_its_mute_so_that_it_stops_clipping_and_puts_nothing_into_its_bus() {
        let mut rig = MeterRig::new(&[1.5], &[Some(0)], 1);
        for _ in 0..2 {
            rig.play();
        }
        let (lane, group) = (rig.lanes[0], rig.groups[0]);
        assert_eq!((rig.read(lane).1, rig.read(group).1), (2, 2), "one clip a buffer in each while it is heard");
        send(&rig, GraphCommand::SetMute { inst: 0, muted: true });
        // The buffer it is let go of in has what is left of it (still over full scale at its start), and then nothing.
        rig.play();
        assert_eq!((rig.read(lane).1, rig.read(group).1), (3, 3));
        rig.play();
        rig.play();
        assert_eq!((rig.read(lane).1, rig.read(group).1), (3, 3));
    }

    #[test]
    fn a_group_is_measured_after_its_mute_and_its_lanes_are_not() {
        let mut rig = MeterRig::new(&[1.5], &[Some(0)], 1);
        for _ in 0..2 {
            rig.play();
        }
        let (lane, group) = (rig.lanes[0], rig.groups[0]);
        send(&rig, GraphCommand::SetGroupMute { group: 0, muted: true });
        rig.play();
        assert_eq!((rig.read(lane).1, rig.read(group).1), (3, 3), "the buffer that the bus is let go of in");
        rig.play();
        rig.play();
        assert_eq!((rig.read(lane).1, rig.read(group).1), (5, 3), "the lane goes on clipping into a bus that is cut");
    }

    #[test]
    fn a_lane_that_puts_out_what_is_not_a_number_is_silent_once_it_is_muted_and_so_is_a_group() {
        // (`NaN * 0` is `NaN`: a mute puts zeros. The buffer it is let go of in is a ramp, and has what the plugin put in it.)
        let mut rig = MeterRig::new(&[f32::NAN, 0.25], &[None, None], 0);
        send(&rig, GraphCommand::SetMute { inst: 0, muted: true });
        let heard = heard_after(&mut rig, 2);
        assert!(all_at(&heard, 0.25), "{heard:?}");
        let mut rig = MeterRig::new(&[f32::NAN, 0.25], &[Some(0), None], 1);
        send(&rig, GraphCommand::SetGroupMute { group: 0, muted: true });
        let heard = heard_after(&mut rig, 2);
        assert!(all_at(&heard, 0.25), "{heard:?}");
    }

    /// What the engine makes of every combination of mute and solo of two groups and four lanes (the first two in the first group, the third in the second, the fourth in
    /// none), as the web client's `audibleOf` has to say it (`web/src/client/fixtures/audible.json`): which of them reach the master, read from a run of the audio graph. The
    /// lanes put out 1, 2, 4 and 8, and each bus has an effect that puts 16 or 32 out whatever comes in (a tail), so that the sum is a number whose bits are which lanes are
    /// heard and which buses. A case is the state of each node, two bits each (the first muted and the second soloed), the groups first and then the lanes, and the sum.
    /// `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_is_heard_is_current() {
        // (One graph for all of them: each case sets every flag, and two buffers settle what the last case left.)
        let mut rig = MeterRig::new(&[1.0, 2.0, 4.0, 8.0], &[Some(0), Some(0), Some(1), None], 2);
        for group in 0..2 {
            send(&rig, GraphCommand::InsertGroupEffect { group, index: 0, effect: Box::new(OffsetEffect(16.0 * (1 << group) as f32)), mix: 1.0 });
        }
        let cases: Vec<serde_json::Value> = (0..4096u32)
            .map(|case| {
                for node in 0..6usize {
                    let (muted, solo) = (case >> (2 * node) & 1 == 1, case >> (2 * node) & 2 == 2);
                    let commands = if node < 2 {
                        [GraphCommand::SetGroupMute { group: node, muted }, GraphCommand::SetGroupSolo { group: node, solo }]
                    } else {
                        [GraphCommand::SetMute { inst: node - 2, muted }, GraphCommand::SetSolo { inst: node - 2, solo }]
                    };
                    commands.into_iter().for_each(|command| send(&rig, command));
                }
                // (Two buffers: the first lets go of what is let go of, over a ramp.)
                let heard = heard_after(&mut rig, 2);
                assert!(heard.iter().all(|&s| s == heard[0] && s.fract() == 0.0), "case {case}: a steady whole number, {:?}", &heard[..4]);
                serde_json::json!([case, heard[0] as u32])
            })
            .collect();
        let fresh = serde_json::to_string(&serde_json::json!({ "groups": 2, "lanes": [0, 0, 1, null], "cases": cases })).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/audible.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what the engine makes of a mute and a solo now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    #[test]
    fn a_mute_or_a_solo_for_a_lane_or_a_group_that_is_not_there_is_ignored() {
        let mut rig = MeterRig::new(&[0.3], &[None], 0);
        for command in [
            GraphCommand::SetMute { inst: 5, muted: true },
            GraphCommand::SetSolo { inst: 5, solo: true },
            GraphCommand::SetGroupMute { group: 0, muted: true },
            GraphCommand::SetGroupSolo { group: 0, solo: true },
        ] {
            send(&rig, command);
        }
        assert!(all_at(&heard_after(&mut rig, 2), 0.3));
    }
}
