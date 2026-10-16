//! Loading a session: instantiate its plugins, tell the audio thread what to run
//! (over the command channel), and return the session model.
//!
//! One loader for every front-end: the TUI and `play` both go through
//! `load_session`, so a session means the same thing in all of them. It reports
//! progress through a callback instead of drawing anything, which is how the TUI
//! splash follows along.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crossbeam_channel::Sender;

use crate::app::controller::outputs_fit_the_chain;
use crate::app::model::{
    BPM_MAX, BPM_MIN, FREE, GroupNode, InstrumentNode, ModSourceSlot, ModTargetSlot, ModulatorSlot, NodeId, NodeIds, ParamKind,
    ParamSlot, PatternState, PluginSlot, SYNC_DIVISIONS, SessionModel, format_from_id, mod_source_slot_to_graph, modulator_slot_commands, sync_from_name,
    sync_name, tempo_of_patterns,
};
use crate::plugin::chain::{GraphCommand, ModulatorLocation};
use crate::session::NodeUi;
use crate::telemetry::SlotAllocator;
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
    fn snapshot(plugin: &mut dyn plugin::Plugin) -> Self {
        let all = plugin.parameters();
        let baselines: Vec<f32> = all
            .iter()
            .map(|p| plugin.get_parameter(p.index).unwrap_or(p.default))
            .collect();
        Params { all, values: baselines.clone(), baselines }
    }

    /// The parameter list modulator targets resolve against, each parameter's
    /// `default` replaced by what it is set to now (after the preset and the
    /// session's overrides): a target is centred on that, as it is when the user
    /// binds one in the TUI, not on what the plugin starts at.
    fn for_targets(&self) -> Vec<plugin::ParameterInfo> {
        self.all
            .iter()
            .zip(&self.values)
            .map(|(info, &value)| plugin::ParameterInfo { default: value, ..info.clone() })
            .collect()
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

/// The model's slot for a plugin that has just been loaded and left alone: every
/// parameter at the value the plugin reports (its declared default if it reports
/// none), no preset, fully wet. `node` is the node the plugin sits in. (A session's
/// plugins get theirs from `plugin_slot`, with the values the loader read back in
/// the same way.)
pub fn fresh_slot(plugin: &mut dyn plugin::Plugin, id: &str, node: NodeId) -> PluginSlot {
    let params = Params::snapshot(plugin);
    plugin_slot(
        node,
        plugin.name().to_string(),
        id.to_string(),
        plugin.is_instrument(),
        &params,
        plugin.presets(),
        None,
        1.0,
    )
}

/// The model's slot for a loaded plugin. Parameters a plugin marks "(locked)" are
/// hidden, and a parameter with value labels becomes an enum.
#[allow(clippy::too_many_arguments)]
fn plugin_slot(
    node: NodeId,
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
            unit: p.unit.clone(),
            group: p.group.clone(),
            step_count: p.step_count,
        })
        .collect();
    PluginSlot {
        node,
        name,
        format: format_from_id(&id),
        id,
        is_instrument,
        params: slots,
        presets,
        current_preset,
        mix,
        pinned: Vec::new(),
    }
}

fn send(cmd_tx: &Sender<GraphCommand>, cmd: GraphCommand) -> anyhow::Result<()> {
    cmd_tx.send(cmd).map_err(|_| anyhow::anyhow!("command channel closed"))
}

/// What the loader hands out as it builds nodes: their ids, the telemetry
/// slots modulators and their targets publish into, and where the session says
/// the nodes are drawn.
struct Allocators<'a> {
    ids: &'a mut NodeIds,
    slots: &'a mut SlotAllocator,
    ui: &'a mut BTreeMap<NodeId, NodeUi>,
}

/// Note where a node is drawn, if the session says.
fn place(ui: &mut BTreeMap<NodeId, NodeUi>, node: NodeId, position: Option<NodeUi>) {
    if let Some(position) = position {
        ui.insert(node, position);
    }
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
    let mut ids = NodeIds::default();
    let mut slots = SlotAllocator::default();
    let mut ui: BTreeMap<NodeId, NodeUi> = BTreeMap::new();
    place(&mut ui, NodeId::MASTER, config.master_ui);
    // The parameter lists modulator targets resolve names against: per lane (slot 0 =
    // the instrument, then its effects) and per group bus effect.
    let mut lane_params: Vec<Vec<Vec<plugin::ParameterInfo>>> = Vec::new();
    let mut bus_params: Vec<Vec<Vec<plugin::ParameterInfo>>> = Vec::new();

    // Set up submix groups first, so instruments can reference them by index.
    let mut groups: Vec<GroupNode> = Vec::new();
    for (g_idx, group_config) in config.groups.iter().enumerate() {
        let group_node = ids.alloc();
        place(&mut ui, group_node, group_config.ui);
        send(cmd_tx, GraphCommand::AddGroup)?;
        let meter = slots.meter();
        if let Some(slots) = meter {
            send(cmd_tx, GraphCommand::SetGroupMeter { group: g_idx, slots })?;
        }
        if (group_config.volume - 1.0).abs() > f32::EPSILON {
            send(cmd_tx, GraphCommand::SetGroupVolume { group: g_idx, value: group_config.volume })?;
        }
        if group_config.pan.abs() > f32::EPSILON {
            send(cmd_tx, GraphCommand::SetGroupPan { group: g_idx, value: group_config.pan })?;
        }
        if group_config.muted {
            send(cmd_tx, GraphCommand::SetGroupMute { group: g_idx, muted: true })?;
        }
        if group_config.solo {
            send(cmd_tx, GraphCommand::SetGroupSolo { group: g_idx, solo: true })?;
        }
        let mut group_effects: Vec<PluginSlot> = Vec::new();
        let mut group_bus_params = Vec::new();
        for (fx_idx, effect_config) in group_config.effects.iter().enumerate() {
            let effect_source = session::resolve_plugin_path(&effect_config.plugin, session_dir);
            let mut effect = plugin::load(&effect_source, sample_rate, max_block_size, runtime)?;
            outputs_fit_the_chain(effect.name(), effect.audio_output_count())
                .map_err(|refusal| anyhow::anyhow!("group[{g_idx}].effect[{fx_idx}]: {refusal}"))?;
            if let Some(ref preset_name) = effect_config.preset {
                session::apply_preset(&mut effect, preset_name);
            }
            let presets = effect.presets();
            let mut params = Params::snapshot(effect.as_mut());
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
            let effect_node = ids.alloc();
            place(&mut ui, effect_node, effect_config.ui);
            let mut slot = plugin_slot(
                effect_node,
                name,
                effect_source,
                false,
                &params,
                presets,
                effect_config.preset.clone(),
                effect_config.mix as f32,
            );
            slot.pin_names(&effect_config.pinned);
            group_effects.push(slot);
            group_bus_params.push(params.for_targets());
        }
        bus_params.push(group_bus_params);
        groups.push(GroupNode {
            node: group_node,
            // As the rename popup stores it: trimmed, and nothing is no name.
            name: group_config.name.as_deref().map(str::trim).filter(|name| !name.is_empty()).map(String::from),
            volume: group_config.volume,
            pan: group_config.pan,
            muted: group_config.muted,
            solo: group_config.solo,
            effects: group_effects,
            // Group modulators are loaded after the instruments, once member
            // chains are known (so member-target param names resolve).
            modulators: Vec::new(),
            meter,
        });
    }

    // Set up the graph structure: add instrument lanes.
    let mut instruments: Vec<InstrumentNode> = Vec::new();
    for (inst_idx, inst_config) in config.instruments.iter().enumerate() {
        let lane_node = ids.alloc();
        place(&mut ui, lane_node, inst_config.ui);
        send(cmd_tx, GraphCommand::AddInstrument { range: inst_config.range })?;
        let lane_meter = slots.meter();
        if let Some(slots) = lane_meter {
            send(cmd_tx, GraphCommand::SetLaneMeter { inst: inst_idx, slots })?;
        }

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
            let mut params = Params::snapshot(instrument.as_mut());

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
                GraphCommand::SwapInstrument { inst: inst_idx, instrument, inst_buf, remapper, keep: None },
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

            chain_params.push(params.for_targets());
            let mut slot = plugin_slot(
                lane_node,
                name,
                instrument_source,
                true,
                &params,
                presets,
                plug_config.preset.clone(),
                1.0,
            );
            slot.pin_names(&plug_config.pinned);
            Some(slot)
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
            // (The audio thread turns away an effect that has other outputs than the chain has channels, and the model would keep it: the session is refused.)
            outputs_fit_the_chain(effect.name(), effect.audio_output_count())
                .map_err(|refusal| anyhow::anyhow!("instrument[{inst_idx}].effect[{fx_idx}]: {refusal}"))?;
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
            let mut params = Params::snapshot(effect.as_mut());
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

            chain_params.push(params.for_targets());
            let effect_node = ids.alloc();
            place(&mut ui, effect_node, effect_config.ui);
            let mut slot = plugin_slot(
                effect_node,
                name,
                effect_source,
                false,
                &params,
                presets,
                effect_config.preset.clone(),
                effect_config.mix as f32,
            );
            slot.pin_names(&effect_config.pinned);
            effects.push(slot);
        }

        // Build the lane's modulators now that every plugin (and its params)
        // is loaded, so targets can resolve against any slot in the chain.
        let modulators =
            load_lane_modulators(inst_config, &chain_params, inst_idx, cmd_tx, &mut Allocators { ids: &mut ids, slots: &mut slots, ui: &mut ui })?;

        // Mute and solo: what the engine has to begin with is neither.
        if inst_config.muted {
            send(cmd_tx, GraphCommand::SetMute { inst: inst_idx, muted: true })?;
        }
        if inst_config.solo {
            send(cmd_tx, GraphCommand::SetSolo { inst: inst_idx, solo: true })?;
        }

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
            // (The length in beats first: a length that is set changes the length of a pattern that is there, in proportion, and there is none yet.)
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPatternLength {
                inst: inst_idx,
                beats: p.length_beats,
            });
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPattern {
                inst: inst_idx,
                pattern: plugin::chain::Pattern { events: pattern_events, length_samples },
                base_note: p.base_note,
                in_key: p.in_key,
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
            node: lane_node,
            range: inst_config.range,
            transpose: inst_config.transpose,
            volume,
            muted: inst_config.muted,
            solo: inst_config.solo,
            instrument: instrument_slot,
            effects,
            modulators,
            pattern,
            pitch_bend_range,
            remap,
            group: inst_config.group,
            meter: lane_meter,
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
            &mut Allocators { ids: &mut ids, slots: &mut slots, ui: &mut ui },
        )?;
        groups[g_idx].modulators = mods;
    }

    // The tempo of a session is its `bpm`, or that of its first recorded pattern if it has none, held to the tempos there can be.
    // The audio thread is told it once, and only if the session says one (it starts at `DEFAULT_BPM`): each pattern says the
    // tempo that it was recorded at, and the last of them was what the audio thread was left with.
    let bpm = config.bpm.map_or_else(|| tempo_of_patterns(&instruments), |bpm| bpm.clamp(BPM_MIN, BPM_MAX));
    if config.bpm.is_some() || instruments.iter().any(|lane| lane.pattern.is_some()) {
        let _ = cmd_tx.send(plugin::chain::GraphCommand::SetGlobalBpm { bpm });
    }
    Ok(SessionModel { instruments, groups, ids, slots, ui, bpm })
}

/// Load modulators from a plugin's config and send the commands to the audio thread.
/// Build the lane's modulators (lane-scoped) and send them to the audio
/// thread. Targets address plugins by chain slot (0 = instrument, 1..N =
/// effect). `chain_params[slot]` holds that plugin's parameters for name→index
/// resolution.
///
/// Migration: modulators come from the lane's config (slot defaults to the
/// target's `slot`, i.e. instrument unless set; a lane without an instrument
/// plugin has them too, its slot-0 targets find no parameter) and — for older sessions —
/// from each effect's config (those target their own effect, so their slot is
/// forced to that effect, and their cross-mod sibling indices are offset by
/// where the effect's group lands in the merged lane list).
fn load_lane_modulators(
    inst_config: &session::InstrumentSlotConfig,
    chain_params: &[Vec<plugin::ParameterInfo>],
    inst_idx: usize,
    cmd_tx: &crossbeam_channel::Sender<plugin::chain::GraphCommand>,
    alloc: &mut Allocators,
) -> anyhow::Result<Vec<ModulatorSlot>> {
    // Gather modulator groups with their default slot (instrument = 0, each
    // effect = its slot) and their base index in the merged lane list.
    let mut groups: Vec<(&[session::ModulatorConfig], usize)> = vec![(&inst_config.modulators, 0)];
    for (i, fx) in inst_config.effects.iter().enumerate() {
        groups.push((&fx.modulators, i + 1));
    }
    let mut bases = Vec::with_capacity(groups.len());
    let mut acc = 0usize;
    for (configs, _) in &groups {
        bases.push(acc);
        acc += configs.len();
    }

    // The configs of the lane's modulators in the order they are loaded, which is how a cable between them
    // addresses them: where it is centred is what the file gives the field it drives.
    let siblings: Vec<&session::ModulatorConfig> = groups.iter().flat_map(|(configs, _)| configs.iter()).collect();
    let sibling = |index: usize, field| sibling_field_info(siblings.get(index).copied(), field);

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
        let (output, position) = ModulatorSlot::alloc_slots(alloc.slots, &source_slot);
        for command in modulator_slot_commands(ModulatorLocation::Lane(inst_idx), mod_idx, output, position) {
            send(cmd_tx, command)?;
        }

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
                    let (base, min, max) = sibling(mi, plugin::chain::CrossModField::Rate);
                    (plugin::chain::ModTargetKind::ModulatorRate { mod_index: mi },
                     0, format!("Mod {mi} rate"), min, max, base)
                } else if let Some(ref pair) = target_config.mod_depth {
                    let mi = pair.first().copied().unwrap_or(0) + group_base;
                    let ti = pair.get(1).copied().unwrap_or(0);
                    let (base, min, max) = sibling(mi, plugin::chain::CrossModField::Depth(ti));
                    (plugin::chain::ModTargetKind::ModulatorDepth { mod_index: mi, target_index: ti },
                     0, format!("Mod {mi} depth {ti}"), min, max, base)
                } else if let Some(mi) = target_config.mod_attack {
                    let mi = mi + group_base;
                    let (base, min, max) = sibling(mi, plugin::chain::CrossModField::Attack);
                    (plugin::chain::ModTargetKind::ModulatorAttack { mod_index: mi },
                     0, format!("Mod {mi} attack"), min, max, base)
                } else if let Some(mi) = target_config.mod_decay {
                    let mi = mi + group_base;
                    let (base, min, max) = sibling(mi, plugin::chain::CrossModField::Decay);
                    (plugin::chain::ModTargetKind::ModulatorDecay { mod_index: mi },
                     0, format!("Mod {mi} decay"), min, max, base)
                } else if let Some(mi) = target_config.mod_sustain {
                    let mi = mi + group_base;
                    let (base, min, max) = sibling(mi, plugin::chain::CrossModField::Sustain);
                    (plugin::chain::ModTargetKind::ModulatorSustain { mod_index: mi },
                     0, format!("Mod {mi} sustain"), min, max, base)
                } else if let Some(mi) = target_config.mod_release {
                    let mi = mi + group_base;
                    let (base, min, max) = sibling(mi, plugin::chain::CrossModField::Release);
                    (plugin::chain::ModTargetKind::ModulatorRelease { mod_index: mi },
                     0, format!("Mod {mi} release"), min, max, base)
                } else {
                    log::warn!("Modulator target has no param or mod_* field, skipping");
                    continue;
                };

            let effective = alloc.slots.alloc();
            let target = plugin::chain::ModTarget {
                kind: kind.clone(),
                depth: target_config.depth as f32,
                base_value,
                param_min,
                param_max,
                effective,
            };

            cmd_tx
                .send(plugin::chain::GraphCommand::AddModTarget {
                    inst: inst_idx,
                    mod_index: mod_idx,
                    target,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;

            loaded_targets.push(ModTargetSlot {
                edge: alloc.ids.alloc_edge(),
                slot,
                param_name: label.clone(),
                kind,
                depth: target_config.depth as f32,
                param_min,
                param_max,
                effective,
            });

            log::info!("Modulator {mod_idx} target: '{label}' (slot {slot}) depth={}", target_config.depth);
        }

        let node = alloc.ids.alloc();
        place(alloc.ui, node, mod_config.ui);
        loaded.push(ModulatorSlot {
            node,
            source: source_slot,
            targets: loaded_targets,
            output,
            position,
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
            } else if mod_config.midi_velocity {
                Some(plugin::chain::MidiBindSource::Velocity)
            } else if mod_config.midi_key {
                Some(plugin::chain::MidiBindSource::Key)
            } else {
                None
            };
            // Unbound on load ⇒ re-arm so the next control binds.
            let source = ModSourceSlot::MidiLearn { source: bind, learning: bind.is_none() };
            let desc = match bind {
                Some(plugin::chain::MidiBindSource::Cc(n)) => format!("MIDI CC {n}"),
                Some(plugin::chain::MidiBindSource::PitchBend) => "MIDI pitch bend".to_string(),
                Some(plugin::chain::MidiBindSource::Aftertouch) => "MIDI aftertouch".to_string(),
                Some(plugin::chain::MidiBindSource::Velocity) => "MIDI velocity".to_string(),
                Some(plugin::chain::MidiBindSource::Key) => "MIDI key".to_string(),
                None => "MIDI (learning)".to_string(),
            };
            (source, desc)
        }
        "macro" => {
            // (The number is held to the knob's range: a file can say what the knob cannot be at.)
            let (min, max) = crate::app::intent::ModulatorField::Value.range();
            let value = (mod_config.value as f32).clamp(min, max);
            (ModSourceSlot::Macro { value }, format!("macro at {value:.2}"))
        }
        "steps" => {
            // (What the file gives is held to what a step sequencer can be: a value to -1 to 1, a glide to 0 to 1 and a length to 1 to 16. A step that is
            // not given is nothing, and a file that gives no list at all has a new one's staircase.)
            use crate::app::model::{STEP_MAX, STEP_MIN};
            use crate::plugin::chain::MAX_STEPS;
            let mut values = ModSourceSlot::staircase();
            let length = match &mod_config.steps {
                Some(given) => {
                    values = [0.0; MAX_STEPS];
                    for (value, given) in values.iter_mut().zip(given) {
                        *value = (*given as f32).clamp(STEP_MIN, STEP_MAX);
                    }
                    given.len()
                }
                None => MAX_STEPS,
            };
            let length = mod_config.length.unwrap_or(length).clamp(1, MAX_STEPS);
            let rate = mod_config.rate.map_or(8.0, |rate| rate as f32);
            let glide = (mod_config.glide as f32).clamp(0.0, 1.0);
            let sync = sync_of(mod_config);
            let source = ModSourceSlot::Steps { values, length, rate, glide, retrigger: mod_config.retrigger.unwrap_or(true), sync };
            let desc = if sync == 0 { format!("{length} steps at {rate:.1}Hz") } else { format!("{length} steps, one in {}", sync_name(sync)) };
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
            let rate = mod_config.rate.unwrap_or(1.0);
            // (A note does not start an LFO again unless the file says so, as it never did; the fade is held to what one can be.)
            let (min, max) = crate::app::intent::ModulatorField::Fade.range();
            let fade = (mod_config.fade as f32).clamp(min, max);
            let sync = sync_of(mod_config);
            let source = ModSourceSlot::Lfo { waveform, rate: rate as f32, retrigger: mod_config.retrigger.unwrap_or(false), fade, sync };
            let desc = if sync == 0 { format!("{} {:.1}Hz", waveform.name(), rate) } else { format!("{} {}", waveform.name(), sync_name(sync)) };
            (source, desc)
        }
    }
}

/// The place in the list of note lengths that the file ties a modulator's rate to the tempo by: 0 if it says none (`free` or nothing at all). A name that is no note length is none too,
/// and the log says so (as a waveform that is not one is a sine: a modulator is not worth refusing a session for).
fn sync_of(config: &session::ModulatorConfig) -> usize {
    let Some(name) = &config.sync else { return 0 };
    sync_from_name(name).unwrap_or_else(|| {
        log::warn!("Unknown sync '{name}', defaulting to free (it is one of: {FREE}, {})", SYNC_DIVISIONS.map(|(name, _)| name).join(", "));
        0
    })
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
    field_info(&mm.source, |ti| mm.targets.get(ti).map(|t| t.depth), field)
}

/// The same for a cable from one modulator of a rack to another, before the rack is loaded: the modulator is
/// described by its config, which has the value the file gives the field (the modulator may not be there, and
/// then the field has its default). The cable's modulation is centred on it.
fn sibling_field_info(config: Option<&session::ModulatorConfig>, field: plugin::chain::CrossModField) -> (f32, f32, f32) {
    match config {
        Some(config) => field_info(&build_mod_source(config).0, |ti| config.targets.get(ti).map(|t| t.depth as f32), field),
        None => field_info(&ModSourceSlot::new_lfo(), |_| None, field),
    }
}

/// (base value, min, max) of `field` of a modulator that has `source` and whose cables have the depths
/// `depth_of` says: where a cable that drives the field is centred, and the range it is held to. A field
/// the kind of modulator does not have has the default.
fn field_info(source: &ModSourceSlot, depth_of: impl Fn(usize) -> Option<f32>, field: plugin::chain::CrossModField) -> (f32, f32, f32) {
    use plugin::chain::CrossModField;
    match field {
        CrossModField::Rate => (source.setting(crate::app::intent::ModulatorField::Rate).unwrap_or(1.0), 0.01, 50.0),
        CrossModField::Attack => {
            let v = if let ModSourceSlot::Envelope { attack, .. } = source { *attack } else { 0.01 };
            (v, 0.001, 10.0)
        }
        CrossModField::Decay => {
            let v = if let ModSourceSlot::Envelope { decay, .. } = source { *decay } else { 0.3 };
            (v, 0.001, 10.0)
        }
        CrossModField::Sustain => {
            let v = if let ModSourceSlot::Envelope { sustain, .. } = source { *sustain } else { 0.7 };
            (v, 0.0, 1.0)
        }
        CrossModField::Release => {
            let v = if let ModSourceSlot::Envelope { release, .. } = source { *release } else { 0.5 };
            (v, 0.001, 10.0)
        }
        CrossModField::Depth(ti) => (depth_of(ti).unwrap_or(0.5), 0.0, 1.0),
    }
}

fn load_group_modulators(
    group_config: &session::GroupConfig,
    g_idx: usize,
    member_chains: &[&[Vec<plugin::ParameterInfo>]],
    member_mods: &[&[ModulatorSlot]],
    bus_params: &[Vec<plugin::ParameterInfo>],
    cmd_tx: &crossbeam_channel::Sender<plugin::chain::GraphCommand>,
    alloc: &mut Allocators,
) -> anyhow::Result<Vec<ModulatorSlot>> {
    // A cable between the group's modulators is centred on what the file gives the field it drives.
    let sibling = |index: usize, field| sibling_field_info(group_config.modulators.get(index), field);
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
        let (output, position) = ModulatorSlot::alloc_slots(alloc.slots, &source_slot);
        for command in modulator_slot_commands(ModulatorLocation::Group(g_idx), mod_idx, output, position) {
            send(cmd_tx, command)?;
        }

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
                    let (base, min, max) = sibling(mi, plugin::chain::CrossModField::Rate);
                    (plugin::chain::ModTargetKind::ModulatorRate { mod_index: mi },
                     0, format!("Mod {mi} rate"), min, max, base)
                } else if let Some(ref pair) = target_config.mod_depth {
                    let mi = pair.first().copied().unwrap_or(0);
                    let ti = pair.get(1).copied().unwrap_or(0);
                    let (base, min, max) = sibling(mi, plugin::chain::CrossModField::Depth(ti));
                    (plugin::chain::ModTargetKind::ModulatorDepth { mod_index: mi, target_index: ti },
                     0, format!("Mod {mi} depth {ti}"), min, max, base)
                } else if let Some(mi) = target_config.mod_attack {
                    let (base, min, max) = sibling(mi, plugin::chain::CrossModField::Attack);
                    (plugin::chain::ModTargetKind::ModulatorAttack { mod_index: mi },
                     0, format!("Mod {mi} attack"), min, max, base)
                } else if let Some(mi) = target_config.mod_decay {
                    let (base, min, max) = sibling(mi, plugin::chain::CrossModField::Decay);
                    (plugin::chain::ModTargetKind::ModulatorDecay { mod_index: mi },
                     0, format!("Mod {mi} decay"), min, max, base)
                } else if let Some(mi) = target_config.mod_sustain {
                    let (base, min, max) = sibling(mi, plugin::chain::CrossModField::Sustain);
                    (plugin::chain::ModTargetKind::ModulatorSustain { mod_index: mi },
                     0, format!("Mod {mi} sustain"), min, max, base)
                } else if let Some(mi) = target_config.mod_release {
                    let (base, min, max) = sibling(mi, plugin::chain::CrossModField::Release);
                    (plugin::chain::ModTargetKind::ModulatorRelease { mod_index: mi },
                     0, format!("Mod {mi} release"), min, max, base)
                } else {
                    log::warn!("Group modulator target has no param/member/bus/mod_* field, skipping");
                    continue;
                };

            let effective = alloc.slots.alloc();
            let target = plugin::chain::ModTarget {
                kind: kind.clone(),
                depth: target_config.depth as f32,
                base_value,
                param_min,
                param_max,
                effective,
            };
            cmd_tx
                .send(plugin::chain::GraphCommand::AddGroupModTarget {
                    group: g_idx,
                    mod_index: mod_idx,
                    target,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;

            loaded_targets.push(ModTargetSlot {
                edge: alloc.ids.alloc_edge(),
                slot,
                param_name: label,
                kind,
                depth: target_config.depth as f32,
                param_min,
                param_max,
                effective,
            });
        }

        let node = alloc.ids.alloc();
        place(alloc.ui, node, mod_config.ui);
        loaded.push(ModulatorSlot {
            node,
            source: source_slot,
            targets: loaded_targets,
            output,
            position,
        });
        log::info!("Loaded group modulator {mod_idx} for group={g_idx}: {desc}");
    }
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::{DISABLED_PATTERN, GROUPS, LEGACY, MIDI, PINNED, PLACED, Run, SETTINGS, load, try_load};

    #[test]
    fn plugin_slots_hide_locked_parameters_and_turn_labelled_ones_into_enums() {
        let info = |index: u32, name: &str, labels: Option<Vec<&str>>| plugin::ParameterInfo {
            index,
            name: name.to_string(),
            min: 0.0,
            max: 10.0,
            default: 5.0,
            labels: labels.map(|l| l.into_iter().map(String::from).collect()),
            ..Default::default()
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
            NodeId(7),
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
    fn a_group_named_with_nothing_or_with_spaces_has_no_name() {
        // As in the rename popup: the name is trimmed, and nothing is no name, so the
        // header reads "Group N" rather than blank.
        let model = load(
            "[[group]]\nname = \"\"\n[[group]]\nname = \"   \"\n[[group]]\nname = \" Pad \"\n[[group]]\nname = \"Lead\"\n[[group]]\n",
        )
        .model;
        let names: Vec<Option<&str>> = model.groups.iter().map(|g| g.name.as_deref()).collect();
        assert_eq!(names, [None, None, Some("Pad"), Some("Lead"), None]);
    }

    #[test]
    fn every_modulation_target_gets_an_edge_id_of_its_own() {
        let model = load(GROUPS).model;
        let edges: Vec<_> = model
            .groups
            .iter()
            .flat_map(|g| &g.modulators)
            .chain(model.instruments.iter().flat_map(|lane| &lane.modulators))
            .flat_map(|m| &m.targets)
            .map(|t| t.edge)
            .collect();
        assert!(edges.len() >= 6, "the scene has targets in lanes and in groups: {}", edges.len());
        let distinct: std::collections::HashSet<_> = edges.iter().collect();
        assert_eq!(distinct.len(), edges.len());
    }

    #[test]
    fn a_slot_keeps_each_parameters_unit_section_and_steps() {
        let info = |index: u32, name: &str, unit: Option<&str>, group: &[&str], step_count: u32| plugin::ParameterInfo {
            index,
            name: name.to_string(),
            max: 10.0,
            unit: unit.map(String::from),
            group: group.iter().map(|g| g.to_string()).collect(),
            step_count,
            ..Default::default()
        };
        let params = Params {
            all: vec![info(0, "gain", Some("dB"), &["Output", "Master"], 0), info(1, "steps", None, &[], 5)],
            baselines: vec![0.0, 0.0],
            values: vec![0.0, 0.0],
        };
        let slot = plugin_slot(NodeId(1), "P".to_string(), "builtin:osc".to_string(), false, &params, vec![], None, 1.0);
        let rows: Vec<_> = slot.params.iter().map(|p| (p.unit.as_deref(), p.group.clone(), p.step_count)).collect();
        assert_eq!(rows, [(Some("dB"), vec!["Output".to_string(), "Master".to_string()], 0), (None, vec![], 5)]);
    }

    #[test]
    fn a_fresh_slot_describes_the_plugin_at_its_defaults() {
        let load = |id: &str| plugin::load(id, 48_000.0, 256, &Default::default()).unwrap();

        let reverb = load("builtin:reverb");
        let mut reverb = reverb;
        let slot = fresh_slot(reverb.as_mut(), "builtin:reverb", NodeId(3));
        assert_eq!(slot.node, NodeId(3));
        assert_eq!((slot.name.as_str(), slot.id.as_str(), slot.format.as_str()), ("Reverb", "builtin:reverb", "Built-in"));
        assert!(!slot.is_instrument);
        assert_eq!((slot.mix, slot.current_preset.as_deref()), (1.0, None));
        assert!(!slot.presets.is_empty(), "the plugin's presets come along");
        assert_eq!(slot.params.len(), reverb.parameters().len());
        for (row, info) in slot.params.iter().zip(reverb.parameters()) {
            assert_eq!((row.index, row.name.as_str(), row.min, row.max), (info.index, info.name.as_str(), info.min, info.max));
            assert_eq!((row.default, row.value), (info.default, info.default), "{}", info.name);
        }

        let osc = load("builtin:osc");
        let mut osc = osc;
        let slot = fresh_slot(osc.as_mut(), "builtin:osc", NodeId(4));
        assert!(slot.is_instrument);
        let waveform = slot.params.iter().find(|p| p.name == "waveform").unwrap();
        assert!(matches!(&waveform.kind, ParamKind::Enum(labels) if labels.len() == 5), "labelled parameters are enums");
    }

    /// A plugin whose parameters are not at their declared defaults when it is loaded
    /// (as a VST3 may be, with the state it starts in): `live` is what it reports.
    struct Live {
        live: Vec<Option<f32>>,
    }

    impl plugin::Plugin for Live {
        fn name(&self) -> &str {
            "Live"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn sample_rate(&self) -> f32 {
            48_000.0
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }
        fn process(&mut self, _: &[(u64, [u8; 3])], _: &[&[f32]], _: &mut [&mut [f32]]) -> anyhow::Result<()> {
            Ok(())
        }
        fn parameters(&self) -> Vec<plugin::ParameterInfo> {
            (0..self.live.len() as u32)
                .map(|index| plugin::ParameterInfo { index, name: format!("p{index}"), min: 0.0, max: 1.0, default: 0.25, ..Default::default() })
                .collect()
        }
        fn get_parameter(&mut self, index: u32) -> Option<f32> {
            self.live[index as usize]
        }
        fn set_parameter(&mut self, _: u32, _: f32) -> anyhow::Result<()> {
            Ok(())
        }
        fn presets(&self) -> Vec<plugin::Preset> {
            Vec::new()
        }
        fn load_preset(&mut self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_fresh_slot_shows_what_the_plugin_is_set_to_not_what_it_declares() {
        // (Finding f14.) The session's plugins are read back by the loader; so is a
        // plugin added from the selector, and the value is also what is not saved as an edit.
        let mut live = Live { live: vec![Some(0.75), None, Some(0.0)] };
        let slot = fresh_slot(&mut live, "clap:live", NodeId(5));
        let rows: Vec<(f32, f32)> = slot.params.iter().map(|p| (p.value, p.default)).collect();
        assert_eq!(rows, [(0.75, 0.75), (0.25, 0.25), (0.0, 0.0)], "the live value, and the declared default where it reports none");
    }

    #[test]
    fn every_modulator_and_every_parameter_it_drives_gets_a_telemetry_slot() {
        // `load` also checks the model's slots (all different, none reserved).
        let run = load(GROUPS);
        let model = &run.model;
        let group = &model.groups[0];
        let lane_lfo = &model.instruments[1].modulators[0];
        assert!(lane_lfo.output.is_some() && group.modulators.iter().all(|m| m.output.is_some()));
        // The group LFO's three parameter targets publish what their parameters are
        // set to, and so do its cross-mod target and the envelope's two, which drive
        // a modulator's own settings (f132).
        let effective: Vec<Vec<bool>> =
            group.modulators.iter().map(|m| m.targets.iter().map(|t| t.effective.is_some()).collect()).collect();
        assert_eq!(effective, [vec![true, true, true, true], vec![true, true]]);

        // Slots 0 and 1 are the master meter's. Then, in the order the loader sends
        // things: the group's meter (2, 3), lane 0's (4, 5), lane 1's (6, 7) and the
        // output of its LFO (8), lane 2's meter (9, 10); then the group modulators,
        // each output followed by its targets (the ones the loader skipped took no
        // slot).
        let output = |m: &ModulatorSlot| m.output.unwrap().index();
        assert_eq!(output(lane_lfo), 8);
        assert_eq!(output(&group.modulators[0]), 11);
        let driven: Vec<u32> = group.modulators[0].targets.iter().filter_map(|t| t.effective).map(|s| s.index()).collect();
        assert_eq!(driven, [12, 13, 14, 15]);
        assert_eq!(output(&group.modulators[1]), 16);
        let driven: Vec<u32> = group.modulators[1].targets.iter().filter_map(|t| t.effective).map(|s| s.index()).collect();
        assert_eq!(driven, [17, 18]);
        assert_eq!(model.slots.high_water(), 19);

        // The audio thread was told, right after each node was added.
        assert_eq!(
            run.telemetry,
            [
                "SetGroupMeter group=0 peak=2 clips=3",
                "SetLaneMeter inst=0 peak=4 clips=5",
                "SetLaneMeter inst=1 peak=6 clips=7",
                "SetModulatorOutput Lane(1) index=0 slot=8",
                "SetLaneMeter inst=2 peak=9 clips=10",
                "SetModulatorOutput Group(0) index=0 slot=11",
                "SetModulatorOutput Group(0) index=1 slot=16",
            ]
        );
    }

    #[test]
    fn a_lane_modulators_cable_to_a_setting_of_another_gets_a_slot_named_at_the_port_it_ends_at() {
        // (f132: only a cable to a parameter did; the engine publishes what a modulator's own setting was set to now.)
        let model = load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\nrate = 1.0\n[[instrument.modulator.target]]\nmod_attack = 1\ndepth = 0.2\n\
             [[instrument.modulator]]\ntype = \"envelope\"\n",
        )
        .model;
        let (lfo, envelope) = (&model.instruments[0].modulators[0], &model.instruments[0].modulators[1]);
        let slot = lfo.targets[0].effective.expect("a cable to a setting has a slot");
        let layout = model.telemetry_layout();
        let named: Vec<_> = layout.modulation.iter().filter(|e| e.kind == crate::app::layout::SlotKind::Effective).map(|e| (e.slot, e.node, e.port)).collect();
        assert_eq!(named, [(slot.index(), envelope.node, crate::app::graph::Port::Attack)]);
    }

    /// The `AddModTarget` / `AddGroupModTarget` lines of a load.
    fn target_commands(run: &crate::app::testutil::Run) -> Vec<&str> {
        run.commands.iter().map(String::as_str).filter(|c| c.starts_with("Add") && c.contains("ModTarget")).collect()
    }

    #[test]
    fn a_lane_target_is_centred_on_what_the_session_sets_the_parameter_to() {
        // The oscillator's `detune` and the filter's `cutoff` are set, and an LFO
        // drives each: it wobbles around the value the session set, not around the
        // plugin's default (0 and 1).
        let run = load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\ndetune = 3.0\n\
             [[instrument.effect]]\nplugin = \"builtin:filter\"\n[instrument.effect.params]\ncutoff = 0.35\n\
             [[instrument.modulator]]\n\
             [[instrument.modulator.target]]\nparam = \"detune\"\ndepth = 0.5\n\
             [[instrument.modulator.target]]\nslot = 1\nparam = \"cutoff\"\ndepth = 0.5\n",
        );
        assert_eq!(
            target_commands(&run),
            [
                "AddModTarget inst=0 mod=0 kind=PluginParam { slot: 0, param_index: 1 } depth=0.500 base=3.000 range=[-24.000, 24.000]",
                "AddModTarget inst=0 mod=0 kind=PluginParam { slot: 1, param_index: 0 } depth=0.500 base=0.350 range=[0.000, 1.000]",
            ]
        );
    }

    #[test]
    fn a_target_is_centred_on_the_value_a_preset_gave_the_parameter() {
        // A parameter the preset moves off its default, and one the session sets on
        // top of the preset: the first is centred on the preset's value, the second
        // on the session's.
        let mut reverb = crate::plugin::builtin::load("builtin:reverb", 48_000.0, 256).unwrap();
        let params = reverb.parameters();
        let named = |name: &str| params.iter().find(|p| p.name == name).unwrap().clone();
        let (damping, size) = (named("damping"), named("size"));
        let preset = reverb
            .presets()
            .into_iter()
            .find(|p| {
                reverb.load_preset(&p.id).unwrap();
                reverb.get_parameter(damping.index) != Some(damping.default)
            })
            .expect("a preset that moves `damping`");
        reverb.load_preset(&preset.id).unwrap();
        let preset_damping = reverb.get_parameter(damping.index).unwrap();

        let run = load(&format!(
            "[[instrument]]\nplugin = \"builtin:osc\"\n\
             [[instrument.effect]]\nplugin = \"builtin:reverb\"\npreset = \"{}\"\n[instrument.effect.params]\nsize = 0.9\n\
             [[instrument.modulator]]\n\
             [[instrument.modulator.target]]\nslot = 1\nparam = \"damping\"\ndepth = 0.5\n\
             [[instrument.modulator.target]]\nslot = 1\nparam = \"size\"\ndepth = 0.5\n",
            preset.name
        ));
        let commands = target_commands(&run);
        let base = |c: &str| c.split("base=").nth(1).unwrap().split(' ').next().unwrap().to_string();
        assert_eq!(base(commands[0]), format!("{preset_damping:.3}"), "{}", commands[0]);
        assert_ne!(base(commands[0]), format!("{:.3}", damping.default));
        assert_eq!(base(commands[1]), "0.900", "{}", commands[1]);
        let _ = size;
    }

    #[test]
    fn group_targets_are_centred_on_the_session_s_values_too() {
        // A member's filter `cutoff` and the bus reverb's `damping` are set, and a
        // group LFO drives each.
        let run = load(
            "[[group]]\n[[group.effect]]\nplugin = \"builtin:reverb\"\n[group.effect.params]\ndamping = 0.6\n\
             [[group.modulator]]\n\
             [[group.modulator.target]]\nmember = 0\nslot = 1\nparam = \"cutoff\"\ndepth = 0.5\n\
             [[group.modulator.target]]\nbus = 0\nparam = \"damping\"\ndepth = 0.5\n\
             [[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n\
             [[instrument.effect]]\nplugin = \"builtin:filter\"\n[instrument.effect.params]\ncutoff = 0.35\n",
        );
        assert_eq!(
            target_commands(&run),
            [
                "AddGroupModTarget group=0 mod=0 kind=GroupMember { member: 0, slot: 1, param_index: 0 } depth=0.500 base=0.350 range=[0.000, 1.000]",
                "AddGroupModTarget group=0 mod=0 kind=GroupBus { effect_index: 0, param_index: 2 } depth=0.500 base=0.600 range=[0.000, 1.000]",
            ]
        );
    }

    #[test]
    fn a_parameter_the_session_leaves_alone_keeps_its_default_as_the_centre() {
        let run = load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n\
             [[instrument.effect]]\nplugin = \"builtin:filter\"\n\
             [[instrument.modulator]]\n\
             [[instrument.modulator.target]]\nslot = 1\nparam = \"cutoff\"\ndepth = 0.5\n",
        );
        assert_eq!(
            target_commands(&run),
            ["AddModTarget inst=0 mod=0 kind=PluginParam { slot: 1, param_index: 0 } depth=0.500 base=1.000 range=[0.000, 1.000]"]
        );
    }

    #[test]
    fn each_of_a_lanes_modulators_is_told_its_own_slot_by_its_index() {
        // MIDI: six modulators in lane 0, the first and the fifth with one target.
        let run = load(MIDI);
        assert_eq!(
            run.telemetry,
            [
                "SetLaneMeter inst=0 peak=2 clips=3",
                "SetModulatorOutput Lane(0) index=0 slot=4",
                // slot 5 is the first modulator's target
                "SetModulatorOutput Lane(0) index=1 slot=6",
                "SetModulatorOutput Lane(0) index=2 slot=7",
                "SetModulatorOutput Lane(0) index=3 slot=8",
                "SetModulatorOutput Lane(0) index=4 slot=9",
                // slot 10 is the fifth modulator's target
                "SetModulatorOutput Lane(0) index=5 slot=11",
            ]
        );
        let outputs: Vec<u32> = run.model.instruments[0].modulators.iter().map(|m| m.output.unwrap().index()).collect();
        assert_eq!(outputs, [4, 6, 7, 8, 9, 11]);
    }

    #[test]
    fn a_step_sequencer_in_a_lane_is_told_where_to_publish_its_position_right_after_its_output() {
        // Two step sequencers, the second with a cable, around an LFO.
        let run = load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n\
             [[instrument.modulator]]\ntype = \"steps\"\n\
             [[instrument.modulator]]\n\
             [[instrument.modulator]]\ntype = \"steps\"\n\
             [[instrument.modulator.target]]\nparam = \"detune\"\ndepth = 0.5\n",
        );
        assert_eq!(
            run.telemetry,
            [
                "SetLaneMeter inst=0 peak=2 clips=3",
                "SetModulatorOutput Lane(0) index=0 slot=4",
                "SetModulatorPosition Lane(0) index=0 slot=5",
                "SetModulatorOutput Lane(0) index=1 slot=6",
                "SetModulatorOutput Lane(0) index=2 slot=7",
                "SetModulatorPosition Lane(0) index=2 slot=8",
                // slot 9 is the second step sequencer's target
            ]
        );
        let slots: Vec<(u32, Option<u32>)> =
            run.model.instruments[0].modulators.iter().map(|m| (m.output.unwrap().index(), m.position.map(|slot| slot.index()))).collect();
        assert_eq!(slots, [(4, Some(5)), (6, None), (7, Some(8))]);
        run.model.check_ids().unwrap();
    }

    #[test]
    fn a_step_sequencer_in_a_group_is_told_where_to_publish_its_position_right_after_its_output() {
        let run = load(
            "[[group]]\n[[group.modulator]]\ntype = \"steps\"\n[[group.modulator]]\n[[group.modulator]]\ntype = \"steps\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n",
        );
        let slots: Vec<(u32, Option<u32>)> =
            run.model.groups[0].modulators.iter().map(|m| (m.output.unwrap().index(), m.position.map(|slot| slot.index()))).collect();
        let [(first, Some(first_position)), (second, None), (third, Some(third_position))] = slots[..] else { panic!("{slots:?}") };
        assert_eq!((first_position, third_position), (first + 1, third + 1));
        let modulators: Vec<&String> = run.telemetry.iter().filter(|line| line.contains("Modulator")).collect();
        assert_eq!(
            modulators,
            [
                &format!("SetModulatorOutput Group(0) index=0 slot={first}"),
                &format!("SetModulatorPosition Group(0) index=0 slot={first_position}"),
                &format!("SetModulatorOutput Group(0) index=1 slot={second}"),
                &format!("SetModulatorOutput Group(0) index=2 slot={third}"),
                &format!("SetModulatorPosition Group(0) index=2 slot={third_position}"),
            ]
        );
        run.model.check_ids().unwrap();
    }

    #[test]
    fn a_session_with_no_modulators_uses_a_meter_for_each_lane_and_nothing_else() {
        let run = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n");
        assert_eq!(run.model.slots.high_water(), 4, "the master meter's two, and the lane's");
        assert_eq!(run.telemetry, ["SetLaneMeter inst=0 peak=2 clips=3"]);
        let meter = run.model.instruments[0].meter.unwrap();
        assert_eq!((meter.peak.index(), meter.clips.index()), (2, 3));
        assert!(run.model.groups.is_empty());
    }

    #[test]
    fn a_group_and_its_lanes_each_get_a_meter_as_they_are_added() {
        let run = load(
            "[[group]]\n[[group]]\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 1\n[[instrument]]\nplugin = \"builtin:osc\"\n",
        );
        assert_eq!(
            run.telemetry,
            [
                "SetGroupMeter group=0 peak=2 clips=3",
                "SetGroupMeter group=1 peak=4 clips=5",
                "SetLaneMeter inst=0 peak=6 clips=7",
                "SetLaneMeter inst=1 peak=8 clips=9",
            ]
        );
        let slots = |m: Option<crate::telemetry::MeterSlots>| m.map(|m| (m.peak.index(), m.clips.index()));
        assert_eq!(run.model.groups.iter().map(|g| slots(g.meter)).collect::<Vec<_>>(), [Some((2, 3)), Some((4, 5))]);
        assert_eq!(run.model.instruments.iter().map(|l| slots(l.meter)).collect::<Vec<_>>(), [Some((6, 7)), Some((8, 9))]);
    }

    #[test]
    fn a_modulator_slot_command_follows_the_command_that_made_the_modulator() {
        for scene in [MIDI, GROUPS] {
            let (tx, rx) = crossbeam_channel::unbounded();
            let model = crate::app::testutil::load_onto(scene, &tx);
            let sent: Vec<String> = rx.try_iter().map(|c| crate::app::testutil::describe(&c)).collect();
            for (i, line) in sent.iter().enumerate() {
                if line.starts_with("SetModulatorOutput") {
                    let before = &sent[i - 1];
                    assert!(
                        before.starts_with("InsertModulator") || before.starts_with("InsertGroupModulator"),
                        "{line} follows {before}"
                    );
                }
            }
            let modulators: usize = model.instruments.iter().map(|l| l.modulators.len()).sum::<usize>()
                + model.groups.iter().map(|g| g.modulators.len()).sum::<usize>();
            assert_eq!(sent.iter().filter(|l| l.starts_with("SetModulatorOutput")).count(), modulators);
        }
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
                "InsertModulator inst=0 index=0 source=Lfo { waveform: Sine, rate: 4.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddModTarget inst=0 mod=0 kind=PluginParam { slot: 0, param_index: 1 } depth=0.040 base=0.000 range=[-24.000, 24.000]",
                "AddInstrument range=Some((60, 108))",
                "SwapInstrument inst=1 plugin=Oscillator bufs=2 remap=false",
                "InsertModulator inst=1 index=0 source=Lfo { waveform: Sine, rate: 7.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddModTarget inst=1 mod=0 kind=PluginParam { slot: 0, param_index: 1 } depth=0.150 base=0.000 range=[-24.000, 24.000]",
            ]
        );
        assert_eq!(run.model.instruments.len(), 2);
        assert_eq!(run.model.instruments[0].range, Some((12, 59)));
        assert_eq!(run.model.instruments[1].modulators[0].targets[0].depth, 0.15);
    }

    #[test]
    fn echo_example_loads_a_filter_and_a_ping_pong_delay_with_an_lfo_each() {
        let run = load(include_str!("../../examples/echo.toml"));
        assert_eq!(
            run.commands,
            [
                "AddInstrument range=None",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "SetVolume inst=0 value=0.300",
                "SetParameter inst=0 slot=0 param=6 value=0.300",
                "SetParameter inst=0 slot=0 param=0 value=3.000",
                "InsertEffect inst=0 index=0 plugin=Filter mix=1.000",
                "SetParameter inst=0 slot=1 param=0 value=0.550",
                "SetParameter inst=0 slot=1 param=1 value=2.000",
                "InsertEffect inst=0 index=1 plugin=Delay mix=0.400",
                "InsertModulator inst=0 index=0 source=Lfo { waveform: Sine, rate: 0.2, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddModTarget inst=0 mod=0 kind=PluginParam { slot: 1, param_index: 0 } depth=0.300 base=0.550 range=[0.000, 1.000]",
                // (The second LFO moves the delay's `time`, which the preset has at 0.3 s: that is where it is centred, over a range of 0.001 to 2 s.)
                "InsertModulator inst=0 index=1 source=Lfo { waveform: Triangle, rate: 0.3, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddModTarget inst=0 mod=1 kind=PluginParam { slot: 2, param_index: 0 } depth=0.004 base=0.300 range=[0.001, 2.000]",
            ]
        );
        let delay = &run.model.instruments[0].effects[1];
        assert_eq!((delay.name.as_str(), delay.current_preset.as_deref(), delay.mix), ("Delay", Some("Ping-Pong"), 0.4));
    }

    /// A group with a macro at 0.5, and a lane in it with a macro at 0.25 that drives the oscillator's `detune`.
    const MACROS: &str = r#"
[[group]]
[[group.modulator]]
type = "macro"
value = 0.5

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
type = "macro"
value = 0.25
[[instrument.modulator.target]]
param = "detune"
depth = 0.4
"#;

    #[test]
    fn a_macro_is_loaded_at_the_value_the_file_gives_it_in_a_lane_and_in_a_group_and_the_engine_is_given_it() {
        let run = load(MACROS);
        assert!(matches!(run.model.instruments[0].modulators[0].source, ModSourceSlot::Macro { value } if value == 0.25));
        assert!(matches!(run.model.groups[0].modulators[0].source, ModSourceSlot::Macro { value } if value == 0.5));
        assert!(run.commands.contains(&"InsertModulator inst=0 index=0 source=Macro { value: 0.25 }".to_string()), "{:#?}", run.commands);
        assert!(run.commands.contains(&"InsertGroupModulator group=0 index=0 source=Macro { value: 0.5 }".to_string()), "{:#?}", run.commands);
        // Its cable is a cable like any other.
        assert_eq!(run.model.instruments[0].modulators[0].targets.len(), 1);
        assert!(run.commands.iter().any(|c| c.starts_with("AddModTarget inst=0 mod=0 kind=PluginParam { slot: 0, param_index: 1 } depth=0.400")), "{:#?}", run.commands);
    }

    #[test]
    fn a_macro_that_has_no_value_is_at_rest_and_a_value_beyond_its_range_is_held_in_it() {
        let value_of = |text: &str| match load(text).model.instruments[0].modulators[0].source {
            ModSourceSlot::Macro { value } => value,
            ref other => panic!("{other:?}"),
        };
        let lane = |value: &str| format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"macro\"\n{value}");
        assert_eq!(value_of(&lane("")), 0.0);
        assert_eq!(value_of(&lane("value = 1.5\n")), 1.0);
        assert_eq!(value_of(&lane("value = -2.0\n")), 0.0);
        assert_eq!(value_of(&lane("value = 0.75\n")), 0.75);
    }

    /// A group with a step sequencer that the file describes, and a lane in it with one that the file leaves as a new one is, and an LFO whose cable drives its rate.
    const SEQUENCERS: &str = r#"
[[group]]
[[group.modulator]]
type = "steps"
steps = [0.5, -0.25, 1.0]
rate = 3.5
glide = 0.25
retrigger = false

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
type = "steps"
[[instrument.modulator]]
[[instrument.modulator.target]]
mod_rate = 0
depth = 0.2
"#;

    fn steps_of(source: &ModSourceSlot) -> ([f32; 16], usize, f32, f32, bool) {
        match source {
            ModSourceSlot::Steps { values, length, rate, glide, retrigger, .. } => (*values, *length, *rate, *glide, *retrigger),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_step_sequencer_is_loaded_with_what_the_file_gives_it_and_the_engine_is_given_it_at_its_first_step() {
        let run = load(SEQUENCERS);
        let mut values = [0.0; 16];
        values[..3].copy_from_slice(&[0.5, -0.25, 1.0]);
        // The values that are not given are nothing, and as many are in use as are given.
        assert_eq!(steps_of(&run.model.groups[0].modulators[0].source), (values, 3, 3.5, 0.25, false));
        assert!(
            run.commands.contains(
                &"InsertGroupModulator group=0 index=0 source=Steps { values: [0.5, -0.25, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], length: 3, rate: 3.5, glide: 0.25, retrigger: false, sync: 0.0, position: 0.0 }"
                    .to_string()
            ),
            "{:#?}",
            run.commands
        );
        // One that the file does not describe is what a new one is: the staircase, all sixteen in use, 8 a second, no glide, started again by a note.
        let ModSourceSlot::Steps { values: staircase, .. } = ModSourceSlot::new_steps() else { unreachable!() };
        assert_eq!(steps_of(&run.model.instruments[0].modulators[0].source), (staircase, 16, 8.0, 0.0, true));
        // And a cable that drives its rate is centred on the 8 that it has, where an LFO that has none is at 1.
        assert!(
            run.commands.iter().any(|c| c.starts_with("AddModTarget inst=0 mod=1 kind=ModulatorRate { mod_index: 0 } depth=0.200 base=8.000 range=[0.010, 50.000]")),
            "{:#?}",
            run.commands
        );
        assert_eq!(run.model.instruments[0].modulators.len(), 2);
    }

    #[test]
    fn an_lfo_with_a_key_in_the_file_is_loaded_with_it_and_one_that_says_nothing_of_it_runs_free() {
        let lane = |fields: &str| format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"lfo\"\n{fields}");
        let of = |fields: &str| match load(&lane(fields)).model.instruments[0].modulators[0].source {
            ModSourceSlot::Lfo { retrigger, fade, .. } => (retrigger, fade),
            ref other => panic!("{other:?}"),
        };
        assert_eq!(of(""), (false, 0.0), "a file that says nothing of it is an LFO that runs free, as every file made one");
        assert_eq!(of("retrigger = true\nfade = 0.75\n"), (true, 0.75));
        assert_eq!(of("retrigger = false\n"), (false, 0.0));
        // A fade is held to what one can be.
        assert_eq!(of("fade = 9.0\n").1, 5.0);
        assert_eq!(of("fade = -1.0\n").1, 0.0);
        // The engine is given the key with the LFO, which rests at full until a note.
        let run = load(&lane("rate = 2.0\nretrigger = true\nfade = 0.5\n"));
        let inserted = "InsertModulator inst=0 index=0 source=Lfo { waveform: Sine, rate: 2.0, phase: 0.0, retrigger: true, fade: 0.5, sync: 0.0, age: inf }".to_string();
        assert!(run.commands.contains(&inserted), "{:#?}", run.commands);
        // A group's LFO has them too.
        let run = load("[[group]]\n[[group.modulator]]\nretrigger = true\nfade = 1.5\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n");
        match run.model.groups[0].modulators[0].source {
            ModSourceSlot::Lfo { retrigger, fade, .. } => assert_eq!((retrigger, fade), (true, 1.5)),
            ref other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_modulator_that_the_file_ties_to_the_tempo_is_loaded_with_the_note_length_and_the_engine_is_given_its_beats_and_the_rate_it_keeps() {
        let lane = |kind: &str, fields: &str| format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"{kind}\"\n{fields}");
        let sync_of = |kind: &str, fields: &str| match load(&lane(kind, fields)).model.instruments[0].modulators[0].source {
            ModSourceSlot::Lfo { sync, rate, .. } | ModSourceSlot::Steps { sync, rate, .. } => (sync, rate),
            ref other => panic!("{other:?}"),
        };
        // A file that says nothing of it is as free as every file was.
        assert_eq!(sync_of("lfo", "rate = 2.0\n"), (0, 2.0));
        assert_eq!(sync_of("steps", ""), (0, 8.0));
        // 11 is an eighth note and 12 its triplet, 14 a sixteenth, 1 four bars and 16 a thirty-second; the rate it has is kept.
        assert_eq!(sync_of("lfo", "rate = 2.0\nsync = \"1/8\"\n"), (11, 2.0));
        assert_eq!(sync_of("lfo", "sync = \"1/8 triplet\"\n").0, 12);
        assert_eq!(sync_of("steps", "sync = \"1/16\"\nrate = 3.0\n"), (14, 3.0));
        assert_eq!(sync_of("lfo", "sync = \"4 bars\"\n").0, 1);
        assert_eq!(sync_of("steps", "sync = \"1/32\"\n").0, 16);
        // (The name is read in any case and with any space about it, and `free` is none.)
        assert_eq!(sync_of("lfo", "sync = \"  1/4   DOTTED \"\n").0, 7);
        assert_eq!(sync_of("lfo", "sync = \"free\"\n").0, 0);
        // The engine is given the beats with the modulator, a group's too.
        let run = load(&lane("lfo", "rate = 2.0\nsync = \"1/8\"\n"));
        let inserted = "InsertModulator inst=0 index=0 source=Lfo { waveform: Sine, rate: 2.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.5, age: inf }".to_string();
        assert!(run.commands.contains(&inserted), "{:#?}", run.commands);
        let run = load("[[group]]\n[[group.modulator]]\ntype = \"steps\"\nsync = \"1/16\"\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n");
        match run.model.groups[0].modulators[0].source {
            ModSourceSlot::Steps { sync, .. } => assert_eq!(sync, 14),
            ref other => panic!("{other:?}"),
        }
        assert!(run.commands.iter().any(|c| c.starts_with("InsertGroupModulator group=0 index=0 source=Steps {") && c.contains("sync: 0.25")), "{:#?}", run.commands);
    }

    #[test]
    fn a_sync_that_is_no_note_length_leaves_the_modulator_free_and_a_modulator_that_cannot_follow_the_tempo_ignores_one() {
        let lane = |kind: &str, fields: &str| format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"{kind}\"\n{fields}");
        for unknown in ["1/7", "1/8t", "eighth", "7"] {
            match load(&lane("lfo", &format!("rate = 2.0\nsync = \"{unknown}\"\n"))).model.instruments[0].modulators[0].source {
                ModSourceSlot::Lfo { sync, rate, .. } => assert_eq!((sync, rate), (0, 2.0), "{unknown}"),
                ref other => panic!("{other:?}"),
            }
        }
        // An envelope, a macro and a MIDI control have no rate to tie to it.
        for kind in ["envelope", "macro", "midi"] {
            let run = load(&lane(kind, "sync = \"1/8\"\n"));
            assert!(!format!("{:?}", run.model.instruments[0].modulators[0].source).contains("sync"), "{kind}");
        }
    }

    #[test]
    fn what_a_file_says_that_a_step_sequencer_cannot_be_is_held_to_what_it_can() {
        let lane = |fields: &str| format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"steps\"\n{fields}");
        let of = |fields: &str| steps_of(&load(&lane(fields)).model.instruments[0].modulators[0].source);
        // A value is held to -1 to 1, and so is a glide to 0 to 1.
        let (values, length, ..) = of("steps = [2.0, -3.0, 0.5]\n");
        assert_eq!((&values[..4], length), (&[1.0, -1.0, 0.5, 0.0][..], 3));
        assert_eq!(of("glide = -1.0\n").3, 0.0);
        assert_eq!(of("glide = 2.0\n").3, 1.0);
        assert_eq!(of("glide = 0.5\n").3, 0.5);
        // A length is held to 1 to 16, and is how many there are values if the file does not say; steps past the values given are nothing.
        assert_eq!(of("length = 0\n").1, 1);
        assert_eq!(of("length = 40\n").1, 16);
        assert_eq!(of("steps = [0.5]\nlength = 9\n").1, 9);
        assert_eq!(of("steps = [0.5]\nlength = 9\n").0[1..9], [0.0; 8]);
        assert_eq!(of("steps = [0.5, 0.25]\n").1, 2);
        // A rate is what the file says, and a step sequencer's is 8 and not an LFO's 1 if it says none.
        assert_eq!(of("rate = 0.5\n").2, 0.5);
        assert_eq!(of("").2, 8.0);
        let lfo = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\n";
        assert!(matches!(load(lfo).model.instruments[0].modulators[0].source, ModSourceSlot::Lfo { rate, .. } if rate == 1.0));
    }

    #[test]
    fn a_step_sequencers_values_that_are_not_finite_numbers_are_refused_with_their_place() {
        let Err(error) = try_load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"steps\"\nsteps = [0.5, nan]\n") else { panic!("a value that is not a number is refused") };
        let error = format!("{error:#}");
        assert!(error.contains("instrument[0].modulator[0].steps[1]") && error.contains("is not a finite number"), "{error}");
    }

    #[test]
    fn a_macro_value_that_is_not_a_finite_number_is_refused_with_its_place() {
        let Err(error) = try_load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"macro\"\nvalue = nan\n") else { panic!("a value that is not a number is refused") };
        let error = format!("{error:#}");
        assert!(error.contains("instrument[0].modulator[0].value") && error.contains("is not a finite number"), "{error}");
    }

    #[test]
    fn a_lane_and_a_group_that_a_file_mutes_or_solos_are_so_in_the_model_and_the_engine_and_the_others_are_not() {
        let run = load(
            "[[group]]\nmute = true\n[[group]]\nsolo = true\n[[group]]\n\
             [[instrument]]\nplugin = \"builtin:osc\"\nmute = true\n\
             [[instrument]]\nplugin = \"builtin:osc\"\nsolo = true\n\
             [[instrument]]\nplugin = \"builtin:osc\"\nmute = true\nsolo = true\n\
             [[instrument]]\nplugin = \"builtin:osc\"\n",
        );
        let lanes: Vec<(bool, bool)> = run.model.instruments.iter().map(|lane| (lane.muted, lane.solo)).collect();
        assert_eq!(lanes, [(true, false), (false, true), (true, true), (false, false)]);
        let groups: Vec<(bool, bool)> = run.model.groups.iter().map(|group| (group.muted, group.solo)).collect();
        assert_eq!(groups, [(true, false), (false, true), (false, false)]);
        // (The engine starts every lane and group neither muted nor soloed, so only what a file turns on is sent.)
        let sent: Vec<&str> = run.commands.iter().map(String::as_str).filter(|c| c.starts_with("SetMute") || c.starts_with("SetSolo") || c.starts_with("SetGroupMute") || c.starts_with("SetGroupSolo")).collect();
        assert_eq!(
            sent,
            [
                "SetGroupMute group=0 muted=true",
                "SetGroupSolo group=1 solo=true",
                "SetMute inst=0 muted=true",
                "SetSolo inst=1 solo=true",
                "SetMute inst=2 muted=true",
                "SetSolo inst=2 solo=true",
            ]
        );
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
                "InsertModulator inst=1 index=0 source=Lfo { waveform: Sine, rate: 3.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddInstrument range=None",
                "SwapInstrument inst=2 plugin=Oscillator bufs=2 remap=false",
                // Group modulators come last, once the members' chains are known. Two targets
                // never resolve and are skipped: `param` without member / bus, and a bus
                // that does not exist.
                "InsertGroupModulator group=0 index=0 source=Lfo { waveform: Triangle, rate: 0.5, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddGroupModTarget group=0 mod=0 kind=GroupMember { member: 0, slot: 0, param_index: 1 } depth=0.400 base=0.000 range=[-24.000, 24.000]",
                "AddGroupModTarget group=0 mod=0 kind=GroupMember { member: 1, slot: 1, param_index: 0 } depth=-0.300 base=1.000 range=[0.000, 1.000]",
                "AddGroupModTarget group=0 mod=0 kind=GroupBus { effect_index: 0, param_index: 2 } depth=0.250 base=0.350 range=[0.000, 1.000]",
                "AddGroupModTarget group=0 mod=0 kind=GroupMemberMod { member: 1, mod_index: 0, field: Rate } depth=0.200 base=3.000 range=[0.010, 50.000]",
                "InsertGroupModulator group=0 index=1 source=Envelope { attack: 0.02, decay: 0.4, sustain: 0.5, release: 1.0, state: Idle, level: 0.0, notes_held: 0 }",
                "AddGroupModTarget group=0 mod=1 kind=ModulatorRate { mod_index: 0 } depth=0.300 base=0.500 range=[0.010, 50.000]",
                "AddGroupModTarget group=0 mod=1 kind=ModulatorDepth { mod_index: 0, target_index: 2 } depth=0.100 base=0.250 range=[0.000, 1.000]",
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
                "InsertModulator inst=0 index=0 source=Lfo { waveform: Sine, rate: 2.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddModTarget inst=0 mod=0 kind=PluginParam { slot: 0, param_index: 1 } depth=0.100 base=0.000 range=[-24.000, 24.000]",
                "InsertModulator inst=0 index=1 source=Lfo { waveform: Sine, rate: 3.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddModTarget inst=0 mod=1 kind=PluginParam { slot: 1, param_index: 0 } depth=0.200 base=1.000 range=[0.000, 1.000]",
                "InsertModulator inst=0 index=2 source=Lfo { waveform: Sine, rate: 4.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddModTarget inst=0 mod=2 kind=ModulatorRate { mod_index: 1 } depth=0.300 base=3.000 range=[0.010, 50.000]",
            ]
        );
        let lane = &run.model.instruments[0];
        assert_eq!(lane.modulators.len(), 3);
        assert_eq!(lane.modulators[1].targets[0].slot, 1);
    }

    #[test]
    fn a_position_in_the_session_is_the_place_of_the_node_it_is_on() {
        let model = load(PLACED).model;
        let at = |x: f32, y: f32| Some(NodeUi { x, y });
        let place = |node: NodeId| model.ui.get(&node).copied();
        let (group, lane) = (&model.groups[0], &model.instruments[0]);
        assert_eq!(model.master_ui(), at(1200.0, 300.5));
        assert_eq!(place(group.node), at(800.0, 40.0));
        assert_eq!(place(group.effects[0].node), at(1100.0, 40.0));
        assert_eq!(place(group.modulators[0].node), at(800.0, -120.0));
        assert_eq!(place(lane.node), at(0.0, 0.0));
        assert_eq!(place(lane.effects[0].node), at(376.0, 0.25));
        assert_eq!(place(lane.modulators[0].node), at(376.0, -200.0));
        // The lane the session says nothing of has none, and nothing else is in the table.
        assert_eq!(place(model.instruments[1].node), None);
        assert_eq!(model.ui.len(), 7);
        // A session with no positions has no places.
        assert!(load(GROUPS).model.ui.is_empty());
    }

    /// The index the plugin of a slot has for the parameter of this name.
    fn index_of(slot: &PluginSlot, name: &str) -> u32 {
        slot.params.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("{} has no parameter {name}", slot.name)).index
    }

    #[test]
    fn the_parameters_a_session_pins_are_pinned_on_the_plugin_they_are_for_in_the_order_they_are_named() {
        let model = load(PINNED).model;
        let (group, lane, plain) = (&model.groups[0], &model.instruments[0], &model.instruments[1]);
        let osc = lane.instrument.as_ref().unwrap();
        assert_eq!(osc.pinned, [index_of(osc, "sustain"), index_of(osc, "detune"), index_of(osc, "waveform")]);
        // A name that the plugin has no parameter of, and a name twice, are no pins (and the order is that of the first time).
        let filter = &lane.effects[0];
        assert_eq!(filter.pinned, [index_of(filter, "resonance"), index_of(filter, "cutoff")]);
        let bus = &group.effects[0];
        assert_eq!(bus.pinned, [index_of(bus, "drive"), index_of(bus, "cutoff")]);
        // The lane the session says nothing of has none, and a session from before pins has none.
        assert!(plain.instrument.as_ref().unwrap().pinned.is_empty());
        assert!(load(GROUPS).model.instruments.iter().flat_map(|lane| lane.instrument.iter().chain(&lane.effects)).all(|slot| slot.pinned.is_empty()));
    }

    #[test]
    fn a_pin_for_a_parameter_the_plugin_hides_is_no_pin() {
        let mut run = load("[[instrument]]\nplugin = \"builtin:osc\"\npinned = [\"sustain\"]\n");
        let osc = run.model.instruments[0].instrument.as_mut().unwrap();
        // (What a plugin marks "(locked)" is not in the model's list of parameters, so no name of it finds one.)
        let sustain = index_of(osc, "sustain");
        assert_eq!(osc.pinned, [sustain], "the session pinned it");
        osc.params.retain(|p| p.name != "sustain");
        // (Pinning is saying which are pinned, not adding to them: the sustain that was pinned is not any more.)
        osc.pin_names(&["sustain".to_string(), "detune".to_string()]);
        assert_eq!(osc.pinned, [index_of(osc, "detune")], "no sustain (index {sustain}) any more");
    }

    #[test]
    fn a_separator_is_no_pin_since_it_is_no_control() {
        let mut osc = plugin::load("builtin:osc", 48_000.0, 256, &Default::default()).unwrap();
        let mut slot = fresh_slot(osc.as_mut(), "builtin:osc", NodeId(7));
        slot.params.push(ParamSlot { name: "Envelope".into(), index: 99, kind: ParamKind::Separator, ..Default::default() });
        slot.pin_names(&["Envelope".to_string(), "detune".to_string()]);
        assert_eq!(slot.pinned, [index_of(&slot, "detune")]);
    }

    #[test]
    fn a_name_that_two_parameters_have_pins_the_first_of_them() {
        // (As the name of a modulator's target is: some plugins name two parameters alike, and a file has names to go by.)
        let mut osc = plugin::load("builtin:osc", 48_000.0, 256, &Default::default()).unwrap();
        let mut slot = fresh_slot(osc.as_mut(), "builtin:osc", NodeId(7));
        let first = index_of(&slot, "detune");
        slot.params.push(ParamSlot { name: "detune".into(), index: 77, ..Default::default() });
        slot.pin_names(&["detune".to_string()]);
        assert_eq!(slot.pinned, [first]);
    }

    #[test]
    fn the_names_of_the_pins_are_the_names_of_the_parameters_pinned_in_the_order_they_were_pinned() {
        let mut osc = plugin::load("builtin:osc", 48_000.0, 256, &Default::default()).unwrap();
        let mut slot = fresh_slot(osc.as_mut(), "builtin:osc", NodeId(7));
        slot.pinned = vec![index_of(&slot, "pan"), index_of(&slot, "attack")];
        assert_eq!(slot.pinned_names(), ["pan", "attack"]);
        // (An index the plugin has no parameter of names nothing.)
        slot.pinned.push(12_345);
        assert_eq!(slot.pinned_names(), ["pan", "attack"]);
    }

    #[test]
    fn a_plugin_that_is_just_loaded_has_nothing_pinned() {
        let mut osc = plugin::load("builtin:osc", 48_000.0, 256, &Default::default()).unwrap();
        assert!(fresh_slot(osc.as_mut(), "builtin:osc", NodeId(7)).pinned.is_empty());
    }

    #[test]
    fn a_position_on_a_modulator_that_an_older_session_nested_under_an_effect_is_kept() {
        let run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect.modulator]]
ui = { x = 5, y = 6 }
"#,
        );
        let lane = &run.model.instruments[0];
        assert_eq!(run.model.ui.get(&lane.modulators[0].node), Some(&NodeUi { x: 5.0, y: 6.0 }));
    }

    #[test]
    fn a_lane_with_no_plugin_still_has_the_modulators_the_file_gives_it() {
        // A lane the TUI cleared keeps its effects and its modulators, and a save writes them under the bare
        // `[[instrument]]`: the file has to give them back.
        let run = load(
            r#"
[[instrument]]
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "lfo"
rate = 2.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
[[instrument.modulator]]
type = "envelope"
"#,
        );
        assert_eq!(
            run.commands,
            [
                "AddInstrument range=None",
                "InsertEffect inst=0 index=0 plugin=Filter mix=1.000",
                "InsertModulator inst=0 index=0 source=Lfo { waveform: Sine, rate: 2.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddModTarget inst=0 mod=0 kind=PluginParam { slot: 1, param_index: 0 } depth=0.400 base=1.000 range=[0.000, 1.000]",
                "InsertModulator inst=0 index=1 source=Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }",
            ]
        );
        let lane = &run.model.instruments[0];
        assert!(lane.instrument.is_none());
        assert_eq!(lane.modulators.len(), 2);
        assert_eq!((lane.modulators[0].targets[0].slot, lane.modulators[0].targets[0].param_name.as_str()), (1, "cutoff"));
        run.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_to_a_modulators_rate_stage_or_depth_is_centred_on_what_the_file_has_there() {
        // m0 drives, one cable each, the rate of m1 (an LFO at 2 Hz), the four stages of m2 (an
        // envelope) and the depth of m1's first cable (0.4). A cable's modulation is centred on the
        // value it drives, and that is the value the file gives the field, not a default.
        let run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
mod_rate = 1
[[instrument.modulator.target]]
mod_attack = 2
[[instrument.modulator.target]]
mod_decay = 2
[[instrument.modulator.target]]
mod_sustain = 2
[[instrument.modulator.target]]
mod_release = 2
[[instrument.modulator.target]]
mod_depth = [1, 0]
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
param = "detune"
depth = 0.4
[[instrument.modulator]]
type = "envelope"
attack = 0.2
decay = 0.4
sustain = 0.6
release = 0.8
"#,
        );
        let base = |c: &String| c.split("base=").nth(1).unwrap().split(' ').next().unwrap().to_string();
        let bases: Vec<String> = run.commands.iter().filter(|c| c.starts_with("AddModTarget inst=0 mod=0 ")).map(base).collect();
        assert_eq!(bases, ["2.000", "0.200", "0.400", "0.600", "0.800", "0.400"]);
    }

    #[test]
    fn a_cable_to_a_modulator_the_file_does_not_have_is_centred_on_the_default() {
        let run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 3.0
[[instrument.modulator.target]]
mod_rate = 7
"#,
        );
        assert!(run.commands.iter().any(|c| c.contains("kind=ModulatorRate { mod_index: 7 } depth=0.500 base=1.000 range=[0.010, 50.000]")), "{:#?}", run.commands);
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
                "InsertModulator inst=0 index=2 source=MidiLearn { source: Some(PitchBend), value: 0.5, learning: false }",
                "InsertModulator inst=0 index=3 source=MidiLearn { source: Some(Aftertouch), value: 0.0, learning: false }",
                "InsertModulator inst=0 index=4 source=MidiLearn { source: Some(Velocity), value: 0.0, learning: false }",
                "AddModTarget inst=0 mod=4 kind=PluginParam { slot: 0, param_index: 2 } depth=0.500 base=1.000 range=[0.000, 1.000]",
                &format!("InsertModulator inst=0 index=5 source=MidiLearn {{ source: Some(Key), value: {:?}, learning: false }}", 60.0f32 / 127.0),
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
                "SetPatternLength inst=0 beats=2.0",
                "SetPattern inst=0 events=2 length_samples=64000 base_note=Some(60) in_key=true",
                "SetPatternEnabled inst=0 enabled=true",
                "SetPatternLooping inst=0 looping=false",
                // A lane with no plugin is still a lane.
                "AddInstrument range=None",
                // The tempo is told once, when everything else has been (it is the first pattern's, 90).
                "SetGlobalBpm bpm=90.0",
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
        assert!(matches!(&waveform.kind, ParamKind::Enum(labels) if labels.len() == 5), "enums keep their labels");
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
                "SetPatternLength inst=0 beats=4.0",
                "SetPattern inst=0 events=2 length_samples=115200 base_note=Some(60) in_key=false",
                "SetPatternEnabled inst=0 enabled=false",
                "SetGlobalBpm bpm=100.0",
            ]
        );
    }

    /// Every `SetGlobalBpm` that a session sent the audio thread, as the tempo it says.
    fn tempo_commands(run: &Run) -> Vec<String> {
        run.commands.iter().filter(|command| command.starts_with("SetGlobalBpm")).cloned().collect()
    }

    #[test]
    fn the_tempo_of_a_session_is_its_bpm_and_the_audio_thread_is_told_it_once() {
        let run = load("bpm = 96.5\n[[instrument]]\nplugin = \"builtin:osc\"\n");
        assert_eq!(run.model.bpm, 96.5);
        assert_eq!(tempo_commands(&run), ["SetGlobalBpm bpm=96.5"]);
    }

    #[test]
    fn the_bpm_of_a_session_is_the_tempo_whatever_its_patterns_were_recorded_at() {
        // (SETTINGS has a pattern that was recorded at 90: it stays, and its length is counted in it.)
        let run = load(&format!("bpm = 100.0\n{SETTINGS}"));
        assert_eq!(run.model.bpm, 100.0);
        assert_eq!(run.model.instruments[0].pattern.as_ref().unwrap().bpm, 90.0);
        assert!(run.commands.contains(&"SetPattern inst=0 events=2 length_samples=64000 base_note=Some(60) in_key=true".to_string()), "{:#?}", run.commands);
        assert_eq!(tempo_commands(&run), ["SetGlobalBpm bpm=100.0"]);
    }

    #[test]
    fn the_audio_thread_is_told_the_first_patterns_tempo_once_when_there_is_no_bpm() {
        let lane = |bpm: &str| {
            format!("[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.pattern]\nbpm = {bpm}\nlength_beats = 4.0\nenabled = true\n")
        };
        let run = load(&format!("{}{}", lane("90.0"), lane("140.0")));
        assert_eq!(run.model.bpm, 90.0);
        assert_eq!(tempo_commands(&run), ["SetGlobalBpm bpm=90.0"], "(it was the last pattern's, 140, that the audio thread was left with)");
        assert_eq!(run.model.instruments[1].pattern.as_ref().unwrap().bpm, 140.0, "each pattern keeps its own");
    }

    #[test]
    fn a_session_with_no_tempo_at_all_tells_the_audio_thread_none() {
        let run = load("[[instrument]]\nplugin = \"builtin:osc\"\n");
        assert_eq!(run.model.bpm, 120.0);
        assert_eq!(tempo_commands(&run), Vec::<String>::new(), "it starts at 120");
    }

    #[test]
    fn a_pattern_recorded_at_a_tempo_there_cannot_be_gives_the_session_the_nearest_one_and_keeps_its_own() {
        let run = load("[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.pattern]\nbpm = 5.0\nlength_beats = 4.0\nenabled = true\n");
        assert_eq!(run.model.bpm, 20.0);
        assert_eq!(run.model.instruments[0].pattern.as_ref().unwrap().bpm, 5.0);
        assert_eq!(tempo_commands(&run), ["SetGlobalBpm bpm=20.0"]);
    }

    #[test]
    fn the_bpm_of_a_file_is_held_to_the_tempos_there_can_be() {
        for (text, tempo) in [("0", 20.0), ("-5", 20.0), ("19.9", 20.0), ("20", 20.0), ("133.3", 133.3), ("300", 300.0), ("301", 300.0), ("1e30", 300.0)] {
            let run = load(&format!("bpm = {text}\n[[instrument]]\nplugin = \"builtin:osc\"\n"));
            assert_eq!(run.model.bpm, tempo, "bpm = {text}");
            assert_eq!(tempo_commands(&run), [format!("SetGlobalBpm bpm={tempo:?}")], "bpm = {text}");
        }
    }

    fn tempo_of(session: &str) -> f32 {
        load(session).model.bpm
    }

    #[test]
    fn the_tempo_starts_at_the_first_recorded_patterns() {
        let lane = |bpm: &str| {
            format!(
                "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.pattern]\nbpm = {bpm}\nlength_beats = 4.0\nenabled = true\n"
            )
        };
        let plain = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        // The first lane that has a pattern decides, not the first lane.
        assert_eq!(tempo_of(&format!("{plain}{}{}", lane("90.0"), lane("140.0"))), 90.0);
        assert_eq!(tempo_of(&lane("140.0")), 140.0);
    }

    #[test]
    fn the_tempo_is_120_without_a_recorded_pattern() {
        assert_eq!(tempo_of("[[instrument]]\nplugin = \"builtin:osc\"\n"), 120.0);
        assert_eq!(tempo_of(""), 120.0);
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
                "InsertModulator inst=0 index=0 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
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
    fn an_effect_with_other_outputs_than_the_chain_has_fails_the_load_and_the_error_says_where_it_is_and_why() {
        // (The audio thread turns such an effect away, so a model that kept it would show an effect that is not there, and the parameters of the effects after it
        // would be told to places the engine has not, or to another effect: f68.)
        let err = try_load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n[[instrument.effect]]\nplugin = \"builtin:mono-effect\"\n",
        )
        .expect_err("a chain of two channels has no place for it");
        assert_eq!(format!("{err:#}"), "instrument[0].effect[1]: MonoEffect has 1 audio output, and an effect here needs 2");

        // The place is the place in the file: the second lane's first effect.
        let err = try_load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:mono-effect\"\n",
        )
        .expect_err("the same in another lane");
        assert_eq!(format!("{err:#}"), "instrument[1].effect[0]: MonoEffect has 1 audio output, and an effect here needs 2");
    }

    #[test]
    fn a_bus_effect_with_other_outputs_than_the_chain_has_fails_the_load_the_same_way() {
        let err = try_load(
            "[[group]]\n[[group.effect]]\nplugin = \"builtin:reverb\"\n[[group]]\n[[group.effect]]\nplugin = \"builtin:filter\"\n[[group.effect]]\nplugin = \"builtin:reverb\"\n\
             [[group.effect]]\nplugin = \"builtin:mono-effect\"\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 1\n",
        )
        .expect_err("a group's bus is stereo too");
        // (The second group's third effect: the place of the group and the place of the effect are told apart.)
        assert_eq!(format!("{err:#}"), "group[1].effect[2]: MonoEffect has 1 audio output, and an effect here needs 2");
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
