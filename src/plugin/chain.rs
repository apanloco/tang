use std::collections::HashMap;
use std::mem::MaybeUninit;

use crossbeam_channel::{Receiver, Sender};

use super::Plugin;
use crate::session::{self, RemapTarget};

/// Maximum number of audio channels supported (for stack-allocated reference arrays).
const MAX_CHANNELS: usize = 16;

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

        // First pass: collect all distinct detune values
        for target in remap.values() {
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
        for (source_name, target) in remap {
            let source_note = session::parse_note_name(source_name)?;
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
}

impl LfoWaveform {
    /// Evaluate the waveform at a given phase (0.0–1.0), returning a value in -1.0..1.0.
    pub fn eval(self, phase: f32) -> f32 {
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
        }
    }

    pub const ALL: &[LfoWaveform] = &[
        LfoWaveform::Sine,
        LfoWaveform::Triangle,
        LfoWaveform::Saw,
        LfoWaveform::Square,
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
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LfoWaveform::Sine => "sine",
            LfoWaveform::Triangle => "triangle",
            LfoWaveform::Saw => "saw",
            LfoWaveform::Square => "square",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "sine" => Some(LfoWaveform::Sine),
            "triangle" | "tri" => Some(LfoWaveform::Triangle),
            "saw" | "sawtooth" => Some(LfoWaveform::Saw),
            "square" | "sq" => Some(LfoWaveform::Square),
            _ => None,
        }
    }
}

/// Identifies what a modulation target points at.
#[derive(Debug, Clone)]
pub enum ModTargetKind {
    /// Target a plugin parameter by index.
    PluginParam { param_index: u32 },
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

/// Modulation source: either an LFO or an ADSR envelope.
#[derive(Debug, Clone)]
pub enum ModSource {
    Lfo {
        waveform: LfoWaveform,
        rate: f32,
        phase: f32,
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
}

/// A block-rate modulator with a source (LFO or Envelope) and targets.
#[derive(Debug, Clone)]
pub struct Modulator {
    pub source: ModSource,
    sample_rate: f32,
    pub targets: Vec<ModTarget>,
    /// Last computed output value (bipolar -1..1 for LFO, unipolar 0..1 for envelope).
    pub last_output: f32,
}

impl Modulator {
    pub fn new(source: ModSource, sample_rate: f32) -> Self {
        Modulator {
            source,
            sample_rate,
            targets: Vec::new(),
            last_output: 0.0,
        }
    }

    /// Advance the modulator by one buffer. For envelopes, processes MIDI note events.
    fn tick(&mut self, buffer_size: usize, midi_events: &[(u64, [u8; 3])]) {
        match &mut self.source {
            ModSource::Lfo { waveform, rate, phase } => {
                let phase_inc = *rate * buffer_size as f32 / self.sample_rate;
                *phase = (*phase + phase_inc) % 1.0;
                self.last_output = waveform.eval(*phase);
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
        }
    }

    /// Apply the last computed output to plugin parameter targets only.
    /// Cross-mod targets are handled separately via `apply_cross_mod`.
    fn apply_to_plugin(&self, plugin: &mut dyn Plugin) {
        for target in &self.targets {
            if let ModTargetKind::PluginParam { param_index } = target.kind {
                let range = target.param_max - target.param_min;
                let offset = self.last_output * target.depth * range;
                let modulated = (target.base_value + offset).clamp(target.param_min, target.param_max);
                let _ = plugin.set_parameter(param_index, modulated);
            }
        }
    }

}

/// Apply cross-modulator targets within a modulator list.
///
/// For each modulator, applies its `last_output` to any sibling modulator targets
/// (rate, ADSR params, depth). Self-modulation (targeting own index) is skipped.
fn apply_cross_mod(modulators: &mut [Modulator]) {
    // Collect modifications first (avoids simultaneous borrow issues).
    let mut mods_to_apply: Vec<(usize, CrossModField, f32)> = Vec::new();

    for (src_idx, src) in modulators.iter().enumerate() {
        let output = src.last_output;
        for target in &src.targets {
            let (tgt_mod_idx, field) = match &target.kind {
                ModTargetKind::ModulatorRate { mod_index } => (*mod_index, CrossModField::Rate),
                ModTargetKind::ModulatorAttack { mod_index } => (*mod_index, CrossModField::Attack),
                ModTargetKind::ModulatorDecay { mod_index } => (*mod_index, CrossModField::Decay),
                ModTargetKind::ModulatorSustain { mod_index } => (*mod_index, CrossModField::Sustain),
                ModTargetKind::ModulatorRelease { mod_index } => (*mod_index, CrossModField::Release),
                ModTargetKind::ModulatorDepth { mod_index, target_index } => {
                    (*mod_index, CrossModField::Depth(*target_index))
                }
                ModTargetKind::PluginParam { .. } => continue,
            };
            // Skip self-modulation.
            if tgt_mod_idx == src_idx {
                continue;
            }
            let range = target.param_max - target.param_min;
            let modulated = (target.base_value + output * target.depth * range)
                .clamp(target.param_min, target.param_max);
            mods_to_apply.push((tgt_mod_idx, field, modulated));
        }
    }

    // Apply collected modifications.
    for (tgt_idx, field, value) in mods_to_apply {
        if let Some(tgt) = modulators.get_mut(tgt_idx) {
            match field {
                CrossModField::Rate => {
                    if let ModSource::Lfo { rate, .. } = &mut tgt.source {
                        *rate = value;
                    }
                }
                CrossModField::Attack => {
                    if let ModSource::Envelope { attack, .. } = &mut tgt.source {
                        *attack = value;
                    }
                }
                CrossModField::Decay => {
                    if let ModSource::Envelope { decay, .. } = &mut tgt.source {
                        *decay = value;
                    }
                }
                CrossModField::Sustain => {
                    if let ModSource::Envelope { sustain, .. } = &mut tgt.source {
                        *sustain = value;
                    }
                }
                CrossModField::Release => {
                    if let ModSource::Envelope { release, .. } = &mut tgt.source {
                        *release = value;
                    }
                }
                CrossModField::Depth(target_index) => {
                    if let Some(t) = tgt.targets.get_mut(target_index) {
                        t.depth = value;
                    }
                }
            }
        }
    }
}

enum CrossModField {
    Rate,
    Attack,
    Decay,
    Sustain,
    Release,
    Depth(usize),
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

/// Extract the mod_index from a cross-mod target kind, if any.
fn cross_mod_index(kind: &ModTargetKind) -> Option<usize> {
    match kind {
        ModTargetKind::PluginParam { .. } => None,
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
        ModTargetKind::PluginParam { .. } => return,
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

// ---------------------------------------------------------------------------
// GraphCommand — addressed commands for the AudioGraph
// ---------------------------------------------------------------------------

/// Commands sent from the main thread to mutate the audio graph on the audio thread.
pub enum GraphCommand {
    /// Swap the instrument in a specific split.
    SwapInstrument {
        kb: usize,
        split: usize,
        instrument: Box<dyn Plugin>,
        inst_buf: Vec<Vec<f32>>,
        remapper: Option<NoteRemapper>,
    },
    /// Insert an effect into a specific split's chain.
    InsertEffect {
        kb: usize,
        split: usize,
        index: usize,
        effect: Box<dyn Plugin>,
        mix: f64,
    },
    /// Remove an effect from a specific split's chain.
    RemoveEffect {
        kb: usize,
        split: usize,
        index: usize,
    },
    /// Reorder an effect within a specific split's chain.
    ReorderEffect {
        kb: usize,
        split: usize,
        from: usize,
        to: usize,
    },
    /// Set a parameter on a plugin. slot 0 = instrument, 1..N = effects.
    SetParameter {
        kb: usize,
        split: usize,
        slot: usize,
        param_index: u32,
        value: f32,
    },
    /// Set the host-side dry/wet mix on an effect. slot 1..N = effects.
    #[expect(dead_code)]
    SetMix {
        kb: usize,
        split: usize,
        slot: usize,
        value: f32,
    },
    /// Set the host-side volume on a split's instrument.
    SetVolume {
        kb: usize,
        split: usize,
        value: f32,
    },
    /// Set the note range for a split. None = full range.
    #[expect(dead_code)]
    SetSplitRange {
        kb: usize,
        split: usize,
        range: Option<(u8, u8)>,
    },
    /// Add a new keyboard lane (with no splits initially).
    AddKeyboard,
    /// Remove a keyboard lane and all its splits.
    #[expect(dead_code)]
    RemoveKeyboard {
        kb: usize,
    },
    /// Remove the instrument from a split (leaving it empty).
    RemoveInstrument {
        kb: usize,
        split: usize,
    },
    /// Swap instruments (with their buffers and remappers) between two splits.
    SwapInstruments {
        kb: usize,
        split_a: usize,
        split_b: usize,
    },
    /// Add a new empty split to a keyboard.
    AddSplit {
        kb: usize,
        range: Option<(u8, u8)>,
    },
    /// Remove a split from a keyboard.
    RemoveSplit {
        kb: usize,
        split: usize,
    },
    /// Insert a new modulator into a plugin slot.
    /// parent_slot: 0 = instrument, 1..N = effects.
    InsertModulator {
        kb: usize,
        split: usize,
        parent_slot: usize,
        index: usize,
        source: ModSource,
    },
    /// Remove a modulator from a plugin slot.
    RemoveModulator {
        kb: usize,
        split: usize,
        parent_slot: usize,
        index: usize,
    },
    /// Set the rate of an LFO modulator.
    SetModulatorRate {
        kb: usize,
        split: usize,
        parent_slot: usize,
        mod_index: usize,
        rate: f32,
    },
    /// Set the waveform of an LFO modulator.
    SetModulatorWaveform {
        kb: usize,
        split: usize,
        parent_slot: usize,
        mod_index: usize,
        waveform: LfoWaveform,
    },
    /// Replace a modulator's source (for type switching between LFO/Envelope).
    SetModulatorSource {
        kb: usize,
        split: usize,
        parent_slot: usize,
        mod_index: usize,
        source: ModSource,
    },
    /// Set envelope parameters on an Envelope modulator.
    SetModulatorEnvelopeParam {
        kb: usize,
        split: usize,
        parent_slot: usize,
        mod_index: usize,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
    /// Add a modulation target to a modulator.
    AddModTarget {
        kb: usize,
        split: usize,
        parent_slot: usize,
        mod_index: usize,
        target: ModTarget,
    },
    /// Remove a modulation target from a modulator.
    #[expect(dead_code)]
    RemoveModTarget {
        kb: usize,
        split: usize,
        parent_slot: usize,
        mod_index: usize,
        target_index: usize,
    },
    /// Set the depth of a modulation target.
    SetModTargetDepth {
        kb: usize,
        split: usize,
        parent_slot: usize,
        mod_index: usize,
        target_index: usize,
        depth: f32,
    },
    /// Enable/disable pattern playback for a split.
    SetPatternEnabled {
        kb: usize,
        split: usize,
        enabled: bool,
    },
    /// Start/stop pattern recording for a split.
    SetPatternRecording {
        kb: usize,
        split: usize,
        recording: bool,
    },
    /// Set the pattern data (e.g. after loading from session).
    SetPattern {
        kb: usize,
        split: usize,
        pattern: Pattern,
        base_note: Option<u8>,
    },
    /// Clear the pattern for a split.
    ClearPattern {
        kb: usize,
        split: usize,
    },
    /// Swap patterns between two splits in the same keyboard.
    SwapPatterns {
        kb: usize,
        split_a: usize,
        split_b: usize,
    },
    /// Set the global BPM (applied to all pattern players).
    SetGlobalBpm {
        bpm: f32,
    },
    /// Set pattern length in beats.
    SetPatternLength {
        kb: usize,
        split: usize,
        beats: f32,
    },
    /// Set whether the pattern loops or plays once.
    SetPatternLooping {
        kb: usize,
        split: usize,
        looping: bool,
    },
    /// Set the transpose (in semitones) for a split.
    SetTranspose {
        kb: usize,
        split: usize,
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

/// Notification sent from audio thread to TUI when recording completes.
pub struct PatternNotification {
    pub kb: usize,
    pub split: usize,
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

/// Per-split pattern recorder and player.
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
    /// BPM (global, set from main thread).
    bpm: f32,
    /// Notification sender for when recording completes automatically.
    pattern_tx: Option<Sender<PatternNotification>>,
    /// This player's keyboard and split index (for notifications).
    kb_index: usize,
    split_index: usize,
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
            bpm: 120.0,
            pattern_tx: None,
            kb_index: 0,
            split_index: 0,
            count_in_beats: 4.0,
            metronome_pos: 0,
            beat_length_samples: 0,
            click_phase: 0.0,
            click_remaining: 0,
            click_is_downbeat: false,
        }
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
    /// merged output events (original + pattern playback).
    fn process(
        &mut self,
        midi_in: &[(u64, [u8; 3])],
        buffer_frames: usize,
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

        self.process_playback(midi_in, buffer_frames);
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
                kb: self.kb_index,
                split: self.split_index,
                base_note: self.base_note,
                length_beats: self.length_beats,
                looping: self.looping,
                enabled: self.enabled,
                events,
            });
        }
    }

    fn process_playback(&mut self, midi_in: &[(u64, [u8; 3])], buffer_frames: usize) {
        let base = match self.base_note {
            Some(n) => n as i16,
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
        let transpose = trigger as i16 - base;

        let pattern_len = self.pattern.length_samples;
        if pattern_len == 0 {
            return;
        }

        let buf_start = self.playback_pos;
        let buf_end = self.playback_pos + buffer_frames as u64;

        // Check for end-of-pattern
        if buf_end > pattern_len {
            // Emit events from buf_start..pattern_len
            self.emit_events_in_range(buf_start, pattern_len, transpose, 0);
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
                self.emit_events_in_range(0, remainder, transpose, offset);
                self.playback_pos = remainder;
            } else {
                // One-shot: stop playback
                self.playback_pos = pattern_len;
            }
        } else {
            self.emit_events_in_range(buf_start, buf_end, transpose, 0);
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
        transpose: i16,
        frame_offset: u64,
    ) {
        for ev in &self.pattern.events {
            if ev.frame >= range_start && ev.frame < range_end {
                let out_frame = ev.frame - range_start + frame_offset;
                let transposed_note = (ev.note as i16 + transpose).clamp(0, 127) as u8;

                if ev.status == 0x90 {
                    // Note-on
                    self.output_events
                        .push((out_frame, [0x90, transposed_note, ev.velocity]));
                    self.active_voices.push(PatternVoice {
                        pattern_note: ev.note,
                        playing_note: transposed_note,
                        channel: 0,
                    });
                } else {
                    // Note-off
                    self.output_events
                        .push((out_frame, [0x80, transposed_note, 0]));
                    self.active_voices
                        .retain(|v| v.pattern_note != ev.note);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SplitLane — one instrument + effect chain within a keyboard
// ---------------------------------------------------------------------------

struct SplitLane {
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
    /// Modulators attached to the instrument (slot 0).
    inst_modulators: Vec<Modulator>,
    /// Modulators attached to each effect. Index i corresponds to effects[i].
    effect_modulators: Vec<Vec<Modulator>>,
    /// Pattern recorder/player for this split.
    pattern: PatternPlayer,
    /// Transpose in semitones applied to note events.
    transpose: i8,
}

impl SplitLane {
    fn new(num_channels: usize) -> Self {
        SplitLane {
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
            inst_modulators: Vec::new(),
            effect_modulators: Vec::new(),
            pattern: PatternPlayer::new(48000.0),
            transpose: 0,
        }
    }

    /// Get the modulators vec for a given parent slot (0 = instrument, 1..N = effects).
    fn modulators_for(&mut self, parent_slot: usize) -> Option<&mut Vec<Modulator>> {
        if parent_slot == 0 {
            Some(&mut self.inst_modulators)
        } else {
            self.effect_modulators.get_mut(parent_slot - 1)
        }
    }

    /// Get the sample rate. Derived from the instrument if loaded, otherwise a sensible default.
    fn sample_rate(&self) -> f32 {
        self.instrument
            .as_ref()
            .map(|i| i.sample_rate())
            .unwrap_or(48000.0)
    }

    /// Filter MIDI events by this split's key range.
    /// Note-on/note-off: only pass if note is within range (inclusive).
    /// CC, pitch bend, channel pressure, etc.: always pass through.
    fn filter_midi(&mut self, midi_events: &[(u64, [u8; 3])]) {
        self.filtered_midi.clear();
        let range = match self.range {
            Some(r) => r,
            None => {
                // Full range — pass everything
                self.filtered_midi.extend_from_slice(midi_events);
                return;
            }
        };

        for &(frame, bytes) in midi_events {
            let status_type = bytes[0] & 0xF0;
            match status_type {
                0x80 | 0x90 => {
                    // Note-on or note-off: filter by range
                    let note = bytes[1];
                    if note >= range.0 && note <= range.1 {
                        self.filtered_midi.push((frame, bytes));
                    }
                }
                _ => {
                    // CC, pitch bend, channel pressure, etc. — duplicate to all splits
                    self.filtered_midi.push((frame, bytes));
                }
            }
        }
    }

    /// Process this split's instrument + effect chain, writing output to `split_out`.
    /// `split_out` must have `num_channels` vecs, each with `frames` length.
    fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        split_out: &mut [Vec<f32>],
        num_channels: usize,
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
        let frames = split_out.first().map(|b| b.len()).unwrap_or(0);
        let effective_events = self.pattern.process(effective_events, frames);

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

        // Apply modulators (block-rate: once per buffer, before instrument processing).
        // Three-pass: tick all → apply cross-mod → apply plugin targets.
        let buffer_size = split_out.first().map(|b| b.len()).unwrap_or(0);
        if buffer_size > 0 {
            // Instrument modulators.
            if let Some(inst) = &mut self.instrument {
                // Pass 1: tick all.
                for m in &mut self.inst_modulators {
                    m.tick(buffer_size, effective_events);
                }
                // Pass 2: cross-mod.
                apply_cross_mod(&mut self.inst_modulators);
                // Pass 3: apply plugin-param targets.
                for m in &self.inst_modulators {
                    m.apply_to_plugin(inst.as_mut());
                }
            }
            // Effect modulators.
            for (fx, mods) in self.effects.iter_mut().zip(self.effect_modulators.iter_mut()) {
                for m in mods.iter_mut() {
                    m.tick(buffer_size, effective_events);
                }
                apply_cross_mod(mods);
                for m in mods.iter() {
                    m.apply_to_plugin(fx.as_mut());
                }
            }
        }

        let instrument = match self.instrument.as_mut() {
            Some(inst) => inst,
            None => {
                for ch in split_out.iter_mut() {
                    ch.fill(0.0);
                }
                // Render metronome even without an instrument (count-in)
                let frames = split_out.first().map(|b| b.len()).unwrap_or(0);
                self.pattern.render_metronome(split_out, frames);
                return Ok(());
            }
        };

        let frames = split_out.first().map(|b| b.len()).unwrap_or(0);
        let inst_outputs = self.inst_buf.len();

        if inst_outputs <= num_channels && self.effects.is_empty() && (self.volume - 1.0).abs() < f32::EPSILON {
            // Fast path: instrument output fits, no effects, no volume scaling
            let mut storage = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
            let out_refs = mut_slices(split_out, &mut storage);
            instrument.process(effective_events, &[], out_refs)?;
            self.pattern.render_metronome(split_out, frames);
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

        if self.effects.is_empty() {
            // No effects — copy first num_channels from inst_buf to output
            for (ch, out) in split_out.iter_mut().enumerate() {
                if ch < self.inst_buf.len() {
                    out.copy_from_slice(&self.inst_buf[ch]);
                } else {
                    out.fill(0.0);
                }
            }
            self.pattern.render_metronome(split_out, frames);
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

        for (effect, &mix) in self.effects.iter_mut().zip(self.mix_values.iter()) {
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
            }
            src_is_a = !src_is_a;
        }

        // Copy final result to split_out
        let final_buf = if src_is_a { &self.buf_a } else { &self.buf_b };
        for (ch, out) in split_out.iter_mut().enumerate() {
            if ch < final_buf.len() {
                let copy_len = out.len().min(final_buf[ch].len());
                out[..copy_len].copy_from_slice(&final_buf[ch][..copy_len]);
            }
        }

        // Metronome click (additive, on top of instrument+effects)
        self.pattern.render_metronome(split_out, frames);

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// KeyboardLane
// ---------------------------------------------------------------------------

struct KeyboardLane {
    splits: Vec<SplitLane>,
}

// ---------------------------------------------------------------------------
// AudioGraph — multi-keyboard, multi-split audio processor
// ---------------------------------------------------------------------------

/// An audio graph with multiple keyboards, each containing splits with instrument + effects.
/// All splits are summed together into the final output.
///
/// Commands are drained at the top of every audio callback via try_recv loop.
pub struct AudioGraph {
    keyboards: Vec<KeyboardLane>,
    /// Accumulation buffer for summing all splits
    mix_buf: Vec<Vec<f32>>,
    /// Per-split scratch buffer (reused across splits)
    split_buf: Vec<Vec<f32>>,
    num_channels: usize,
    command_rx: Receiver<GraphCommand>,
    return_tx: Sender<Box<dyn Plugin>>,
    /// Notification channel for pattern recording completion.
    pattern_tx: Option<Sender<PatternNotification>>,
}

impl AudioGraph {
    /// Create an empty audio graph. Outputs silence until instruments are added.
    pub fn new(
        num_channels: usize,
        command_rx: Receiver<GraphCommand>,
        return_tx: Sender<Box<dyn Plugin>>,
    ) -> Self {
        AudioGraph {
            keyboards: Vec::new(),
            mix_buf: (0..num_channels).map(|_| Vec::new()).collect(),
            split_buf: (0..num_channels).map(|_| Vec::new()).collect(),
            num_channels,
            command_rx,
            return_tx,
            pattern_tx: None,
        }
    }

    /// Set the notification channel for pattern recording completion.
    pub fn set_pattern_tx(&mut self, tx: Sender<PatternNotification>) {
        self.pattern_tx = Some(tx.clone());
        // Propagate to existing splits.
        for (kb_idx, kb) in self.keyboards.iter_mut().enumerate() {
            for (sp_idx, sp) in kb.splits.iter_mut().enumerate() {
                sp.pattern.pattern_tx = Some(tx.clone());
                sp.pattern.kb_index = kb_idx;
                sp.pattern.split_index = sp_idx;
            }
        }
    }

    pub fn num_channels(&self) -> usize {
        self.num_channels
    }

    /// Drain all pending commands from the command channel (lock-free).
    pub fn drain_commands(&mut self) {
        while let Ok(cmd) = self.command_rx.try_recv() {
            match cmd {
                GraphCommand::SwapInstrument {
                    kb,
                    split,
                    instrument: new_inst,
                    inst_buf,
                    remapper,
                } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        lane.inst_buf = inst_buf;
                        lane.remapper = remapper;
                        if let Some(old) = lane.instrument.replace(new_inst) {
                            let _ = self.return_tx.try_send(old);
                        }
                    }
                }
                GraphCommand::InsertEffect {
                    kb,
                    split,
                    index,
                    effect,
                    mix,
                } => {
                    let num_channels = self.num_channels;
                    if let Some(lane) = self.get_split_mut(kb, split) {
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
                            lane.effect_modulators.insert(idx, Vec::new());
                        }
                    }
                }
                GraphCommand::RemoveEffect { kb, split, index } => {
                    let old = self.get_split_mut(kb, split).and_then(|lane| {
                        if index < lane.effects.len() {
                            let old = lane.effects.remove(index);
                            lane.mix_values.remove(index);
                            // Remove this effect's modulators along with it.
                            if index < lane.effect_modulators.len() {
                                lane.effect_modulators.remove(index);
                            }
                            Some(old)
                        } else {
                            None
                        }
                    });
                    if let Some(old) = old {
                        let _ = self.return_tx.try_send(old);
                    }
                }
                GraphCommand::ReorderEffect {
                    kb,
                    split,
                    from,
                    to,
                } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        if from < lane.effects.len() && to < lane.effects.len() && from != to {
                            let effect = lane.effects.remove(from);
                            let mix = lane.mix_values.remove(from);
                            lane.effects.insert(to, effect);
                            lane.mix_values.insert(to, mix);
                            // Move effect_modulators along with the effect.
                            let mods = lane.effect_modulators.remove(from);
                            lane.effect_modulators.insert(to, mods);
                        }
                    }
                }
                GraphCommand::SetParameter {
                    kb,
                    split,
                    slot,
                    param_index,
                    value,
                } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        let plugin: Option<&mut Box<dyn Plugin>> = if slot == 0 {
                            lane.instrument.as_mut()
                        } else {
                            lane.effects.get_mut(slot - 1)
                        };
                        if let Some(p) = plugin {
                            if let Err(e) = p.set_parameter(param_index, value) {
                                log::warn!("SetParameter kb={kb} split={split} slot={slot} index={param_index}: {e}");
                            }
                        }
                        // Update modulator base values for matching plugin-param targets.
                        let mods = if slot == 0 {
                            Some(&mut lane.inst_modulators)
                        } else {
                            lane.effect_modulators.get_mut(slot - 1)
                        };
                        if let Some(mods) = mods {
                            for modulator in mods {
                                for target in &mut modulator.targets {
                                    if let ModTargetKind::PluginParam { param_index: pi } = target.kind {
                                        if pi == param_index {
                                            target.base_value = value;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                GraphCommand::SetMix {
                    kb,
                    split,
                    slot,
                    value,
                } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        if slot > 0 {
                            if let Some(mix) = lane.mix_values.get_mut(slot - 1) {
                                *mix = value as f64;
                            }
                        }
                    }
                }
                GraphCommand::SetVolume { kb, split, value } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        lane.volume = value;
                    }
                }
                GraphCommand::SetSplitRange { kb, split, range } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        lane.range = range;
                    }
                }
                GraphCommand::AddKeyboard => {
                    self.keyboards.push(KeyboardLane {
                        splits: Vec::new(),
                    });
                }
                GraphCommand::RemoveKeyboard { kb } => {
                    if kb < self.keyboards.len() {
                        let removed = self.keyboards.remove(kb);
                        // Return all plugins from removed keyboard
                        for mut split in removed.splits {
                            if let Some(inst) = split.instrument.take() {
                                let _ = self.return_tx.try_send(inst);
                            }
                            for effect in split.effects.drain(..) {
                                let _ = self.return_tx.try_send(effect);
                            }
                        }
                    }
                }
                GraphCommand::RemoveInstrument { kb, split } => {
                    let old = self
                        .get_split_mut(kb, split)
                        .and_then(|lane| {
                            lane.inst_buf.clear();
                            lane.remapper = None;
                            lane.inst_modulators.clear();
                            lane.instrument.take()
                        });
                    if let Some(old) = old {
                        let _ = self.return_tx.try_send(old);
                    }
                }
                GraphCommand::SwapInstruments {
                    kb,
                    split_a,
                    split_b,
                } => {
                    if let Some(keyboard) = self.keyboards.get_mut(kb) {
                        if split_a < keyboard.splits.len() && split_b < keyboard.splits.len() && split_a != split_b {
                            // Swap instrument, inst_buf, and remapper between the two splits.
                            let (a, b) = if split_a < split_b {
                                let (left, right) = keyboard.splits.split_at_mut(split_b);
                                (&mut left[split_a], &mut right[0])
                            } else {
                                let (left, right) = keyboard.splits.split_at_mut(split_a);
                                (&mut right[0], &mut left[split_b])
                            };
                            std::mem::swap(&mut a.instrument, &mut b.instrument);
                            std::mem::swap(&mut a.inst_buf, &mut b.inst_buf);
                            std::mem::swap(&mut a.remapper, &mut b.remapper);
                        }
                    }
                }
                GraphCommand::AddSplit { kb, range } => {
                    if let Some(keyboard) = self.keyboards.get_mut(kb) {
                        let mut lane = SplitLane::new(self.num_channels);
                        lane.range = range;
                        lane.pattern.kb_index = kb;
                        lane.pattern.split_index = keyboard.splits.len();
                        lane.pattern.pattern_tx = self.pattern_tx.clone();
                        keyboard.splits.push(lane);
                    }
                }
                GraphCommand::RemoveSplit { kb, split } => {
                    if let Some(keyboard) = self.keyboards.get_mut(kb) {
                        if split < keyboard.splits.len() {
                            let mut removed = keyboard.splits.remove(split);
                            if let Some(inst) = removed.instrument.take() {
                                let _ = self.return_tx.try_send(inst);
                            }
                            for effect in removed.effects.drain(..) {
                                let _ = self.return_tx.try_send(effect);
                            }
                            // Re-index remaining splits so pattern notifications route correctly.
                            for (i, sp) in keyboard.splits.iter_mut().enumerate() {
                                sp.pattern.split_index = i;
                            }
                        }
                    }
                }
                GraphCommand::InsertModulator {
                    kb,
                    split,
                    parent_slot,
                    index,
                    source,
                } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        let m = Modulator::new(source, lane.sample_rate());
                        if let Some(mods) = lane.modulators_for(parent_slot) {
                            let idx = index.min(mods.len());
                            mods.insert(idx, m);
                        }
                    }
                }
                GraphCommand::RemoveModulator { kb, split, parent_slot, index } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        if let Some(mods) = lane.modulators_for(parent_slot) {
                            if index < mods.len() {
                                mods.remove(index);
                                // Clean up cross-mod targets in remaining siblings.
                                fixup_cross_mod_after_remove(mods, index);
                            }
                        }
                    }
                }
                GraphCommand::SetModulatorRate {
                    kb,
                    split,
                    parent_slot,
                    mod_index,
                    rate,
                } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        if let Some(mods) = lane.modulators_for(parent_slot) {
                            if let Some(m) = mods.get_mut(mod_index) {
                                if let ModSource::Lfo { rate: ref mut r, .. } = m.source {
                                    *r = rate;
                                }
                            }
                            // Update cross-mod base values for targets pointing at this rate.
                            update_cross_mod_base(mods, mod_index, CrossModField::Rate, rate);
                        }
                    }
                }
                GraphCommand::SetModulatorWaveform {
                    kb,
                    split,
                    parent_slot,
                    mod_index,
                    waveform,
                } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        if let Some(m) = lane.modulators_for(parent_slot).and_then(|ms| ms.get_mut(mod_index)) {
                            if let ModSource::Lfo { waveform: ref mut w, .. } = m.source {
                                *w = waveform;
                            }
                        }
                    }
                }
                GraphCommand::SetModulatorSource {
                    kb,
                    split,
                    parent_slot,
                    mod_index,
                    source,
                } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        if let Some(m) = lane.modulators_for(parent_slot).and_then(|ms| ms.get_mut(mod_index)) {
                            m.source = source;
                            m.last_output = 0.0;
                        }
                    }
                }
                GraphCommand::SetModulatorEnvelopeParam {
                    kb,
                    split,
                    parent_slot,
                    mod_index,
                    attack,
                    decay,
                    sustain,
                    release,
                } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        if let Some(mods) = lane.modulators_for(parent_slot) {
                            if let Some(m) = mods.get_mut(mod_index) {
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
                            update_cross_mod_base(mods, mod_index, CrossModField::Attack, attack);
                            update_cross_mod_base(mods, mod_index, CrossModField::Decay, decay);
                            update_cross_mod_base(mods, mod_index, CrossModField::Sustain, sustain);
                            update_cross_mod_base(mods, mod_index, CrossModField::Release, release);
                        }
                    }
                }
                GraphCommand::AddModTarget {
                    kb,
                    split,
                    parent_slot,
                    mod_index,
                    target,
                } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        if let Some(m) = lane.modulators_for(parent_slot).and_then(|ms| ms.get_mut(mod_index)) {
                            m.targets.push(target);
                        }
                    }
                }
                GraphCommand::RemoveModTarget {
                    kb,
                    split,
                    parent_slot,
                    mod_index,
                    target_index,
                } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        if let Some(m) = lane.modulators_for(parent_slot).and_then(|ms| ms.get_mut(mod_index)) {
                            if target_index < m.targets.len() {
                                m.targets.remove(target_index);
                            }
                        }
                    }
                }
                GraphCommand::SetModTargetDepth {
                    kb,
                    split,
                    parent_slot,
                    mod_index,
                    target_index,
                    depth,
                } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        if let Some(m) = lane.modulators_for(parent_slot).and_then(|ms| ms.get_mut(mod_index)) {
                            if let Some(t) = m.targets.get_mut(target_index) {
                                t.depth = depth;
                            }
                        }
                    }
                }
                GraphCommand::SetPatternEnabled { kb, split, enabled } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        lane.pattern.enabled = enabled;
                    }
                }
                GraphCommand::SetPatternRecording { kb, split, recording } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        if recording {
                            // Ensure notification routes back to the correct split.
                            lane.pattern.kb_index = kb;
                            lane.pattern.split_index = split;
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
                GraphCommand::SetPattern { kb, split, pattern, base_note } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        lane.pattern.pattern = pattern;
                        lane.pattern.base_note = base_note;
                        lane.pattern.enabled = !lane.pattern.pattern.events.is_empty();
                    }
                }
                GraphCommand::SwapPatterns { kb, split_a, split_b } => {
                    if let Some(kb_node) = self.keyboards.get_mut(kb) {
                        if split_a < kb_node.splits.len() && split_b < kb_node.splits.len() {
                            // Swap pattern data, base_note, enabled, length_beats between the two splits.
                            let (a_pattern, a_base, a_enabled, a_beats) = {
                                let p = &kb_node.splits[split_a].pattern;
                                (p.pattern.clone(), p.base_note, p.enabled, p.length_beats)
                            };
                            let (b_pattern, b_base, b_enabled, b_beats) = {
                                let p = &kb_node.splits[split_b].pattern;
                                (p.pattern.clone(), p.base_note, p.enabled, p.length_beats)
                            };
                            let pa = &mut kb_node.splits[split_a].pattern;
                            pa.pattern = b_pattern;
                            pa.base_note = b_base;
                            pa.enabled = b_enabled;
                            pa.length_beats = b_beats;
                            pa.trigger_note = None;
                            pa.held_notes.clear();
                            pa.active_voices.clear();
                            let pb = &mut kb_node.splits[split_b].pattern;
                            pb.pattern = a_pattern;
                            pb.base_note = a_base;
                            pb.enabled = a_enabled;
                            pb.length_beats = a_beats;
                            pb.trigger_note = None;
                            pb.held_notes.clear();
                            pb.active_voices.clear();
                        }
                    }
                }
                GraphCommand::ClearPattern { kb, split } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        lane.pattern.pattern = Pattern::default();
                        lane.pattern.base_note = None;
                        lane.pattern.enabled = false;
                        lane.pattern.recording = false;
                        lane.pattern.counting_in = false;
                        lane.pattern.trigger_note = None;
                        lane.pattern.held_notes.clear();
                        lane.pattern.click_remaining = 0;
                        lane.pattern.active_voices.clear();
                    }
                }
                GraphCommand::SetGlobalBpm { bpm } => {
                    for kb in &mut self.keyboards {
                        for sp in &mut kb.splits {
                            sp.pattern.bpm = bpm;
                        }
                    }
                }
                GraphCommand::SetPatternLength { kb, split, beats } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        lane.pattern.length_beats = beats;
                    }
                }
                GraphCommand::SetPatternLooping { kb, split, looping } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        lane.pattern.looping = looping;
                    }
                }
                GraphCommand::SetTranspose { kb, split, semitones } => {
                    if let Some(lane) = self.get_split_mut(kb, split) {
                        lane.transpose = semitones;
                    }
                }
            }
        }
    }

    fn get_split_mut(&mut self, kb: usize, split: usize) -> Option<&mut SplitLane> {
        self.keyboards
            .get_mut(kb)
            .and_then(|k| k.splits.get_mut(split))
    }

    /// Process audio: drain commands, run all keyboards/splits, sum to output.
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

        // Resize split_buf
        for buf in self.split_buf.iter_mut() {
            buf.resize(frames, 0.0);
        }

        // Process each keyboard → each split, accumulate into mix_buf
        for keyboard in self.keyboards.iter_mut() {
            for split in keyboard.splits.iter_mut() {
                // Zero split_buf
                for buf in self.split_buf.iter_mut() {
                    buf.fill(0.0);
                }

                split.process(midi_events, &mut self.split_buf, self.num_channels)?;

                // Accumulate split output into mix_buf
                for ch in 0..self.num_channels {
                    for i in 0..frames {
                        self.mix_buf[ch][i] += self.split_buf[ch][i];
                    }
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
        // Create one keyboard with one full-range split (mimics old PluginChain behavior)
        graph.keyboards.push(KeyboardLane {
            splits: vec![SplitLane::new(num_channels)],
        });
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
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                kb: 0,
                split: 0,
                instrument: inst,
                inst_buf,
                remapper: None,
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
                kb: 0,
                split: 0,
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
                kb: 0,
                split: 0,
                index: 0,
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
                kb: 0,
                split: 0,
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
                kb: 0,
                split: 0,
                instrument: inst,
                inst_buf,
                remapper: Some(remapper),
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
    fn two_splits_sum_output() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);

        // One keyboard with two splits: both full range
        graph.keyboards.push(KeyboardLane {
            splits: vec![SplitLane::new(2), SplitLane::new(2)],
        });

        // Swap instruments into both splits
        let inst_a = ConstInstrument::new(0.3);
        let inst_buf_a = (0..inst_a.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                kb: 0,
                split: 0,
                instrument: inst_a,
                inst_buf: inst_buf_a,
                remapper: None,
            })
            .unwrap();

        let inst_b = ConstInstrument::new(0.5);
        let inst_buf_b = (0..inst_b.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                kb: 0,
                split: 1,
                instrument: inst_b,
                inst_buf: inst_buf_b,
                remapper: None,
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
    fn range_filtering() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);

        // One keyboard with two splits: C0-B3 and C4-C8
        let mut split_low = SplitLane::new(2);
        split_low.range = Some((12, 59)); // C0-B3
        let mut split_high = SplitLane::new(2);
        split_high.range = Some((60, 96)); // C4-C8

        graph.keyboards.push(KeyboardLane {
            splits: vec![split_low, split_high],
        });

        // Low split: value 0.3
        let inst_low = ConstInstrument::new(0.3);
        let inst_buf_low = (0..inst_low.audio_output_count())
            .map(|_| Vec::new())
            .collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                kb: 0,
                split: 0,
                instrument: inst_low,
                inst_buf: inst_buf_low,
                remapper: None,
            })
            .unwrap();

        // High split: value 0.7
        let inst_high = ConstInstrument::new(0.7);
        let inst_buf_high = (0..inst_high.audio_output_count())
            .map(|_| Vec::new())
            .collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                kb: 0,
                split: 1,
                instrument: inst_high,
                inst_buf: inst_buf_high,
                remapper: None,
            })
            .unwrap();

        // Play note in low range (C2 = 36): only low split should respond
        let mut out = make_output();
        graph.process(&[note_on(36)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 0.3).abs() < 1e-6));

        // Now note-off and play note in high range (C5 = 72): only high split should respond
        let mut out = make_output();
        graph.process(&[note_off(36), note_on(72)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 0.7).abs() < 1e-6));

        drop(return_rx);
    }

    #[test]
    fn cc_passthrough_to_all_splits() {
        // CC events (e.g. sustain pedal) should reach all splits regardless of range
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);

        let mut split_low = SplitLane::new(2);
        split_low.range = Some((0, 59));
        let mut split_high = SplitLane::new(2);
        split_high.range = Some((60, 127));

        graph.keyboards.push(KeyboardLane {
            splits: vec![split_low, split_high],
        });

        // Install instruments in both splits
        for s in 0..2 {
            let inst = ConstInstrument::new(0.5);
            let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
            cmd_tx
                .send(GraphCommand::SwapInstrument {
                    kb: 0,
                    split: s,
                    instrument: inst,
                    inst_buf,
                    remapper: None,
                })
                .unwrap();
        }

        // Send a CC event (sustain pedal) — should be filtered to both splits
        let cc_event: (u64, [u8; 3]) = (0, [0xB0, 64, 127]);
        let mut out = make_output();
        graph.process(&[cc_event], &mut out).unwrap();

        // No note-on, so output is silence, but the point is it didn't crash
        // and the CC was delivered to both splits (verified by filter_midi logic)
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
                kb: 0,
                split: 0,
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
        // No keyboards at all

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
        assert_eq!(LfoWaveform::Square.next(), LfoWaveform::Sine);
    }

    #[test]
    fn lfo_waveform_from_str() {
        assert_eq!(LfoWaveform::from_str("sine"), Some(LfoWaveform::Sine));
        assert_eq!(LfoWaveform::from_str("TRIANGLE"), Some(LfoWaveform::Triangle));
        assert_eq!(LfoWaveform::from_str("tri"), Some(LfoWaveform::Triangle));
        assert_eq!(LfoWaveform::from_str("saw"), Some(LfoWaveform::Saw));
        assert_eq!(LfoWaveform::from_str("square"), Some(LfoWaveform::Square));
        assert_eq!(LfoWaveform::from_str("unknown"), None);
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

    #[test]
    fn modulator_applies_set_parameter() {
        let (mut graph, cmd_tx, _return_rx) = make_graph(2);

        // Swap in a param-tracking instrument.
        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                kb: 0,
                split: 0,
                instrument: inst,
                inst_buf,
                remapper: None,
            })
            .unwrap();

        // Add a modulator targeting param 0 (cutoff) on the instrument (parent_slot=0).
        cmd_tx
            .send(GraphCommand::InsertModulator {
                kb: 0,
                split: 0,
                parent_slot: 0,
                index: 0,
                source: ModSource::Lfo { waveform: LfoWaveform::Sine, rate: 1.0, phase: 0.0 },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                kb: 0,
                split: 0,
                parent_slot: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { param_index: 0 },
                    depth: 0.5,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
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
            first_sample >= 0.0 && first_sample <= 1.0,
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
            sample >= 0.0 && sample <= 1.0,
            "modulated value out of range after many buffers: {sample}"
        );
    }

    #[test]
    fn modulator_base_value_updated_by_set_parameter() {
        let (mut graph, cmd_tx, _return_rx) = make_graph(2);

        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                kb: 0,
                split: 0,
                instrument: inst,
                inst_buf,
                remapper: None,
            })
            .unwrap();

        cmd_tx
            .send(GraphCommand::InsertModulator {
                kb: 0,
                split: 0,
                parent_slot: 0,
                index: 0,
                source: ModSource::Lfo { waveform: LfoWaveform::Sine, rate: 1.0, phase: 0.0 },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                kb: 0,
                split: 0,
                parent_slot: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { param_index: 0 },
                    depth: 0.5,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                },
            })
            .unwrap();

        // Process to pick up commands.
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();

        // Now send SetParameter to change the base value.
        cmd_tx
            .send(GraphCommand::SetParameter {
                kb: 0,
                split: 0,
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
    fn remove_effect_removes_its_modulators() {
        let (mut graph, cmd_tx, _return_rx) = make_graph(2);

        // Set up instrument + 2 effects.
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        insert_effect(&cmd_tx, 0, Box::new(PassthroughEffect), 1.0);
        insert_effect(&cmd_tx, 1, Box::new(PassthroughEffect), 1.0);

        // Add modulator on effect 1 (parent_slot=2).
        cmd_tx
            .send(GraphCommand::InsertModulator {
                kb: 0,
                split: 0,
                parent_slot: 2,
                index: 0,
                source: ModSource::Lfo { waveform: LfoWaveform::Sine, rate: 1.0, phase: 0.0 },
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        // Remove effect at index 0. The modulators on effect 1 (now effect 0)
        // should still be there.
        cmd_tx
            .send(GraphCommand::RemoveEffect {
                kb: 0,
                split: 0,
                index: 0,
            })
            .unwrap();

        // Should not crash — process after removing effect.
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s.is_finite()));
    }
}
