//! The session model: the main thread's mirror of what the audio graph holds.
//!
//! One `InstrumentNode` per lane (instrument slot, effects, lane modulators,
//! pattern, group membership) and one `GroupNode` per submix group (bus effects,
//! group modulators). Front-ends edit it alongside the audio graph: every change
//! is applied to both, the model is what gets saved, and the helpers at the end
//! of this file keep the model's indices in step with the audio thread's own
//! fix-ups (`shift_slots_after_insert`, `fixup_group_member_after_remove`, ...
//! in `plugin::chain`) when lanes, effects, modulators or members come and go.
//!
//! Nothing here knows about the terminal: the TUI imports it today, and the
//! controller and the web projection planned in `docs/web-ui.md` build on it.

use crate::plugin;

/// Default dry/wet mix for a newly added effect. Half-wet suits blend/parallel
/// effects (reverb, delay), the common DAW default for sends.
pub const DEFAULT_EFFECT_MIX: f32 = 0.5;

/// Default dry/wet mix for a freshly added effect, by plugin. Filters and
/// other tone-shaping inserts must be fully wet — at 50% the dry path passes
/// the very frequencies the filter removes, so e.g. a highpass barely changes
/// the sound. Blend effects (reverb) keep the half-wet default.
pub fn default_effect_mix(id: &str) -> f32 {
    if id == "builtin:filter" {
        1.0
    } else {
        DEFAULT_EFFECT_MIX
    }
}

// ---------------------------------------------------------------------------
// Plugin slot — main-thread mirror of what the audio thread has
// ---------------------------------------------------------------------------

pub struct PluginSlot {
    pub name: String,
    pub format: String,
    #[allow(dead_code)]
    pub id: String,
    #[allow(dead_code)]
    pub is_instrument: bool,
    pub params: Vec<ParamSlot>,
    /// All presets the plugin reports, cached at load time. Used by the
    /// preset selector popup.
    pub presets: Vec<plugin::Preset>,
    /// Name of the currently-loaded preset (if any). Tracked for save
    /// round-trip and display.
    pub current_preset: Option<String>,
    /// Effect dry/wet mix (1.0 = full wet). Only meaningful for effect slots;
    /// instrument slots leave this at 1.0 and don't read it.
    pub mix: f32,
}

pub enum ParamKind {
    Float,
    Enum(Vec<String>),
    Separator,
}

pub struct ParamSlot {
    pub name: String,
    pub index: u32,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub value: f32,
    pub kind: ParamKind,
}

// ---------------------------------------------------------------------------
// Lanes and groups
// ---------------------------------------------------------------------------

/// Main-thread mirror of pattern state for an instrument.
pub struct PatternState {
    pub bpm: f32,
    pub length_beats: f32,
    pub looping: bool,
    pub base_note: Option<u8>,
    pub events: Vec<(u64, u8, u8, u8)>, // (frame, status, note, velocity)
    pub enabled: bool,
    pub recording: bool,
    /// Transpose playback by scale degrees (in-key) instead of semitones.
    pub in_key: bool,
}

pub struct InstrumentNode {
    pub range: Option<(u8, u8)>,
    pub transpose: i8,
    /// Host-side output gain, applied before effects (1.0 = unity).
    pub volume: f32,
    pub instrument: Option<PluginSlot>,
    pub effects: Vec<PluginSlot>,
    /// Lane-scoped modulators. Each target addresses a plugin in this chain by
    /// slot (0 = instrument, 1..N = effects).
    pub modulators: Vec<ModulatorSlot>,
    pub pattern: Option<PatternState>,
    /// Carried through from session config; not editable in the UI.
    pub pitch_bend_range: f64,
    pub remap: std::collections::HashMap<String, crate::session::RemapTarget>,
    /// Submix group membership: index into the session's groups, or None.
    pub group: Option<usize>,
}

/// A submix group: its members are the instruments whose `group` points here;
/// it sums them and runs its own effect chain + volume.
pub struct GroupNode {
    pub name: Option<String>,
    pub volume: f32,
    /// Stereo balance pan for the group bus (-1 left … 0 center … +1 right).
    pub pan: f32,
    pub effects: Vec<PluginSlot>,
    /// Group-scoped modulators. Each target addresses a member instrument
    /// (`GroupMember`), a bus effect (`GroupBus`), or a sibling group modulator.
    pub modulators: Vec<ModulatorSlot>,
}

pub enum ModSourceSlot {
    Lfo {
        waveform: crate::plugin::chain::LfoWaveform,
        rate: f32,
    },
    Envelope {
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
    /// A MIDI-learned control (CC or pitch bend). `source` is None while
    /// `learning` (armed for the next incoming control).
    MidiLearn {
        source: Option<crate::plugin::chain::MidiBindSource>,
        learning: bool,
    },
}

impl ModSourceSlot {
    /// A short label for the source (used in the tree and param pane).
    pub fn label(&self) -> String {
        use crate::plugin::chain::MidiBindSource;
        match self {
            ModSourceSlot::Lfo { waveform, rate } => format!("LFO {:.1}Hz {}", rate, waveform.name()),
            ModSourceSlot::Envelope { .. } => "ADSR".to_string(),
            ModSourceSlot::MidiLearn { source, learning } => match source {
                Some(MidiBindSource::Cc(n)) => format!("CC {n}"),
                Some(MidiBindSource::PitchBend) => "Pitch Bend".to_string(),
                Some(MidiBindSource::Aftertouch) => "Aftertouch".to_string(),
                None if *learning => "MIDI (learning…)".to_string(),
                None => "MIDI (unbound)".to_string(),
            },
        }
    }
}

pub struct ModulatorSlot {
    pub source: ModSourceSlot,
    pub targets: Vec<ModTargetSlot>,
}

pub struct ModTargetSlot {
    /// Slot the target lives on: 0 = instrument, 1..N = effects. Used to label
    /// the target with its plugin and to resolve cross-mod vs plugin params.
    pub slot: usize,
    pub param_name: String,
    pub kind: crate::plugin::chain::ModTargetKind,
    pub depth: f32,
    #[allow(dead_code)]
    pub param_min: f32,
    #[allow(dead_code)]
    pub param_max: f32,
}

// ---------------------------------------------------------------------------
// Plugin ids
// ---------------------------------------------------------------------------

pub fn format_from_id(id: &str) -> String {
    if id.starts_with("builtin:") {
        "Built-in".into()
    } else if id.starts_with("lv2:") || id.starts_with("http://") || id.starts_with("urn:") {
        "LV2".into()
    } else if id.starts_with("clap:") || id.contains('.') && !id.contains('/') {
        "CLAP".into()
    } else if id.starts_with("vst3:") {
        "VST3".into()
    } else if id.ends_with(".lv2") {
        "LV2".into()
    } else if id.ends_with(".clap") {
        "CLAP".into()
    } else if id.ends_with(".vst3") {
        "VST3".into()
    } else {
        "?".into()
    }
}

// ---------------------------------------------------------------------------
// Modulator rows: how a modulator's source and targets map to parameter rows
// ---------------------------------------------------------------------------

/// Depth range for a modulator target. Negative depth **inverts** the
/// modulation direction — e.g. a control that lowers the parameter instead of
/// raising it, an LFO wobbling in anti-phase, or an envelope that dips below
/// the base. The audio-side math is a plain multiply, so this is only the
/// editor's bound; sessions already round-trip negative depth.
pub const DEPTH_MIN: f32 = -1.0;
pub const DEPTH_MAX: f32 = 1.0;

/// Number of param-pane rows for a modulator: source params + "Targets"
/// separator + one row per target. Scope-independent (lane or group).
pub fn modulator_param_len(m: &ModulatorSlot) -> usize {
    let fixed = match &m.source {
        ModSourceSlot::Lfo { .. } => 3,      // Type + Waveform + Rate
        ModSourceSlot::Envelope { .. } => 5, // Type + A + D + S + R
        ModSourceSlot::MidiLearn { .. } => 1, // Source-info row only
    };
    fixed + 1 + m.targets.len()
}

/// (min, max) for a modulator pseudo-param at row `pa`, or None for enum /
/// separator rows. Scope-independent.
pub fn modulator_param_range(m: &ModulatorSlot, pa: usize) -> Option<(f32, f32)> {
    if pa == 0 {
        return None; // Type enum
    }
    match &m.source {
        ModSourceSlot::Lfo { .. } => match pa {
            1 => None, // Waveform enum
            2 => Some((0.01, 50.0)),
            3 => None, // Separator
            _ => m.targets.get(pa - 4).map(|_| (DEPTH_MIN, DEPTH_MAX)),
        },
        ModSourceSlot::Envelope { .. } => match pa {
            1 => Some((0.001, 10.0)),
            2 => Some((0.001, 10.0)),
            3 => Some((0.0, 1.0)),
            4 => Some((0.001, 10.0)),
            5 => None, // Separator
            _ => m.targets.get(pa - 6).map(|_| (DEPTH_MIN, DEPTH_MAX)),
        },
        // Row 0 = source info (handled above), 1 = Targets separator, 2+ = depths.
        ModSourceSlot::MidiLearn { .. } => match pa {
            1 => None,
            _ => m.targets.get(pa - 2).map(|_| (DEPTH_MIN, DEPTH_MAX)),
        },
    }
}

/// True if a modulator pseudo-param at row `pa` is an enum (Type / Waveform).
pub fn modulator_param_is_enum(m: &ModulatorSlot, pa: usize) -> bool {
    match &m.source {
        ModSourceSlot::Lfo { .. } => pa == 0 || pa == 1, // Type, Waveform
        ModSourceSlot::Envelope { .. } => pa == 0,       // Type
        ModSourceSlot::MidiLearn { .. } => false,        // source row is read-only info
    }
}

/// Map a modulator's param-pane row `pa` to a cross-mod target on that
/// modulator (index `mod_index`): its rate/ADSR field or one of its target
/// depths. Returns `(kind, min, max, base, label)`, or None for enum/separator
/// rows. Used by MIDI-learn to bind a control to a modulator's own parameter.
pub fn modulator_row_to_cross_mod(
    m: &ModulatorSlot,
    mod_index: usize,
    pa: usize,
) -> Option<(crate::plugin::chain::ModTargetKind, f32, f32, f32, String)> {
    use crate::plugin::chain::ModTargetKind as K;
    let depth_at = |off: usize| {
        let ti = pa - off;
        m.targets
            .get(ti)
            .map(|t| (K::ModulatorDepth { mod_index, target_index: ti }, DEPTH_MIN, DEPTH_MAX, t.depth, format!("Mod {mod_index} depth {ti}")))
    };
    match &m.source {
        ModSourceSlot::Lfo { rate, .. } => match pa {
            2 => Some((K::ModulatorRate { mod_index }, 0.01, 50.0, *rate, format!("Mod {mod_index} rate"))),
            _ if pa >= 4 => depth_at(4),
            _ => None,
        },
        ModSourceSlot::Envelope { attack, decay, sustain, release } => match pa {
            1 => Some((K::ModulatorAttack { mod_index }, 0.001, 10.0, *attack, format!("Mod {mod_index} attack"))),
            2 => Some((K::ModulatorDecay { mod_index }, 0.001, 10.0, *decay, format!("Mod {mod_index} decay"))),
            3 => Some((K::ModulatorSustain { mod_index }, 0.0, 1.0, *sustain, format!("Mod {mod_index} sustain"))),
            4 => Some((K::ModulatorRelease { mod_index }, 0.001, 10.0, *release, format!("Mod {mod_index} release"))),
            _ if pa >= 6 => depth_at(6),
            _ => None,
        },
        ModSourceSlot::MidiLearn { .. } if pa >= 2 => depth_at(2),
        ModSourceSlot::MidiLearn { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// Index fix-ups and audio-graph conversion
// ---------------------------------------------------------------------------

/// After removing a modulator at `removed_index` from the session model,
/// clean up cross-mod targets in siblings: remove targets pointing at the
/// removed index, and decrement indices > removed_index.
pub fn fixup_cross_mod_after_remove(modulators: &mut [ModulatorSlot], removed_index: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            let idx = match &t.kind {
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
            };
            idx != Some(removed_index)
        });
        for t in &mut m.targets {
            let idx = match &mut t.kind {
                ModTargetKind::PluginParam { .. }
                | ModTargetKind::GroupMember { .. }
                | ModTargetKind::GroupBus { .. }
                | ModTargetKind::GroupMemberMod { .. } => continue,
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
    }
}

/// Mirror of the audio-thread `fixup_group_bus_after_remove` for the session model:
/// after a group bus effect at `removed` is deleted, drop GroupBus targets on
/// it and shift higher bus indices down.
pub fn fixup_group_bus_after_remove(modulators: &mut [ModulatorSlot], removed: usize) {
    use crate::plugin::chain::ModTargetKind;
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

/// Session-model mirror of `fixup_group_member_after_remove`: after the member at
/// ordinal `removed` leaves a group, drop GroupMember targets on it and shift
/// higher ordinals down.
pub fn fixup_group_member_after_remove(modulators: &mut [ModulatorSlot], removed: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| match &t.kind {
            ModTargetKind::GroupMember { member, .. }
            | ModTargetKind::GroupMemberMod { member, .. } => *member != removed,
            _ => true,
        });
        for t in &mut m.targets {
            let member = match &mut t.kind {
                ModTargetKind::GroupMember { member, .. }
                | ModTargetKind::GroupMemberMod { member, .. } => member,
                _ => continue,
            };
            if *member > removed {
                *member -= 1;
            }
        }
    }
}

/// Session-model mirror of `shift_group_member_after_insert`: after a member joins
/// a group at ordinal `inserted`, bump group-target member ordinals at/after it.
pub fn shift_group_member_after_insert(modulators: &mut [ModulatorSlot], inserted: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            let member = match &mut t.kind {
                ModTargetKind::GroupMember { member, .. }
                | ModTargetKind::GroupMemberMod { member, .. } => member,
                _ => continue,
            };
            if *member >= inserted {
                *member += 1;
            }
        }
    }
}

/// Session-model mirror of `fixup_group_member_mod_after_remove`: after member
/// `member`'s lane modulator at `removed` is deleted, drop GroupMemberMod
/// targets on it and shift that member's higher mod-indices down.
pub fn fixup_group_member_mod_after_remove(
    modulators: &mut [ModulatorSlot],
    member: usize,
    removed: usize,
) {
    use crate::plugin::chain::ModTargetKind;
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

/// The (group, member-ordinal) of instrument `inst`, or None if ungrouped.
pub fn lane_member_ordinal(instruments: &[InstrumentNode], inst: usize) -> Option<(usize, usize)> {
    let group = instruments.get(inst)?.group?;
    let ordinal = instruments[..inst]
        .iter()
        .filter(|n| n.group == Some(group))
        .count();
    Some((group, ordinal))
}

/// Convert a `ModSourceSlot` to the audio-thread `ModSource` carried by `GraphCommand`s.
pub fn mod_source_slot_to_graph(slot: &ModSourceSlot) -> crate::plugin::chain::ModSource {
    match slot {
        ModSourceSlot::Lfo { waveform, rate } => crate::plugin::chain::ModSource::Lfo {
            waveform: *waveform,
            rate: *rate,
            phase: 0.0,
        },
        ModSourceSlot::Envelope { attack, decay, sustain, release } => crate::plugin::chain::ModSource::Envelope {
            attack: *attack,
            decay: *decay,
            sustain: *sustain,
            release: *release,
            state: crate::plugin::chain::EnvState::Idle,
            level: 0.0,
            notes_held: 0,
        },
        ModSourceSlot::MidiLearn { source, learning } => crate::plugin::chain::ModSource::MidiLearn {
            source: *source,
            value: 0.0,
            learning: *learning,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::chain::{CrossModField, LfoWaveform, MidiBindSource, ModSource, ModTargetKind as K};

    fn target(kind: K) -> ModTargetSlot {
        ModTargetSlot {
            slot: 0,
            param_name: "p".to_string(),
            kind,
            depth: 0.5,
            param_min: 0.0,
            param_max: 1.0,
        }
    }

    fn lfo(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            source: ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate: 2.0 },
            targets,
        }
    }

    fn adsr(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            source: ModSourceSlot::Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 },
            targets,
        }
    }

    fn midi(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            source: ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(74)), learning: false },
            targets,
        }
    }

    /// A target list as readable text (`ModTargetKind` has no `PartialEq`).
    fn kinds(m: &ModulatorSlot) -> Vec<String> {
        m.targets.iter().map(|t| format!("{:?}", t.kind)).collect()
    }

    /// One target of every kind that is *not* in the index space under test, to
    /// prove the fix-ups leave them alone.
    fn bystanders() -> Vec<ModTargetSlot> {
        vec![
            target(K::PluginParam { slot: 1, param_index: 7 }),
            target(K::GroupMember { member: 3, slot: 0, param_index: 1 }),
            target(K::GroupBus { effect_index: 2, param_index: 0 }),
            target(K::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Rate }),
        ]
    }

    #[test]
    fn cross_mod_fixup_drops_targets_on_the_removed_modulator_and_shifts_higher_ones() {
        // The list *after* modulator 0 was removed: every sibling target that
        // pointed at index 0 is gone, indices above 0 move down one.
        let mut targets = vec![
            target(K::ModulatorRate { mod_index: 0 }),
            target(K::ModulatorAttack { mod_index: 2 }),
            target(K::ModulatorDecay { mod_index: 1 }),
            target(K::ModulatorSustain { mod_index: 3 }),
            target(K::ModulatorRelease { mod_index: 0 }),
            target(K::ModulatorDepth { mod_index: 2, target_index: 4 }),
        ];
        targets.extend(bystanders());
        let mut mods = vec![lfo(targets), adsr(vec![target(K::ModulatorRate { mod_index: 1 })])];
        fixup_cross_mod_after_remove(&mut mods, 0);
        assert_eq!(
            kinds(&mods[0]),
            [
                "ModulatorAttack { mod_index: 1 }",
                "ModulatorDecay { mod_index: 0 }",
                "ModulatorSustain { mod_index: 2 }",
                "ModulatorDepth { mod_index: 1, target_index: 4 }",
                "PluginParam { slot: 1, param_index: 7 }",
                "GroupMember { member: 3, slot: 0, param_index: 1 }",
                "GroupBus { effect_index: 2, param_index: 0 }",
                "GroupMemberMod { member: 0, mod_index: 0, field: Rate }",
            ]
        );
        assert_eq!(kinds(&mods[1]), ["ModulatorRate { mod_index: 0 }"], "other racks' targets shift too");
    }

    #[test]
    fn cross_mod_fixup_removing_the_last_modulator_leaves_lower_indices_alone() {
        let mut mods = vec![lfo(vec![
            target(K::ModulatorRate { mod_index: 0 }),
            target(K::ModulatorRate { mod_index: 1 }),
            target(K::ModulatorRate { mod_index: 2 }),
        ])];
        fixup_cross_mod_after_remove(&mut mods, 2);
        assert_eq!(kinds(&mods[0]), ["ModulatorRate { mod_index: 0 }", "ModulatorRate { mod_index: 1 }"]);
    }

    #[test]
    fn group_bus_fixup_drops_targets_on_the_removed_effect_and_shifts_higher_ones() {
        let mut targets = vec![
            target(K::GroupBus { effect_index: 0, param_index: 1 }),
            target(K::GroupBus { effect_index: 1, param_index: 2 }),
            target(K::GroupBus { effect_index: 2, param_index: 3 }),
            target(K::PluginParam { slot: 2, param_index: 7 }),
            target(K::GroupMember { member: 1, slot: 2, param_index: 4 }),
        ];
        targets.push(target(K::ModulatorRate { mod_index: 2 }));
        let mut mods = vec![lfo(targets)];
        fixup_group_bus_after_remove(&mut mods, 1);
        assert_eq!(
            kinds(&mods[0]),
            [
                "GroupBus { effect_index: 0, param_index: 1 }",
                "GroupBus { effect_index: 1, param_index: 3 }",
                "PluginParam { slot: 2, param_index: 7 }",
                "GroupMember { member: 1, slot: 2, param_index: 4 }",
                "ModulatorRate { mod_index: 2 }",
            ]
        );
    }

    #[test]
    fn group_member_removal_fixup_drops_targets_on_the_member_and_shifts_higher_ordinals() {
        let mut targets = vec![
            target(K::GroupMember { member: 0, slot: 0, param_index: 1 }),
            target(K::GroupMember { member: 1, slot: 0, param_index: 2 }),
            target(K::GroupMember { member: 2, slot: 1, param_index: 3 }),
            target(K::GroupMemberMod { member: 1, mod_index: 0, field: CrossModField::Rate }),
            target(K::GroupMemberMod { member: 2, mod_index: 4, field: CrossModField::Depth(1) }),
        ];
        targets.push(target(K::GroupBus { effect_index: 1, param_index: 0 }));
        targets.push(target(K::ModulatorRate { mod_index: 1 }));
        let mut mods = vec![lfo(targets)];
        fixup_group_member_after_remove(&mut mods, 1);
        assert_eq!(
            kinds(&mods[0]),
            [
                "GroupMember { member: 0, slot: 0, param_index: 1 }",
                "GroupMember { member: 1, slot: 1, param_index: 3 }",
                "GroupMemberMod { member: 1, mod_index: 4, field: Depth(1) }",
                "GroupBus { effect_index: 1, param_index: 0 }",
                "ModulatorRate { mod_index: 1 }",
            ]
        );
    }

    #[test]
    fn group_member_insert_shift_bumps_ordinals_at_and_after_the_new_member() {
        let mut targets = vec![
            target(K::GroupMember { member: 0, slot: 0, param_index: 1 }),
            target(K::GroupMember { member: 1, slot: 0, param_index: 2 }),
            target(K::GroupMember { member: 2, slot: 1, param_index: 3 }),
            target(K::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Rate }),
            target(K::GroupMemberMod { member: 1, mod_index: 4, field: CrossModField::Release }),
        ];
        targets.push(target(K::GroupBus { effect_index: 1, param_index: 0 }));
        targets.push(target(K::ModulatorRate { mod_index: 1 }));
        let mut mods = vec![lfo(targets)];
        shift_group_member_after_insert(&mut mods, 1);
        assert_eq!(
            kinds(&mods[0]),
            [
                "GroupMember { member: 0, slot: 0, param_index: 1 }",
                "GroupMember { member: 2, slot: 0, param_index: 2 }",
                "GroupMember { member: 3, slot: 1, param_index: 3 }",
                "GroupMemberMod { member: 0, mod_index: 0, field: Rate }",
                "GroupMemberMod { member: 2, mod_index: 4, field: Release }",
                "GroupBus { effect_index: 1, param_index: 0 }",
                "ModulatorRate { mod_index: 1 }",
            ]
        );
        // Joining at the very front shifts everyone; joining after the last member shifts no one.
        shift_group_member_after_insert(&mut mods, 0);
        assert!(kinds(&mods[0])[0].starts_with("GroupMember { member: 1,"));
        let before = kinds(&mods[0]);
        shift_group_member_after_insert(&mut mods, 99);
        assert_eq!(kinds(&mods[0]), before);
    }

    #[test]
    fn group_member_mod_fixup_only_touches_that_members_modulators() {
        let mut mods = vec![lfo(vec![
            target(K::GroupMemberMod { member: 1, mod_index: 0, field: CrossModField::Rate }),
            target(K::GroupMemberMod { member: 1, mod_index: 1, field: CrossModField::Attack }),
            target(K::GroupMemberMod { member: 1, mod_index: 2, field: CrossModField::Depth(0) }),
            target(K::GroupMemberMod { member: 0, mod_index: 1, field: CrossModField::Rate }),
            target(K::GroupMemberMod { member: 0, mod_index: 3, field: CrossModField::Release }),
            target(K::GroupMember { member: 1, slot: 0, param_index: 5 }),
            target(K::ModulatorRate { mod_index: 1 }),
        ])];
        fixup_group_member_mod_after_remove(&mut mods, 1, 1);
        // Member 1's modulator 1 is gone and its modulator 2 became 1; member 0's
        // modulators (same index, and a higher one) and every other kind stay put.
        assert_eq!(
            kinds(&mods[0]),
            [
                "GroupMemberMod { member: 1, mod_index: 0, field: Rate }",
                "GroupMemberMod { member: 1, mod_index: 1, field: Depth(0) }",
                "GroupMemberMod { member: 0, mod_index: 1, field: Rate }",
                "GroupMemberMod { member: 0, mod_index: 3, field: Release }",
                "GroupMember { member: 1, slot: 0, param_index: 5 }",
                "ModulatorRate { mod_index: 1 }",
            ]
        );
    }

    fn node(group: Option<usize>) -> InstrumentNode {
        InstrumentNode {
            range: None,
            transpose: 0,
            volume: 1.0,
            instrument: None,
            effects: vec![],
            modulators: vec![],
            pattern: None,
            pitch_bend_range: 2.0,
            remap: Default::default(),
            group,
        }
    }

    #[test]
    fn lane_member_ordinal_counts_earlier_members_of_the_same_group() {
        let lanes = [node(None), node(Some(0)), node(Some(1)), node(Some(0)), node(None), node(Some(0))];
        let ordinals: Vec<_> = (0..lanes.len()).map(|i| lane_member_ordinal(&lanes, i)).collect();
        assert_eq!(
            ordinals,
            [None, Some((0, 0)), Some((1, 0)), Some((0, 1)), None, Some((0, 2))]
        );
        assert_eq!(lane_member_ordinal(&lanes, 99), None, "out of range is ungrouped");
    }

    #[test]
    fn mod_source_slot_converts_to_a_fresh_audio_source() {
        match mod_source_slot_to_graph(&ModSourceSlot::Lfo { waveform: LfoWaveform::Saw, rate: 3.5 }) {
            ModSource::Lfo { waveform, rate, phase } => {
                assert_eq!((waveform, rate, phase), (LfoWaveform::Saw, 3.5, 0.0));
            }
            other => panic!("{other:?}"),
        }
        let env = ModSourceSlot::Envelope { attack: 0.1, decay: 0.2, sustain: 0.3, release: 0.4 };
        match mod_source_slot_to_graph(&env) {
            ModSource::Envelope { attack, decay, sustain, release, state, level, notes_held } => {
                assert_eq!((attack, decay, sustain, release), (0.1, 0.2, 0.3, 0.4));
                assert_eq!((state, level, notes_held), (crate::plugin::chain::EnvState::Idle, 0.0, 0));
            }
            other => panic!("{other:?}"),
        }
        let learned = ModSourceSlot::MidiLearn { source: Some(MidiBindSource::PitchBend), learning: false };
        match mod_source_slot_to_graph(&learned) {
            ModSource::MidiLearn { source, value, learning } => {
                assert_eq!((source, value, learning), (Some(MidiBindSource::PitchBend), 0.0, false));
            }
            other => panic!("{other:?}"),
        }
        let armed = ModSourceSlot::MidiLearn { source: None, learning: true };
        assert!(matches!(
            mod_source_slot_to_graph(&armed),
            ModSource::MidiLearn { source: None, learning: true, .. }
        ));
    }

    #[test]
    fn source_labels_are_what_the_tree_shows() {
        let label = |s: ModSourceSlot| s.label();
        assert_eq!(label(ModSourceSlot::Lfo { waveform: LfoWaveform::Triangle, rate: 0.25 }), "LFO 0.2Hz triangle");
        assert_eq!(label(ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate: 12.0 }), "LFO 12.0Hz sine");
        assert_eq!(label(ModSourceSlot::Envelope { attack: 0.0, decay: 0.0, sustain: 0.0, release: 0.0 }), "ADSR");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(74)), learning: false }), "CC 74");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: Some(MidiBindSource::PitchBend), learning: false }), "Pitch Bend");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Aftertouch), learning: false }), "Aftertouch");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: None, learning: true }), "MIDI (learning…)");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: None, learning: false }), "MIDI (unbound)");
    }

    /// Row layouts of the modulator parameter pane, per source type:
    /// `(is_enum, range)` for each row.
    #[test]
    fn modulator_rows_per_source_type() {
        let depth = Some((DEPTH_MIN, DEPTH_MAX));
        let two = || vec![target(K::PluginParam { slot: 0, param_index: 0 }), target(K::PluginParam { slot: 0, param_index: 1 })];

        let m = lfo(two());
        assert_eq!(modulator_param_len(&m), 3 + 1 + 2);
        let rows: Vec<_> = (0..modulator_param_len(&m))
            .map(|r| (modulator_param_is_enum(&m, r), modulator_param_range(&m, r)))
            .collect();
        assert_eq!(
            rows,
            [
                (true, None),                   // Type
                (true, None),                   // Waveform
                (false, Some((0.01, 50.0))),    // Rate
                (false, None),                  // "Targets" separator
                (false, depth),
                (false, depth),
            ]
        );

        let m = adsr(two());
        assert_eq!(modulator_param_len(&m), 5 + 1 + 2);
        let rows: Vec<_> = (0..modulator_param_len(&m))
            .map(|r| (modulator_param_is_enum(&m, r), modulator_param_range(&m, r)))
            .collect();
        assert_eq!(
            rows,
            [
                (true, None),                    // Type
                (false, Some((0.001, 10.0))),    // Attack
                (false, Some((0.001, 10.0))),    // Decay
                (false, Some((0.0, 1.0))),       // Sustain
                (false, Some((0.001, 10.0))),    // Release
                (false, None),                   // "Targets" separator
                (false, depth),
                (false, depth),
            ]
        );

        let m = midi(two());
        assert_eq!(modulator_param_len(&m), 1 + 1 + 2);
        let rows: Vec<_> = (0..modulator_param_len(&m))
            .map(|r| (modulator_param_is_enum(&m, r), modulator_param_range(&m, r)))
            .collect();
        assert_eq!(
            rows,
            [
                (false, None), // read-only source info
                (false, None), // "Targets" separator
                (false, depth),
                (false, depth),
            ]
        );

        // Past the last row there is nothing to edit.
        assert_eq!(modulator_param_range(&m, modulator_param_len(&m)), None);
    }

    #[test]
    fn cross_mod_rows_agree_with_the_row_ranges() {
        // The two tables (`modulator_param_range` for editing, `modulator_row_to_cross_mod`
        // for MIDI-learn / `m`) are written separately; they must not drift apart.
        let two = || vec![target(K::PluginParam { slot: 0, param_index: 0 }), target(K::PluginParam { slot: 0, param_index: 1 })];
        for m in [lfo(two()), adsr(two()), midi(two())] {
            for row in 0..=modulator_param_len(&m) {
                let range = modulator_param_range(&m, row);
                let cross = modulator_row_to_cross_mod(&m, 3, row);
                match (range, cross) {
                    (None, None) => {}
                    (Some((lo, hi)), Some((_, min, max, _, _))) => assert_eq!((lo, hi), (min, max), "row {row}"),
                    (range, cross) => panic!("row {row}: range {range:?} vs cross-mod {:?}", cross.map(|c| c.4)),
                }
            }
        }
    }

    #[test]
    fn cross_mod_rows_name_the_field_and_carry_its_current_value() {
        let two = vec![
            ModTargetSlot { depth: 0.3, ..target(K::PluginParam { slot: 0, param_index: 0 }) },
            ModTargetSlot { depth: -0.6, ..target(K::PluginParam { slot: 0, param_index: 1 }) },
        ];
        let m = lfo(two);
        let (kind, _, _, base, label) = modulator_row_to_cross_mod(&m, 3, 2).unwrap();
        assert_eq!((format!("{kind:?}"), base, label.as_str()), ("ModulatorRate { mod_index: 3 }".to_string(), 2.0, "Mod 3 rate"));
        let (kind, _, _, base, label) = modulator_row_to_cross_mod(&m, 3, 5).unwrap();
        assert_eq!(
            (format!("{kind:?}"), base, label.as_str()),
            ("ModulatorDepth { mod_index: 3, target_index: 1 }".to_string(), -0.6, "Mod 3 depth 1")
        );
        // Type, waveform and the separator are not bindable.
        for row in [0, 1, 3] {
            assert!(modulator_row_to_cross_mod(&m, 3, row).is_none(), "row {row}");
        }

        let m = adsr(vec![]);
        let names: Vec<_> = (0..modulator_param_len(&m))
            .filter_map(|r| modulator_row_to_cross_mod(&m, 0, r))
            .map(|(kind, _, _, base, label)| (format!("{kind:?}"), base, label))
            .collect();
        assert_eq!(
            names,
            [
                ("ModulatorAttack { mod_index: 0 }".to_string(), 0.01, "Mod 0 attack".to_string()),
                ("ModulatorDecay { mod_index: 0 }".to_string(), 0.3, "Mod 0 decay".to_string()),
                ("ModulatorSustain { mod_index: 0 }".to_string(), 0.7, "Mod 0 sustain".to_string()),
                ("ModulatorRelease { mod_index: 0 }".to_string(), 0.5, "Mod 0 release".to_string()),
            ]
        );
    }

    #[test]
    fn filters_default_fully_wet_everything_else_half_wet() {
        assert_eq!(default_effect_mix("builtin:filter"), 1.0);
        assert_eq!(default_effect_mix("builtin:reverb"), DEFAULT_EFFECT_MIX);
        assert_eq!(default_effect_mix("lv2:http://calf.sourceforge.net/plugins/Reverb"), 0.5);
    }

    #[test]
    fn plugin_format_comes_from_the_id() {
        for (id, format) in [
            ("builtin:osc", "Built-in"),
            ("lv2:http://tytel.org/helm", "LV2"),
            ("http://tytel.org/helm", "LV2"),
            ("urn:example:plugin", "LV2"),
            ("clap:com.u-he.diva", "CLAP"),
            ("com.u-he.diva", "CLAP"),
            ("vst3:Pianoteq 9", "VST3"),
            ("./Reverb.lv2", "LV2"),
            ("./Synth.clap", "CLAP"),
            ("/Library/Audio/Plug-Ins/VST3/Pianoteq 9.vst3", "VST3"),
            ("mystery", "?"),
        ] {
            assert_eq!(format_from_id(id), format, "{id}");
        }
    }
}
