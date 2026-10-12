//! Loading a session: instantiate its plugins, tell the audio thread what to run
//! (over the command channel), and return the session model.
//!
//! One loader for every front-end: the TUI and `play` both go through
//! `load_session`, so a session means the same thing in all of them. It reports
//! progress through a callback instead of drawing anything, which is how the TUI
//! splash follows along.

use std::collections::HashMap;
use std::path::Path;

use crossbeam_channel::Sender;

use crate::app::model::{
    GroupNode, InstrumentNode, ModSourceSlot, ModTargetSlot, ModulatorSlot, ParamKind, ParamSlot,
    PatternState, PluginSlot, SessionModel, format_from_id, mod_source_slot_to_graph,
};
use crate::plugin::chain::GraphCommand;
use crate::{plugin, session};

/// What `load_session` is doing, for a front-end that wants to show progress
/// (the TUI splash). Only the plugin slots of instrument lanes are reported, as
/// before: `done` counts instruments and their effects, not group bus effects.
pub enum Progress<'a> {
    /// About to load this plugin, named as in the session file.
    Loading(&'a str),
    /// `done` plugin slots have been loaded so far.
    Loaded { done: usize },
}

/// A loaded plugin's parameters, as the loader tracks them.
struct Params {
    /// Every parameter the plugin reports, including the ones the model hides (a
    /// VST3's "(locked)" entries). Modulator targets resolve names against this.
    all: Vec<plugin::ParameterInfo>,
    /// Each parameter's post-preset value, before any session-file override: the
    /// baseline the model compares against to decide what needs saving.
    baselines: Vec<f32>,
    /// Each parameter's current value: the baseline plus the session's overrides.
    values: Vec<f32>,
}

impl Params {
    /// Read the values of a freshly loaded, preset-applied plugin.
    fn snapshot(plugin: &mut Box<dyn plugin::Plugin>) -> Self {
        let all = plugin.parameters();
        let baselines: Vec<f32> = all
            .iter()
            .map(|p| plugin.get_parameter(p.index).unwrap_or(p.default))
            .collect();
        Params { all, values: baselines.clone(), baselines }
    }
}

/// Apply the session's parameter overrides, in name order so the command stream is
/// the same every run. `send` is called with each parameter the plugin has and its new
/// value, which is also recorded in `params.values`. Returns the names the plugin does
/// not have.
fn apply_overrides(
    params: &mut Params,
    overrides: &HashMap<String, f64>,
    mut send: impl FnMut(&plugin::ParameterInfo, f32) -> anyhow::Result<()>,
) -> anyhow::Result<Vec<String>> {
    let mut names: Vec<&String> = overrides.keys().collect();
    names.sort();
    let mut unknown = Vec::new();
    for name in names {
        let value = overrides[name] as f32;
        match params.all.iter().find(|p| p.name == *name) {
            Some(info) => {
                send(info, value)?;
                if let Some(v) = params.values.get_mut(info.index as usize) {
                    *v = value;
                }
            }
            None => unknown.push(name.clone()),
        }
    }
    Ok(unknown)
}

fn names(params: &[plugin::ParameterInfo]) -> String {
    params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// The model's slot for a loaded plugin. Parameters a plugin marks "(locked)" are
/// hidden, and a parameter with value labels becomes an enum.
fn plugin_slot(
    name: String,
    id: String,
    is_instrument: bool,
    params: &Params,
    presets: Vec<plugin::Preset>,
    current_preset: Option<String>,
    mix: f32,
) -> PluginSlot {
    let slots = params
        .all
        .iter()
        .zip(&params.baselines)
        .zip(&params.values)
        .filter(|((p, _), _)| !p.name.starts_with("(locked)"))
        .map(|((p, &baseline), &value)| ParamSlot {
            kind: p.labels.clone().map_or(ParamKind::Float, ParamKind::Enum),
            name: p.name.clone(),
            index: p.index,
            min: p.min,
            max: p.max,
            default: baseline,
            value,
        })
        .collect();
    PluginSlot {
        name,
        format: format_from_id(&id),
        id,
        is_instrument,
        params: slots,
        presets,
        current_preset,
        mix,
    }
}

fn send(cmd_tx: &Sender<GraphCommand>, cmd: GraphCommand) -> anyhow::Result<()> {
    cmd_tx.send(cmd).map_err(|_| anyhow::anyhow!("command channel closed"))
}

/// Load `config`: instantiate every plugin, send the audio thread the commands
/// that build the graph (groups first, then lanes with their plugins, parameters,
/// modulators, transpose and pattern, then group modulators), and return the
/// session model. Blocks on the command channel when it is full, so the audio
/// thread must be (or soon be) draining it.
#[allow(clippy::too_many_arguments)]
pub fn load_session(
    config: &session::SessionConfig,
    session_dir: &Path,
    sample_rate: f32,
    max_block_size: usize,
    runtime: &plugin::Runtime,
    cmd_tx: &Sender<GraphCommand>,
    progress: &mut dyn FnMut(Progress),
) -> anyhow::Result<SessionModel> {
    let mut loaded_slots = 0usize;
    // The parameter lists modulator targets resolve names against: per lane (slot 0 =
    // the instrument, then its effects) and per group bus effect.
    let mut lane_params: Vec<Vec<Vec<plugin::ParameterInfo>>> = Vec::new();
    let mut bus_params: Vec<Vec<Vec<plugin::ParameterInfo>>> = Vec::new();

    // Set up submix groups first, so instruments can reference them by index.
    let mut groups: Vec<GroupNode> = Vec::new();
    for (g_idx, group_config) in config.groups.iter().enumerate() {
        send(cmd_tx, GraphCommand::AddGroup)?;
        if (group_config.volume - 1.0).abs() > f32::EPSILON {
            send(cmd_tx, GraphCommand::SetGroupVolume { group: g_idx, value: group_config.volume })?;
        }
        if group_config.pan.abs() > f32::EPSILON {
            send(cmd_tx, GraphCommand::SetGroupPan { group: g_idx, value: group_config.pan })?;
        }
        let mut group_effects: Vec<PluginSlot> = Vec::new();
        let mut group_bus_params = Vec::new();
        for (fx_idx, effect_config) in group_config.effects.iter().enumerate() {
            let effect_source = session::resolve_plugin_path(&effect_config.plugin, session_dir);
            let mut effect = plugin::load(&effect_source, sample_rate, max_block_size, runtime)?;
            if let Some(ref preset_name) = effect_config.preset {
                session::apply_preset(&mut effect, preset_name);
            }
            let presets = effect.presets();
            let mut params = Params::snapshot(&mut effect);
            let name = effect.name().to_string();
            send(
                cmd_tx,
                GraphCommand::InsertGroupEffect {
                    group: g_idx,
                    index: fx_idx,
                    effect,
                    mix: effect_config.mix,
                },
            )?;
            apply_overrides(&mut params, &effect_config.params, |info, value| {
                let _ = cmd_tx.send(GraphCommand::SetGroupParameter {
                    group: g_idx,
                    index: fx_idx,
                    param_index: info.index,
                    value,
                });
                Ok(())
            })?;
            group_effects.push(plugin_slot(
                name,
                effect_source,
                false,
                &params,
                presets,
                effect_config.preset.clone(),
                effect_config.mix as f32,
            ));
            group_bus_params.push(params.all);
        }
        bus_params.push(group_bus_params);
        groups.push(GroupNode {
            name: group_config.name.clone(),
            volume: group_config.volume,
            pan: group_config.pan,
            effects: group_effects,
            // Group modulators are loaded after the instruments, once member
            // chains are known (so member-target param names resolve).
            modulators: Vec::new(),
        });
    }

    // Set up the graph structure: add instrument lanes.
    let mut instruments: Vec<InstrumentNode> = Vec::new();
    for (inst_idx, inst_config) in config.instruments.iter().enumerate() {
        send(cmd_tx, GraphCommand::AddInstrument { range: inst_config.range })?;

        // Assign group membership (groups were created above).
        if inst_config.group.is_some() {
            send(cmd_tx, GraphCommand::SetLaneGroup { inst: inst_idx, group: inst_config.group })?;
        }

        // The chain's parameter lists, slot 0 first.
        let mut chain_params: Vec<Vec<plugin::ParameterInfo>> = Vec::new();

        // Load instrument (if present)
        let instrument_slot = if let Some(plug_config) = &inst_config.instrument {
            let instrument_source = session::resolve_plugin_path(&plug_config.plugin, session_dir);
            progress(Progress::Loading(&plug_config.plugin));
            let mut instrument =
                plugin::load(&instrument_source, sample_rate, max_block_size, runtime)?;
            loaded_slots += 1;
            progress(Progress::Loaded { done: loaded_slots });
            log::info!(
                "Loaded instrument for inst={}: {}",
                inst_idx,
                instrument.name()
            );

            if let Some(ref preset_name) = plug_config.preset {
                session::apply_preset(&mut instrument, preset_name);
            }
            let presets = instrument.presets();

            // Snapshot post-preset parameter values; these become the baseline the
            // model compares against to decide which params are user-edited and
            // need to be saved.
            let mut params = Params::snapshot(&mut instrument);

            // Build note remapper if configured
            let remapper = if plug_config.remap.is_empty() {
                None
            } else {
                let r = plugin::chain::NoteRemapper::from_config(
                    &plug_config.remap,
                    plug_config.pitch_bend_range,
                )?;
                log::info!(
                    "Note remapper: {} entries, pitch_bend_range=±{}",
                    plug_config.remap.len(),
                    plug_config.pitch_bend_range,
                );
                Some(r)
            };

            let name = instrument.name().to_string();
            let inst_buf = (0..instrument.audio_output_count())
                .map(|_| Vec::new())
                .collect();
            send(
                cmd_tx,
                GraphCommand::SwapInstrument { inst: inst_idx, instrument, inst_buf, remapper },
            )?;

            // Set volume if not default
            if (plug_config.volume - 1.0).abs() > f64::EPSILON {
                send(
                    cmd_tx,
                    GraphCommand::SetVolume { inst: inst_idx, value: plug_config.volume as f32 },
                )?;
            }

            // Send instrument parameter overrides. They start from the post-preset
            // baseline so values reflect the active preset's actual settings.
            let unknown = apply_overrides(&mut params, &plug_config.params, |info, value| {
                send(
                    cmd_tx,
                    GraphCommand::SetParameter {
                        inst: inst_idx,
                        slot: 0,
                        param_index: info.index,
                        value,
                    },
                )?;
                log::info!("Set instrument '{}' = {}", info.name, value);
                Ok(())
            })?;
            for name in unknown {
                log::warn!(
                    "Unknown instrument parameter '{}' (available: {})",
                    name,
                    names(&params.all)
                );
            }

            chain_params.push(params.all.clone());
            Some(plugin_slot(
                name,
                instrument_source,
                true,
                &params,
                presets,
                plug_config.preset.clone(),
                1.0,
            ))
        } else {
            chain_params.push(Vec::new());
            None
        };

        // Load effects
        let mut effects: Vec<PluginSlot> = Vec::new();
        for (fx_idx, effect_config) in inst_config.effects.iter().enumerate() {
            let effect_source = session::resolve_plugin_path(&effect_config.plugin, session_dir);
            progress(Progress::Loading(&effect_config.plugin));
            let mut effect = plugin::load(&effect_source, sample_rate, max_block_size, runtime)?;
            loaded_slots += 1;
            progress(Progress::Loaded { done: loaded_slots });
            log::info!(
                "Loaded effect for inst={} fx={}: {}",
                inst_idx,
                fx_idx,
                effect.name()
            );

            if let Some(ref preset_name) = effect_config.preset {
                session::apply_preset(&mut effect, preset_name);
            }
            let presets = effect.presets();

            // Snapshot post-preset values as the save-filter baseline.
            let mut params = Params::snapshot(&mut effect);
            let name = effect.name().to_string();

            send(
                cmd_tx,
                GraphCommand::InsertEffect {
                    inst: inst_idx,
                    index: fx_idx,
                    effect,
                    mix: effect_config.mix,
                },
            )?;

            // Send parameter overrides for this effect (slot = fx_idx + 1).
            // They start from the post-preset baseline.
            let unknown = apply_overrides(&mut params, &effect_config.params, |info, value| {
                send(
                    cmd_tx,
                    GraphCommand::SetParameter {
                        inst: inst_idx,
                        slot: fx_idx + 1,
                        param_index: info.index,
                        value,
                    },
                )?;
                log::info!("Set effect {} '{}' = {}", fx_idx, info.name, value);
                Ok(())
            })?;
            for name in unknown {
                log::warn!(
                    "Unknown parameter '{}' for effect {} (available: {})",
                    name,
                    fx_idx,
                    names(&params.all)
                );
            }

            chain_params.push(params.all.clone());
            effects.push(plugin_slot(
                name,
                effect_source,
                false,
                &params,
                presets,
                effect_config.preset.clone(),
                effect_config.mix as f32,
            ));
        }

        // Build the lane's modulators now that every plugin (and its params)
        // is loaded, so targets can resolve against any slot in the chain.
        let modulators = load_lane_modulators(inst_config, &chain_params, inst_idx, cmd_tx)?;

        // Apply instrument transpose.
        if inst_config.transpose != 0 {
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetTranspose {
                inst: inst_idx,
                semitones: inst_config.transpose,
            });
        }

        // Load pattern if configured.
        let pattern = inst_config.pattern.as_ref().map(|p| {
            // Build Pattern and send to audio graph.
            let pattern_events: Vec<plugin::chain::PatternEvent> = p
                .events
                .iter()
                .map(|&(frame, status, note, vel)| plugin::chain::PatternEvent {
                    frame,
                    status,
                    note,
                    velocity: vel,
                })
                .collect();
            let beats_per_sec = p.bpm / 60.0;
            let length_samples = (p.length_beats / beats_per_sec * sample_rate) as u64;
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPattern {
                inst: inst_idx,
                pattern: plugin::chain::Pattern { events: pattern_events, length_samples },
                base_note: p.base_note,
                in_key: p.in_key,
            });
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetGlobalBpm { bpm: p.bpm });
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPatternLength {
                inst: inst_idx,
                beats: p.length_beats,
            });
            // The engine enables a pattern that has events as soon as it receives it, so
            // say what the session wants, including "off".
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPatternEnabled {
                inst: inst_idx,
                enabled: p.enabled,
            });
            if !p.looping {
                let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPatternLooping {
                    inst: inst_idx,
                    looping: false,
                });
            }
            PatternState {
                bpm: p.bpm,
                length_beats: p.length_beats,
                looping: p.looping,
                base_note: p.base_note,
                events: p.events.clone(),
                enabled: p.enabled,
                recording: false,
                in_key: p.in_key,
            }
        });

        let (pitch_bend_range, remap) = inst_config
            .instrument
            .as_ref()
            .map(|p| (p.pitch_bend_range, p.remap.clone()))
            .unwrap_or((2.0, Default::default()));
        let volume = inst_config
            .instrument
            .as_ref()
            .map_or(1.0, |p| p.volume as f32);
        instruments.push(InstrumentNode {
            range: inst_config.range,
            transpose: inst_config.transpose,
            volume,
            instrument: instrument_slot,
            effects,
            modulators,
            pattern,
            pitch_bend_range,
            remap,
            group: inst_config.group,
        });
        lane_params.push(chain_params);
    }

    // Now that all member instruments are loaded, build each group's
    // modulators (member-target param names resolve against the member chains).
    for (g_idx, group_config) in config.groups.iter().enumerate() {
        if group_config.modulators.is_empty() {
            continue;
        }
        // Member chains in ordinal order (members = lanes with group == g_idx).
        let member_chains: Vec<&[Vec<plugin::ParameterInfo>]> = instruments
            .iter()
            .zip(&lane_params)
            .filter(|(lane, _)| lane.group == Some(g_idx))
            .map(|(_, chain)| chain.as_slice())
            .collect();
        // Member modulators in ordinal order (for group→member-mod cross-mod).
        let member_mods: Vec<&[ModulatorSlot]> = instruments
            .iter()
            .filter(|lane| lane.group == Some(g_idx))
            .map(|lane| lane.modulators.as_slice())
            .collect();
        let mods = load_group_modulators(
            group_config,
            g_idx,
            &member_chains,
            &member_mods,
            &bus_params[g_idx],
            cmd_tx,
        )?;
        groups[g_idx].modulators = mods;
    }

    Ok(SessionModel { instruments, groups })
}

/// Load modulators from a plugin's config and send the commands to the audio thread.
/// Build the lane's modulators (lane-scoped) and send them to the audio
/// thread. Targets address plugins by chain slot (0 = instrument, 1..N =
/// effect). `chain_params[slot]` holds that plugin's parameters for name→index
/// resolution.
///
/// Migration: modulators come from the instrument's config (slot defaults to
/// the target's `slot`, i.e. instrument unless set) and — for older sessions —
/// from each effect's config (those target their own effect, so their slot is
/// forced to that effect, and their cross-mod sibling indices are offset by
/// where the effect's group lands in the merged lane list).
fn load_lane_modulators(
    inst_config: &session::InstrumentSlotConfig,
    chain_params: &[Vec<plugin::ParameterInfo>],
    inst_idx: usize,
    cmd_tx: &crossbeam_channel::Sender<plugin::chain::GraphCommand>,
) -> anyhow::Result<Vec<ModulatorSlot>> {
    // Gather modulator groups with their default slot (instrument = 0, each
    // effect = its slot) and their base index in the merged lane list.
    let mut groups: Vec<(&[session::ModulatorConfig], usize)> = Vec::new();
    if let Some(ref inst) = inst_config.instrument {
        groups.push((&inst.modulators, 0));
    }
    for (i, fx) in inst_config.effects.iter().enumerate() {
        groups.push((&fx.modulators, i + 1));
    }
    let mut bases = Vec::with_capacity(groups.len());
    let mut acc = 0usize;
    for (configs, _) in &groups {
        bases.push(acc);
        acc += configs.len();
    }

    let mut loaded = Vec::new();
    let mut lane_index = 0usize;
    for (gi, (mod_configs, default_slot)) in groups.iter().enumerate() {
        let group_base = bases[gi];
        let is_instrument_group = *default_slot == 0;
        for mod_config in mod_configs.iter() {
            let mod_idx = lane_index;
        let (source_slot, desc) = build_mod_source(mod_config);

        cmd_tx
            .send(plugin::chain::GraphCommand::InsertModulator {
                inst: inst_idx,
                index: mod_idx,
                source: mod_source_slot_to_graph(&source_slot),
            })
            .map_err(|_| anyhow::anyhow!("command channel closed"))?;

        let mut loaded_targets: Vec<ModTargetSlot> = Vec::new();
        for target_config in &mod_config.targets {
            // Determine the target kind and associated metadata.
            let (kind, slot, label, param_min, param_max, base_value) =
                if let Some(ref param_name) = target_config.param {
                    // Plugin parameter target. Instrument-group modulators
                    // honor the target's `slot` (instrument unless set);
                    // migrated effect modulators target their own effect.
                    let slot = if is_instrument_group { target_config.slot } else { *default_slot };
                    let param_info = chain_params
                        .get(slot)
                        .and_then(|ps| ps.iter().find(|p| p.name == *param_name));
                    let param_info = match param_info {
                        Some(p) => p,
                        None => {
                            log::warn!(
                                "Modulator target param '{param_name}' not found in slot {slot}",
                            );
                            continue;
                        }
                    };
                    (
                        plugin::chain::ModTargetKind::PluginParam { slot, param_index: param_info.index },
                        slot,
                        param_info.name.clone(),
                        param_info.min,
                        param_info.max,
                        param_info.default,
                    )
                } else if let Some(mi) = target_config.mod_rate {
                    let mi = mi + group_base;
                    (plugin::chain::ModTargetKind::ModulatorRate { mod_index: mi },
                     0, format!("Mod {mi} rate"), 0.01, 50.0, 1.0)
                } else if let Some(ref pair) = target_config.mod_depth {
                    let mi = pair.first().copied().unwrap_or(0) + group_base;
                    let ti = pair.get(1).copied().unwrap_or(0);
                    (plugin::chain::ModTargetKind::ModulatorDepth { mod_index: mi, target_index: ti },
                     0, format!("Mod {mi} depth {ti}"), 0.0, 1.0, 0.5)
                } else if let Some(mi) = target_config.mod_attack {
                    let mi = mi + group_base;
                    (plugin::chain::ModTargetKind::ModulatorAttack { mod_index: mi },
                     0, format!("Mod {mi} attack"), 0.001, 10.0, 0.01)
                } else if let Some(mi) = target_config.mod_decay {
                    let mi = mi + group_base;
                    (plugin::chain::ModTargetKind::ModulatorDecay { mod_index: mi },
                     0, format!("Mod {mi} decay"), 0.001, 10.0, 0.3)
                } else if let Some(mi) = target_config.mod_sustain {
                    let mi = mi + group_base;
                    (plugin::chain::ModTargetKind::ModulatorSustain { mod_index: mi },
                     0, format!("Mod {mi} sustain"), 0.0, 1.0, 0.7)
                } else if let Some(mi) = target_config.mod_release {
                    let mi = mi + group_base;
                    (plugin::chain::ModTargetKind::ModulatorRelease { mod_index: mi },
                     0, format!("Mod {mi} release"), 0.001, 10.0, 0.5)
                } else {
                    log::warn!("Modulator target has no param or mod_* field, skipping");
                    continue;
                };

            let target = plugin::chain::ModTarget {
                kind: kind.clone(),
                depth: target_config.depth as f32,
                base_value,
                param_min,
                param_max,
            };

            cmd_tx
                .send(plugin::chain::GraphCommand::AddModTarget {
                    inst: inst_idx,
                    mod_index: mod_idx,
                    target,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;

            loaded_targets.push(ModTargetSlot {
                slot,
                param_name: label.clone(),
                kind,
                depth: target_config.depth as f32,
                param_min,
                param_max,
            });

            log::info!("Modulator {mod_idx} target: '{label}' (slot {slot}) depth={}", target_config.depth);
        }

        loaded.push(ModulatorSlot {
            source: source_slot,
            targets: loaded_targets,
        });

        log::info!("Loaded lane modulator {mod_idx} for inst={inst_idx}: {desc}");
        lane_index += 1;
        }
    }
    Ok(loaded)
}

/// Build the model's source for a modulator config, with a description for the
/// log. The audio thread's `ModSource` is derived from it (`mod_source_slot_to_graph`).
/// Shared by the lane and group modulator loaders.
fn build_mod_source(mod_config: &session::ModulatorConfig) -> (ModSourceSlot, String) {
    match mod_config.mod_type.as_str() {
        "envelope" => {
            let source = ModSourceSlot::Envelope {
                attack: mod_config.attack as f32,
                decay: mod_config.decay as f32,
                sustain: mod_config.sustain as f32,
                release: mod_config.release as f32,
            };
            (source, "ADSR envelope".to_string())
        }
        "midi" => {
            let bind = if let Some(cc) = mod_config.midi_cc {
                Some(plugin::chain::MidiBindSource::Cc(cc))
            } else if mod_config.midi_pitch_bend {
                Some(plugin::chain::MidiBindSource::PitchBend)
            } else if mod_config.midi_aftertouch {
                Some(plugin::chain::MidiBindSource::Aftertouch)
            } else {
                None
            };
            // Unbound on load ⇒ re-arm so the next control binds.
            let source = ModSourceSlot::MidiLearn { source: bind, learning: bind.is_none() };
            let desc = match bind {
                Some(plugin::chain::MidiBindSource::Cc(n)) => format!("MIDI CC {n}"),
                Some(plugin::chain::MidiBindSource::PitchBend) => "MIDI pitch bend".to_string(),
                Some(plugin::chain::MidiBindSource::Aftertouch) => "MIDI aftertouch".to_string(),
                None => "MIDI (learning)".to_string(),
            };
            (source, desc)
        }
        _ => {
            // Default: LFO.
            let waveform = plugin::chain::LfoWaveform::from_str(&mod_config.waveform)
                .unwrap_or_else(|| {
                    log::warn!(
                        "Unknown waveform '{}', defaulting to sine",
                        mod_config.waveform
                    );
                    plugin::chain::LfoWaveform::Sine
                });
            let source = ModSourceSlot::Lfo { waveform, rate: mod_config.rate as f32 };
            let desc = format!("{} {:.1}Hz", waveform.name(), mod_config.rate);
            (source, desc)
        }
    }
}

/// Build a group's modulators and send them to the audio thread. Targets
/// address a member instrument's chain (`member` + `slot`), one of the group's
/// own bus effects (`bus`), or a sibling group modulator (`mod_*`).
/// `member_chains[ordinal][slot]` holds each member's plugin params (slot 0 =
/// instrument); `bus_params[i]` holds bus effect `i`'s params.
/// (base_value, min, max) for a member modulator's field, used to seed a
/// group→member-modulator cross-mod target on load. Falls back to per-field
/// defaults if the member modulator's type doesn't match the field.
fn member_mod_field_info(mm: &ModulatorSlot, field: plugin::chain::CrossModField) -> (f32, f32, f32) {
    use plugin::chain::CrossModField;
    match field {
        CrossModField::Rate => {
            let v = if let ModSourceSlot::Lfo { rate, .. } = &mm.source { *rate } else { 1.0 };
            (v, 0.01, 50.0)
        }
        CrossModField::Attack => {
            let v = if let ModSourceSlot::Envelope { attack, .. } = &mm.source { *attack } else { 0.01 };
            (v, 0.001, 10.0)
        }
        CrossModField::Decay => {
            let v = if let ModSourceSlot::Envelope { decay, .. } = &mm.source { *decay } else { 0.3 };
            (v, 0.001, 10.0)
        }
        CrossModField::Sustain => {
            let v = if let ModSourceSlot::Envelope { sustain, .. } = &mm.source { *sustain } else { 0.7 };
            (v, 0.0, 1.0)
        }
        CrossModField::Release => {
            let v = if let ModSourceSlot::Envelope { release, .. } = &mm.source { *release } else { 0.5 };
            (v, 0.001, 10.0)
        }
        CrossModField::Depth(ti) => {
            let v = mm.targets.get(ti).map(|t| t.depth).unwrap_or(0.5);
            (v, 0.0, 1.0)
        }
    }
}

fn load_group_modulators(
    group_config: &session::GroupConfig,
    g_idx: usize,
    member_chains: &[&[Vec<plugin::ParameterInfo>]],
    member_mods: &[&[ModulatorSlot]],
    bus_params: &[Vec<plugin::ParameterInfo>],
    cmd_tx: &crossbeam_channel::Sender<plugin::chain::GraphCommand>,
) -> anyhow::Result<Vec<ModulatorSlot>> {
    let mut loaded = Vec::new();
    for (mod_idx, mod_config) in group_config.modulators.iter().enumerate() {
        let (source_slot, desc) = build_mod_source(mod_config);
        cmd_tx
            .send(plugin::chain::GraphCommand::InsertGroupModulator {
                group: g_idx,
                index: mod_idx,
                source: mod_source_slot_to_graph(&source_slot),
            })
            .map_err(|_| anyhow::anyhow!("command channel closed"))?;

        let mut loaded_targets: Vec<ModTargetSlot> = Vec::new();
        for target_config in &mod_config.targets {
            let (kind, slot, label, param_min, param_max, base_value) =
                if let Some(member) = target_config.member {
                    if let Some(ref param_name) = target_config.param {
                        // Member instrument plugin parameter.
                        let slot = target_config.slot;
                        let param_info = member_chains
                            .get(member)
                            .and_then(|chain| chain.get(slot))
                            .and_then(|ps| ps.iter().find(|p| p.name == *param_name));
                        let Some(param_info) = param_info else {
                            log::warn!(
                                "Group modulator target param '{param_name}' not found (member {member}, slot {slot})"
                            );
                            continue;
                        };
                        (
                            plugin::chain::ModTargetKind::GroupMember {
                                member,
                                slot,
                                param_index: param_info.index,
                            },
                            slot,
                            param_info.name.clone(),
                            param_info.min,
                            param_info.max,
                            param_info.default,
                        )
                    } else {
                        // Cross-mod a member instrument's modulator (member + mod_*).
                        use plugin::chain::CrossModField;
                        let (field, mi) = if let Some(mi) = target_config.mod_rate {
                            (CrossModField::Rate, mi)
                        } else if let Some(mi) = target_config.mod_attack {
                            (CrossModField::Attack, mi)
                        } else if let Some(mi) = target_config.mod_decay {
                            (CrossModField::Decay, mi)
                        } else if let Some(mi) = target_config.mod_sustain {
                            (CrossModField::Sustain, mi)
                        } else if let Some(mi) = target_config.mod_release {
                            (CrossModField::Release, mi)
                        } else if let Some(ref pair) = target_config.mod_depth {
                            let mi = pair.first().copied().unwrap_or(0);
                            let ti = pair.get(1).copied().unwrap_or(0);
                            (CrossModField::Depth(ti), mi)
                        } else {
                            log::warn!("Group modulator member target has no param or mod_* field, skipping");
                            continue;
                        };
                        let mm = member_mods.get(member).and_then(|mods| mods.get(mi));
                        let Some(mm) = mm else {
                            log::warn!("Group member-mod target: member {member} has no modulator {mi}");
                            continue;
                        };
                        let (base, min, max) = member_mod_field_info(mm, field);
                        let field_name = match field {
                            CrossModField::Rate => "rate".to_string(),
                            CrossModField::Attack => "attack".to_string(),
                            CrossModField::Decay => "decay".to_string(),
                            CrossModField::Sustain => "sustain".to_string(),
                            CrossModField::Release => "release".to_string(),
                            CrossModField::Depth(ti) => format!("depth {ti}"),
                        };
                        (
                            plugin::chain::ModTargetKind::GroupMemberMod { member, mod_index: mi, field },
                            0,
                            format!("M{member} Mod{mi} {field_name}"),
                            min,
                            max,
                            base,
                        )
                    }
                } else if let Some(bus) = target_config.bus {
                    let Some(ref param_name) = target_config.param else {
                        log::warn!("Group modulator bus target without param, skipping");
                        continue;
                    };
                    let param_info = bus_params
                        .get(bus)
                        .and_then(|ps| ps.iter().find(|p| p.name == *param_name));
                    let Some(param_info) = param_info else {
                        log::warn!(
                            "Group modulator target param '{param_name}' not found (bus {bus})"
                        );
                        continue;
                    };
                    (
                        plugin::chain::ModTargetKind::GroupBus {
                            effect_index: bus,
                            param_index: param_info.index,
                        },
                        0,
                        param_info.name.clone(),
                        param_info.min,
                        param_info.max,
                        param_info.default,
                    )
                } else if let Some(mi) = target_config.mod_rate {
                    (plugin::chain::ModTargetKind::ModulatorRate { mod_index: mi },
                     0, format!("Mod {mi} rate"), 0.01, 50.0, 1.0)
                } else if let Some(ref pair) = target_config.mod_depth {
                    let mi = pair.first().copied().unwrap_or(0);
                    let ti = pair.get(1).copied().unwrap_or(0);
                    (plugin::chain::ModTargetKind::ModulatorDepth { mod_index: mi, target_index: ti },
                     0, format!("Mod {mi} depth {ti}"), 0.0, 1.0, 0.5)
                } else if let Some(mi) = target_config.mod_attack {
                    (plugin::chain::ModTargetKind::ModulatorAttack { mod_index: mi },
                     0, format!("Mod {mi} attack"), 0.001, 10.0, 0.01)
                } else if let Some(mi) = target_config.mod_decay {
                    (plugin::chain::ModTargetKind::ModulatorDecay { mod_index: mi },
                     0, format!("Mod {mi} decay"), 0.001, 10.0, 0.3)
                } else if let Some(mi) = target_config.mod_sustain {
                    (plugin::chain::ModTargetKind::ModulatorSustain { mod_index: mi },
                     0, format!("Mod {mi} sustain"), 0.0, 1.0, 0.7)
                } else if let Some(mi) = target_config.mod_release {
                    (plugin::chain::ModTargetKind::ModulatorRelease { mod_index: mi },
                     0, format!("Mod {mi} release"), 0.001, 10.0, 0.5)
                } else {
                    log::warn!("Group modulator target has no param/member/bus/mod_* field, skipping");
                    continue;
                };

            let target = plugin::chain::ModTarget {
                kind: kind.clone(),
                depth: target_config.depth as f32,
                base_value,
                param_min,
                param_max,
            };
            cmd_tx
                .send(plugin::chain::GraphCommand::AddGroupModTarget {
                    group: g_idx,
                    mod_index: mod_idx,
                    target,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;

            loaded_targets.push(ModTargetSlot {
                slot,
                param_name: label,
                kind,
                depth: target_config.depth as f32,
                param_min,
                param_max,
            });
        }

        loaded.push(ModulatorSlot {
            source: source_slot,
            targets: loaded_targets,
        });
        log::info!("Loaded group modulator {mod_idx} for group={g_idx}: {desc}");
    }
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::{DISABLED_PATTERN, GROUPS, LEGACY, MIDI, SETTINGS, load, try_load};

    #[test]
    fn plugin_slots_hide_locked_parameters_and_turn_labelled_ones_into_enums() {
        let info = |index: u32, name: &str, labels: Option<Vec<&str>>| plugin::ParameterInfo {
            index,
            name: name.to_string(),
            min: 0.0,
            max: 10.0,
            default: 5.0,
            labels: labels.map(|l| l.into_iter().map(String::from).collect()),
        };
        let params = Params {
            all: vec![
                info(0, "cutoff", None),
                info(1, "(locked) bypass", None),
                info(2, "mode", Some(vec!["low", "high"])),
            ],
            baselines: vec![1.0, 2.0, 0.0],
            values: vec![1.5, 2.0, 1.0],
        };
        let slot = plugin_slot(
            "Synth".to_string(),
            "vst3:Synth".to_string(),
            true,
            &params,
            vec![],
            Some("Init".to_string()),
            1.0,
        );
        assert_eq!((slot.name.as_str(), slot.format.as_str(), slot.id.as_str()), ("Synth", "VST3", "vst3:Synth"));
        assert!(slot.is_instrument);
        assert_eq!(slot.current_preset.as_deref(), Some("Init"));
        let rows: Vec<_> = slot.params.iter().map(|p| (p.index, p.name.as_str(), p.default, p.value)).collect();
        assert_eq!(rows, [(0, "cutoff", 1.0, 1.5), (2, "mode", 0.0, 1.0)], "the locked parameter is hidden");
        assert!(matches!(slot.params[0].kind, ParamKind::Float));
        assert!(matches!(&slot.params[1].kind, ParamKind::Enum(l) if l == &["low", "high"]));
        // The loader still resolves modulator targets against the full list.
        assert_eq!(params.all.len(), 3);
    }

    #[test]
    fn parameter_overrides_apply_in_name_order() {
        // Eight names make an accidentally sorted hash order a 1 in 40320 chance.
        let letters = ["h", "c", "f", "a", "g", "d", "b", "e"];
        let mut params = Params {
            all: letters
                .iter()
                .enumerate()
                .map(|(i, n)| plugin::ParameterInfo { index: i as u32, name: n.to_string(), ..Default::default() })
                .collect(),
            baselines: vec![0.0; 8],
            values: vec![0.0; 8],
        };
        let mut overrides: HashMap<String, f64> =
            letters.iter().enumerate().map(|(i, n)| (n.to_string(), i as f64 + 1.0)).collect();
        overrides.insert("zzz".to_string(), 9.0);
        let mut sent = Vec::new();
        let unknown = apply_overrides(&mut params, &overrides, |info, value| {
            sent.push((info.name.clone(), value));
            Ok(())
        })
        .unwrap();
        let names: Vec<&str> = sent.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a", "b", "c", "d", "e", "f", "g", "h"]);
        assert_eq!(unknown, ["zzz"]);
        // Each value lands at its parameter's index: "h" is index 0 and was given 1.0.
        assert_eq!(params.values, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    }

    #[test]
    fn basic_example_loads_one_lane_with_a_reverb() {
        let run = load(include_str!("../../examples/basic.toml"));
        assert_eq!(
            run.commands,
            [
                "AddInstrument range=None",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "InsertEffect inst=0 index=0 plugin=Reverb mix=0.250",
            ]
        );
        assert_eq!(run.progress, ["loading builtin:sine", "loaded 1", "loading builtin:reverb", "loaded 2"]);
        let lane = &run.model.instruments[0];
        assert_eq!(lane.instrument.as_ref().unwrap().name, "Oscillator");
        assert_eq!(lane.effects[0].name, "Reverb");
        assert_eq!(lane.effects[0].current_preset.as_deref(), Some("Arcadia Dream Hall"));
        assert_eq!(lane.effects[0].mix, 0.25);
    }

    #[test]
    fn split_example_loads_two_ranged_lanes_with_vibrato() {
        let run = load(include_str!("../../examples/split.toml"));
        assert_eq!(
            run.commands,
            [
                "AddInstrument range=Some((12, 59))",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "InsertModulator inst=0 index=0 source=Lfo { waveform: Sine, rate: 4.0, phase: 0.0 }",
                "AddModTarget inst=0 mod=0 kind=PluginParam { slot: 0, param_index: 1 } depth=0.040 base=0.000 range=[-24.000, 24.000]",
                "AddInstrument range=Some((60, 108))",
                "SwapInstrument inst=1 plugin=Oscillator bufs=2 remap=false",
                "InsertModulator inst=1 index=0 source=Lfo { waveform: Sine, rate: 7.0, phase: 0.0 }",
                "AddModTarget inst=1 mod=0 kind=PluginParam { slot: 0, param_index: 1 } depth=0.150 base=0.000 range=[-24.000, 24.000]",
            ]
        );
        assert_eq!(run.model.instruments.len(), 2);
        assert_eq!(run.model.instruments[0].range, Some((12, 59)));
        assert_eq!(run.model.instruments[1].modulators[0].targets[0].depth, 0.15);
    }

    #[test]
    fn groups_load_first_and_group_modulators_last() {
        let run = load(GROUPS);
        assert_eq!(
            run.commands,
            [
                // Groups exist before any lane refers to them, with their bus effects.
                "AddGroup",
                "SetGroupVolume group=0 value=0.800",
                "SetGroupPan group=0 value=-0.300",
                "InsertGroupEffect group=0 index=0 plugin=Reverb mix=0.600",
                "SetGroupParameter group=0 fx=0 param=1 value=0.500",
                // Lanes: members join their group, the third lane stays ungrouped.
                "AddInstrument range=Some((12, 59))",
                "SetLaneGroup inst=0 group=Some(0)",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "SetParameter inst=0 slot=0 param=0 value=1.000",
                "AddInstrument range=None",
                "SetLaneGroup inst=1 group=Some(0)",
                "SwapInstrument inst=1 plugin=Oscillator bufs=2 remap=false",
                "InsertEffect inst=1 index=0 plugin=Filter mix=1.000",
                "InsertModulator inst=1 index=0 source=Lfo { waveform: Sine, rate: 3.0, phase: 0.0 }",
                "AddInstrument range=None",
                "SwapInstrument inst=2 plugin=Oscillator bufs=2 remap=false",
                // Group modulators come last, once the members' chains are known. Two targets
                // never resolve and are skipped: `param` without member / bus, and a bus
                // that does not exist.
                "InsertGroupModulator group=0 index=0 source=Lfo { waveform: Triangle, rate: 0.5, phase: 0.0 }",
                "AddGroupModTarget group=0 mod=0 kind=GroupMember { member: 0, slot: 0, param_index: 1 } depth=0.400 base=0.000 range=[-24.000, 24.000]",
                "AddGroupModTarget group=0 mod=0 kind=GroupMember { member: 1, slot: 1, param_index: 0 } depth=-0.300 base=1.000 range=[0.000, 1.000]",
                "AddGroupModTarget group=0 mod=0 kind=GroupBus { effect_index: 0, param_index: 2 } depth=0.250 base=0.350 range=[0.000, 1.000]",
                "AddGroupModTarget group=0 mod=0 kind=GroupMemberMod { member: 1, mod_index: 0, field: Rate } depth=0.200 base=3.000 range=[0.010, 50.000]",
                "InsertGroupModulator group=0 index=1 source=Envelope { attack: 0.02, decay: 0.4, sustain: 0.5, release: 1.0, state: Idle, level: 0.0, notes_held: 0 }",
                "AddGroupModTarget group=0 mod=1 kind=ModulatorRate { mod_index: 0 } depth=0.300 base=1.000 range=[0.010, 50.000]",
                "AddGroupModTarget group=0 mod=1 kind=ModulatorDepth { mod_index: 0, target_index: 2 } depth=0.100 base=0.500 range=[0.000, 1.000]",
            ]
        );
        // Progress covers the lanes' plugin slots only; the group's bus effect is not counted.
        assert_eq!(
            run.progress,
            [
                "loading builtin:osc", "loaded 1", "loading builtin:osc", "loaded 2",
                "loading builtin:filter", "loaded 3", "loading builtin:osc", "loaded 4",
            ]
        );
        let group = &run.model.groups[0];
        assert_eq!((group.name.as_deref(), group.volume, group.pan), (Some("Pad"), 0.8, -0.3));
        assert_eq!(group.effects.len(), 1);
        assert_eq!(group.modulators.len(), 2);
        assert_eq!(group.modulators[0].targets.len(), 4, "two of the six targets were skipped");
        assert_eq!(run.model.instruments.iter().map(|l| l.group).collect::<Vec<_>>(), [Some(0), Some(0), None]);
    }

    #[test]
    fn each_groups_modulators_resolve_against_their_own_members() {
        // Group B's member 0 is lane 1 (the one with a filter), not lane 0.
        let run = load(
            r#"
[[group]]
name = "A"
[[group]]
name = "B"
[[group.modulator]]
[[group.modulator.target]]
member = 0
slot = 1
param = "cutoff"
depth = 0.4

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument]]
plugin = "builtin:osc"
group = 1
[[instrument.effect]]
plugin = "builtin:filter"
"#,
        );
        assert!(
            run.commands.contains(
                &"AddGroupModTarget group=1 mod=0 kind=GroupMember { member: 0, slot: 1, param_index: 0 } depth=0.400 base=1.000 range=[0.000, 1.000]".to_string()
            ),
            "{:#?}",
            run.commands
        );
        assert_eq!(run.model.groups[1].modulators[0].targets.len(), 1);
        assert!(run.model.groups[0].modulators.is_empty());
    }

    #[test]
    fn modulators_nested_under_effects_migrate_into_the_lane() {
        // Older sessions put modulators under an effect. They join the lane's rack after the
        // instrument's own, aimed at that effect (the target's own `slot` is ignored), and their
        // sibling indices are offset by where the effect's modulators start in the merged list.
        let run = load(LEGACY);
        assert_eq!(
            run.commands,
            [
                "AddInstrument range=None",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "InsertEffect inst=0 index=0 plugin=Filter mix=1.000",
                "InsertModulator inst=0 index=0 source=Lfo { waveform: Sine, rate: 2.0, phase: 0.0 }",
                "AddModTarget inst=0 mod=0 kind=PluginParam { slot: 0, param_index: 1 } depth=0.100 base=0.000 range=[-24.000, 24.000]",
                "InsertModulator inst=0 index=1 source=Lfo { waveform: Sine, rate: 3.0, phase: 0.0 }",
                "AddModTarget inst=0 mod=1 kind=PluginParam { slot: 1, param_index: 0 } depth=0.200 base=1.000 range=[0.000, 1.000]",
                "InsertModulator inst=0 index=2 source=Lfo { waveform: Sine, rate: 4.0, phase: 0.0 }",
                "AddModTarget inst=0 mod=2 kind=ModulatorRate { mod_index: 1 } depth=0.300 base=1.000 range=[0.010, 50.000]",
            ]
        );
        let lane = &run.model.instruments[0];
        assert_eq!(lane.modulators.len(), 3);
        assert_eq!(lane.modulators[1].targets[0].slot, 1);
    }

    #[test]
    fn midi_modulators_rearm_for_learning_when_unbound() {
        let run = load(MIDI);
        assert_eq!(
            run.commands,
            [
                "AddInstrument range=None",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "InsertModulator inst=0 index=0 source=MidiLearn { source: Some(Cc(74)), value: 0.0, learning: false }",
                "AddModTarget inst=0 mod=0 kind=PluginParam { slot: 0, param_index: 1 } depth=0.500 base=0.000 range=[-24.000, 24.000]",
                "InsertModulator inst=0 index=1 source=MidiLearn { source: None, value: 0.0, learning: true }",
                "InsertModulator inst=0 index=2 source=MidiLearn { source: Some(PitchBend), value: 0.0, learning: false }",
                "InsertModulator inst=0 index=3 source=MidiLearn { source: Some(Aftertouch), value: 0.0, learning: false }",
            ]
        );
    }

    #[test]
    fn lane_and_effect_settings_become_commands_in_chain_order() {
        let run = load(SETTINGS);
        assert_eq!(
            run.commands,
            [
                "AddInstrument range=Some((36, 83))",
                // Instrument: remapper present, host volume, parameter overrides (the unknown one is dropped).
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=true",
                "SetVolume inst=0 value=0.500",
                "SetParameter inst=0 slot=0 param=0 value=2.000",
                // Effects, each followed by its own parameter overrides (preset applied before insertion).
                "InsertEffect inst=0 index=0 plugin=Reverb mix=0.400",
                "SetParameter inst=0 slot=1 param=1 value=0.900",
                "InsertEffect inst=0 index=1 plugin=Filter mix=1.000",
                "SetParameter inst=0 slot=2 param=0 value=0.250",
                // Then transpose and the pattern (120000 / 90 bpm * 2 beats at 48 kHz = 64000 frames).
                "SetTranspose inst=0 semitones=-12",
                "SetPattern inst=0 events=2 length_samples=64000 base_note=Some(60) in_key=true",
                "SetGlobalBpm bpm=90.0",
                "SetPatternLength inst=0 beats=2.0",
                "SetPatternEnabled inst=0 enabled=true",
                "SetPatternLooping inst=0 looping=false",
                // A lane with no plugin is still a lane.
                "AddInstrument range=None",
            ]
        );
        let lane = &run.model.instruments[0];
        assert_eq!((lane.transpose, lane.volume, lane.pitch_bend_range), (-12, 0.5, 4.0));
        assert!(lane.remap.contains_key("G#4"));
        assert!(run.model.instruments[1].instrument.is_none());
        let osc = lane.instrument.as_ref().unwrap();
        let waveform = osc.params.iter().find(|p| p.name == "waveform").unwrap();
        assert_eq!(waveform.value, 2.0, "the override is the value");
        assert_eq!(waveform.default, 0.0, "the plugin's own value stays the baseline");
        assert!(matches!(&waveform.kind, ParamKind::Enum(labels) if labels.len() == 4), "enums keep their labels");
        let reverb = &lane.effects[0];
        let size = reverb.params.iter().find(|p| p.name == "size").unwrap();
        assert_eq!(size.value, 0.9);
        assert_ne!(size.default, 0.9, "the preset's value, not the override, is the baseline");
        assert_eq!(reverb.mix, 0.4);
        let untouched: Vec<_> = reverb.params.iter().filter(|p| p.name != "size").collect();
        assert!(!untouched.is_empty());
        assert!(
            untouched.iter().all(|p| p.value == p.default),
            "parameters the session does not override keep their baseline"
        );
        assert!(reverb.params.iter().any(|p| p.name == "decay" && p.value != 0.0));
        assert_eq!(run.progress.len(), 6, "three plugin slots, loading + loaded each");
        let pattern = lane.pattern.as_ref().unwrap();
        assert_eq!(
            (pattern.bpm, pattern.length_beats, pattern.looping, pattern.base_note, pattern.enabled, pattern.in_key),
            (90.0, 2.0, false, Some(60), true, true)
        );
        assert_eq!(pattern.events, [(0, 0x90, 60, 100), (24000, 0x80, 60, 0)]);
        assert!(!pattern.recording, "a loaded pattern is never mid-recording");
    }

    #[test]
    fn a_pattern_the_session_marks_disabled_is_loaded_disabled() {
        // The engine enables a pattern that has events as soon as it receives it, so the
        // loader has to say what the session wants. (The TUI used to paper over this by
        // re-sending the flag at startup; `play` did not.)
        let run = load(DISABLED_PATTERN);
        assert_eq!(
            run.commands,
            [
                "AddInstrument range=None",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "SetPattern inst=0 events=2 length_samples=115200 base_note=Some(60) in_key=false",
                "SetGlobalBpm bpm=100.0",
                "SetPatternLength inst=0 beats=4.0",
                "SetPatternEnabled inst=0 enabled=false",
            ]
        );
    }

    #[test]
    fn modulator_targets_that_do_not_resolve_are_skipped() {
        let run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
[[instrument.modulator.target]]
param = "nope"
[[instrument.modulator.target]]
depth = 0.3
[[instrument.modulator.target]]
param = "detune"
slot = 4
"#,
        );
        assert_eq!(
            run.commands,
            [
                "AddInstrument range=None",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "InsertModulator inst=0 index=0 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0 }",
            ],
            "unknown parameter, no field at all, and a slot past the chain are all dropped"
        );
        assert!(run.model.instruments[0].modulators[0].targets.is_empty());
    }

    #[test]
    fn an_unknown_plugin_fails_the_load() {
        let err = try_load("[[instrument]]\nplugin = \"builtin:nope\"\n").expect_err("should fail");
        assert!(format!("{err:#}").contains("Unknown built-in plugin"), "{err:#}");
    }

    #[test]
    fn a_closed_command_channel_fails_the_load() {
        let config = session::parse(include_str!("../../examples/basic.toml")).unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        drop(rx);
        let err = load_session(
            &config,
            Path::new("."),
            48_000.0,
            256,
            &plugin::Runtime::default(),
            &tx,
            &mut |_| {},
        )
        .expect_err("should fail");
        assert!(format!("{err:#}").contains("command channel closed"), "{err:#}");
    }
}
