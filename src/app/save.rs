//! Saving a session: turn the model back into what a session file holds.
//!
//! The mirror of `app::load`. The loader reads a file into the model; `to_save`
//! reads the model into the plain structs `session::save` writes. Nothing here
//! touches the disk or the terminal, so any front-end (and a test) can serialize
//! a session.

use crate::app::model::{ModSourceSlot, ModulatorSlot, NodeId, ParamSlot, PluginSlot, SessionModel};
use crate::plugin::chain::MidiBindSource;
use crate::session::{
    NodeUi, SaveEffect, SaveGroup, SaveInstrument, SaveInstrumentSlot, SaveModSource, SaveModTarget, SaveModulator,
    SavePattern,
};
use std::collections::BTreeMap;

/// The lanes and groups of a session as `session::save` writes them, with where each node is drawn. The piano
/// scale is not part of the model; it goes to `save` separately, and the master's place is `SessionModel::master_ui`.
pub fn to_save(model: &SessionModel) -> (Vec<SaveInstrumentSlot>, Vec<SaveGroup>) {
    let ui = &model.ui;
    let save_groups = model
        .groups
        .iter()
        .map(|g| SaveGroup {
            name: g.name.clone(),
            volume: g.volume,
            pan: g.pan,
            effects: g.effects.iter().map(|fx| effect_to_save(fx, ui)).collect(),
            modulators: modulators_to_save(&g.modulators, ui),
            ui: ui.get(&g.node).copied(),
        })
        .collect();
    let save_instruments = model
        .instruments
        .iter()
        .map(|lane| SaveInstrumentSlot {
            range: lane.range,
            transpose: lane.transpose,
            group: lane.group,
            modulators: modulators_to_save(&lane.modulators, ui),
            instrument: lane.instrument.as_ref().map(|inst| SaveInstrument {
                plugin: inst.id.clone(),
                volume: lane.volume,
                preset: inst.current_preset.clone(),
                params: overrides(&inst.params),
                pinned: inst.pinned_names(),
                pitch_bend_range: lane.pitch_bend_range,
                remap: lane.remap.clone(),
            }),
            effects: lane.effects.iter().map(|fx| effect_to_save(fx, ui)).collect(),
            ui: ui.get(&lane.node).copied(),
            pattern: lane.pattern.as_ref().map(|p| SavePattern {
                bpm: p.bpm,
                length_beats: p.length_beats,
                looping: p.looping,
                base_note: p.base_note,
                events: p.events.clone(),
                enabled: p.enabled,
                in_key: p.in_key,
            }),
        })
        .collect();
    (save_instruments, save_groups)
}

/// The parameters whose value differs from the baseline the loader recorded (the
/// plugin's own value once its preset is applied): what a file keeps as `params`.
fn overrides(params: &[ParamSlot]) -> Vec<(String, f32)> {
    params
        .iter()
        .filter(|p| (p.value - p.default).abs() > f32::EPSILON)
        .map(|p| (p.name.clone(), p.value))
        .collect()
}

fn effect_to_save(fx: &PluginSlot, ui: &BTreeMap<NodeId, NodeUi>) -> SaveEffect {
    SaveEffect {
        plugin: fx.id.clone(),
        mix: fx.mix,
        preset: fx.current_preset.clone(),
        params: overrides(&fx.params),
        pinned: fx.pinned_names(),
        ui: ui.get(&fx.node).copied(),
    }
}

fn modulators_to_save(mods: &[ModulatorSlot], ui: &BTreeMap<NodeId, NodeUi>) -> Vec<SaveModulator> {
    mods.iter()
        .map(|m| {
            let source = match &m.source {
                ModSourceSlot::Lfo { waveform, rate } => {
                    SaveModSource::Lfo { waveform: waveform.name().to_string(), rate: *rate }
                }
                ModSourceSlot::Envelope { attack, decay, sustain, release } => SaveModSource::Envelope {
                    attack: *attack,
                    decay: *decay,
                    sustain: *sustain,
                    release: *release,
                },
                ModSourceSlot::MidiLearn { source, .. } => SaveModSource::MidiLearn {
                    cc: match source {
                        Some(MidiBindSource::Cc(n)) => Some(*n),
                        _ => None,
                    },
                    pitch_bend: matches!(source, Some(MidiBindSource::PitchBend)),
                    aftertouch: matches!(source, Some(MidiBindSource::Aftertouch)),
                },
            };
            SaveModulator {
                source,
                targets: m
                    .targets
                    .iter()
                    .map(|t| SaveModTarget {
                        kind: t.kind.clone(),
                        label: t.param_name.clone(),
                        depth: t.depth,
                        slot: t.slot,
                    })
                    .collect(),
                ui: ui.get(&m.node).copied(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::model::{ModSourceSlot, SessionModel};
    use crate::app::testutil::{
        CROSS_MOD, DISABLED_PATTERN, GROUPS, LEGACY, MIDI, PINNED, PLACED, SETTINGS, assert_same_model, load, try_load,
    };
    use crate::plugin::chain::MidiBindSource;
    use crate::session;

    const BASIC: &str = include_str!("../../examples/basic.toml");
    const SPLIT: &str = include_str!("../../examples/split.toml");

    /// Values that are not exact in binary, on an instrument and its pattern.
    const ODD_TEMPO: &str = r#"
[[instrument]]
plugin = "builtin:osc"
volume = 0.7
[instrument.pattern]
bpm = 93.7
length_beats = 3.3
base_note = "C4"
[[instrument.pattern.events]]
frame = 0
status = "on"
note = "C4"
velocity = 100
"#;

    const SCENES: &[(&str, &str)] = &[
        ("basic", BASIC),
        ("split", SPLIT),
        ("groups", GROUPS),
        ("legacy", LEGACY),
        ("midi", MIDI),
        ("settings", SETTINGS),
        ("cross-mod", CROSS_MOD),
        ("disabled-pattern", DISABLED_PATTERN),
        ("odd-tempo", ODD_TEMPO),
        ("placed", PLACED),
        ("pinned", PINNED),
    ];

    /// The file a save of this model writes.
    fn saved(model: &SessionModel) -> String {
        let (instruments, groups) = to_save(model);
        session::to_toml(&instruments, &groups, None, model.master_ui()).unwrap()
    }

    #[test]
    fn every_scene_survives_a_save_and_a_reload() {
        for (name, text) in SCENES {
            let first = load(text);
            let file = saved(&first.model);
            let second =
                try_load(&file).unwrap_or_else(|e| panic!("{name}: the saved file does not load: {e:#}\n{file}"));
            assert_eq!(first.commands, second.commands, "{name}: the saved file tells the engine something else:\n{file}");
            assert_same_model(name, &first.model, &second.model);
            assert_eq!(file, saved(&second.model), "{name}: saving twice gives a different file");
        }
    }

    // What a saved file looks like. Hand-edited files get rewritten on every save, so
    // the writer has to keep to the style the docs show.

    #[test]
    fn numbers_are_written_the_way_they_were_typed() {
        let file = saved(&load(GROUPS).model);
        for line in ["volume = 0.8", "pan = -0.3", "mix = 0.6", "attack = 0.02", "decay = 0.4", "depth = -0.3"] {
            assert!(file.lines().any(|l| l == line), "no `{line}` in:\n{file}");
        }
        let file = saved(&load(ODD_TEMPO).model);
        for line in ["volume = 0.7", "bpm = 93.7", "length_beats = 3.3"] {
            assert!(file.lines().any(|l| l == line), "no `{line}` in:\n{file}");
        }
        // Whatever the scene: an f32 widened to f64 shows up as a long run of digits.
        for (name, text) in SCENES {
            let file = saved(&load(text).model);
            let noisy: Vec<&str> = file.lines().filter(|l| longest_fraction(l) >= 8).collect();
            assert!(noisy.is_empty(), "{name}: numbers with float noise:\n{}", noisy.join("\n"));
        }
    }

    /// Digits after the longest decimal point on the line.
    fn longest_fraction(line: &str) -> usize {
        line.split('.').skip(1).map(|rest| rest.chars().take_while(char::is_ascii_digit).count()).max().unwrap_or(0)
    }

    #[test]
    fn remapped_keys_are_written_as_inline_tables() {
        let file = saved(&load(SETTINGS).model);
        assert!(file.contains("\"G#4\" = { note = \"G4\", detune = 1.0 }"), "{file}");
    }

    #[test]
    fn a_cross_mod_depth_target_stays_on_one_line() {
        let file = saved(&load(GROUPS).model);
        assert!(file.contains("mod_depth = [0, 2]"), "{file}");
    }

    #[test]
    fn a_lane_whose_instrument_was_cleared_keeps_its_modulators_through_a_save_and_a_reload() {
        // What `d` on the instrument of a lane that has effects does: the plugin goes; the lane, its effects and
        // its modulators stay.
        let mut first = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "lfo"
rate = 2.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
"#,
        );
        first.model.instruments[0].instrument = None;
        let file = saved(&first.model);
        let second = try_load(&file).unwrap_or_else(|e| panic!("the saved file does not load: {e:#}\n{file}"));
        let rack = |model: &SessionModel| {
            model.instruments[0]
                .modulators
                .iter()
                .map(|m| (format!("{:?}", m.source), m.targets.iter().map(|t| (t.slot, t.param_name.clone(), t.depth)).collect::<Vec<_>>()))
                .collect::<Vec<_>>()
        };
        assert_eq!(rack(&first.model).len(), 1);
        assert_eq!(rack(&second.model), rack(&first.model), "the modulator and its cable are in the file:\n{file}");
        assert!(second.model.instruments[0].instrument.is_none());
        assert_eq!(second.model.instruments[0].effects.len(), 1);
        assert_eq!(file, saved(&second.model), "and saving it again gives the same file");
    }

    #[test]
    fn the_places_of_the_nodes_are_saved_on_the_nodes_they_are_for() {
        let file = saved(&load(PLACED).model);
        let lines = |prefix: &str| file.lines().filter(|l| l.starts_with(prefix)).collect::<Vec<_>>();
        assert_eq!(lines("master = "), ["master = { x = 1200, y = 300.5 }"], "{file}");
        assert_eq!(
            lines("ui = "),
            [
                "ui = { x = 0, y = 0 }",
                "ui = { x = 376, y = -200 }",
                "ui = { x = 376, y = 0.25 }",
                "ui = { x = 800, y = 40 }",
                "ui = { x = 1100, y = 40 }",
                "ui = { x = 800, y = -120 }",
            ],
            "the lane, its modulator and its effect, then the group, its effect and its modulator:\n{file}"
        );
    }

    #[test]
    fn the_place_of_a_node_that_has_gone_is_not_saved() {
        let mut run = load(PLACED);
        let gone = run.model.instruments[0].effects.remove(0).node;
        assert!(run.model.ui.contains_key(&gone), "(ids are not reused, so the table is not tidied)");
        let file = saved(&run.model);
        assert!(!file.contains("y = 0.25"), "{file}");
        assert_eq!(file.lines().filter(|l| l.starts_with("ui = ")).count(), 5, "{file}");
        // And the master stays, with nothing else changed in the file.
        assert!(file.contains("master = { x = 1200, y = 300.5 }"), "{file}");
    }

    #[test]
    fn a_session_with_no_places_saves_none() {
        for (name, text) in SCENES {
            if *name == "placed" {
                continue;
            }
            let file = saved(&load(text).model);
            assert!(!file.lines().any(|l| l.starts_with("ui = ") || l == "[ui]"), "{name}:\n{file}");
        }
    }

    #[test]
    fn the_pinned_parameters_are_saved_by_name_in_the_order_they_were_pinned_on_the_plugins_they_are_for() {
        let file = saved(&load(PINNED).model);
        // (The names the file had that were no pins are gone: the model only has pins.)
        for line in [r#"pinned = ["sustain", "detune", "waveform"]"#, r#"pinned = ["resonance", "cutoff"]"#, r#"pinned = ["drive", "cutoff"]"#] {
            assert_eq!(file.lines().filter(|l| *l == line).count(), 1, "{line} in\n{file}");
        }
        assert_eq!(file.lines().filter(|l| l.starts_with("pinned")).count(), 3, "{file}");
        // Each on its own plugin: the instrument's before its effect's, and the bus effect's with the group.
        let at = |line: &str| file.find(line).unwrap_or_else(|| panic!("{line}\n{file}"));
        assert!(at(r#"pinned = ["sustain""#) < at("[[instrument.effect]]") && at("[[instrument.effect]]") < at(r#"pinned = ["resonance""#), "{file}");
        assert!(at("[[group]]") < at(r#"pinned = ["drive""#), "{file}");
    }

    #[test]
    fn a_session_with_nothing_pinned_saves_no_pins() {
        for (name, text) in SCENES {
            if *name == "pinned" {
                continue;
            }
            let file = saved(&load(text).model);
            assert!(!file.contains("pinned"), "{name}:\n{file}");
        }
    }

    #[test]
    fn an_effect_that_is_moved_takes_its_pins_with_it() {
        let mut run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
pinned = ["drive"]
[[instrument.effect]]
plugin = "builtin:filter"
pinned = ["cutoff", "resonance"]
"#,
        );
        run.model.instruments[0].effects.swap(0, 1);
        let file = saved(&run.model);
        let first = file.find(r#"pinned = ["cutoff", "resonance"]"#).unwrap();
        let second = file.find(r#"pinned = ["drive"]"#).unwrap();
        assert!(first < second, "{file}");
    }

    #[test]
    fn a_plain_lane_is_saved_as_just_its_plugin() {
        let run = load("[[instrument]]\nplugin = \"builtin:osc\"\n");
        assert_eq!(saved(&run.model), "[[instrument]]\nplugin = \"builtin:osc\"\n");
    }

    #[test]
    fn a_parameter_is_saved_only_while_it_differs_from_its_baseline() {
        let mut run = load("[[instrument]]\nplugin = \"builtin:osc\"\n");
        let sustain = |model: &mut SessionModel| -> usize {
            let params = &mut model.instruments[0].instrument.as_mut().unwrap().params;
            params.iter().position(|p| p.name == "sustain").unwrap()
        };
        let i = sustain(&mut run.model);
        let osc = run.model.instruments[0].instrument.as_mut().unwrap();
        let baseline = osc.params[i].default;
        osc.params[i].value = baseline / 2.0;
        let file = saved(&run.model);
        assert!(file.contains("[instrument.params]") && file.contains("sustain = 0.5"), "{file}");

        let osc = run.model.instruments[0].instrument.as_mut().unwrap();
        osc.params[i].value = baseline;
        assert!(!saved(&run.model).contains("params"), "back at the baseline it is not an override");
    }

    #[test]
    fn an_instruments_preset_is_saved() {
        // No built-in instrument has presets, so set the name the way a preset load does.
        let mut run = load("[[instrument]]\nplugin = \"builtin:osc\"\n");
        run.model.instruments[0].instrument.as_mut().unwrap().current_preset = Some("Warm Pad".into());
        assert!(saved(&run.model).contains("preset = \"Warm Pad\""));
    }

    #[test]
    fn a_control_learned_after_loading_is_saved_as_bound() {
        let mut run = load(MIDI);
        // The second modulator is the armed, unbound one (see `MIDI`).
        let m = &mut run.model.instruments[0].modulators[1];
        assert!(matches!(m.source, ModSourceSlot::MidiLearn { source: None, learning: true }));
        m.source = ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(7)), learning: false };
        let file = saved(&run.model);
        assert!(file.matches("midi_cc = ").count() == 2 && file.contains("midi_cc = 7"), "{file}");
    }

    #[test]
    fn several_remapped_keys_survive_a_save_and_reload() {
        let text = r#"
[[instrument]]
plugin = "builtin:osc"
pitch_bend_range = 2.0
[instrument.remap]
"G#4" = { note = "G4", detune = 1.0 }
"C#2" = { note = "D2", detune = -0.5 }
"F#3" = { note = "F3", detune = 1.0 }
"#;
        let remap = |model: &SessionModel| {
            let mut keys: Vec<_> =
                model.instruments[0].remap.iter().map(|(k, v)| (k.clone(), v.note.clone(), v.detune)).collect();
            keys.sort_by(|a, b| a.0.cmp(&b.0));
            keys
        };
        let first = load(text);
        let second = load(&saved(&first.model));
        assert_eq!(remap(&first.model).len(), 3);
        assert_eq!(remap(&first.model), remap(&second.model));
        assert_eq!(first.commands, second.commands, "the same detune values still share channels");
    }
}
