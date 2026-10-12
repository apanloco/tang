//! Saving a session: turn the model back into what a session file holds.
//!
//! The mirror of `app::load`. The loader reads a file into the model; `to_save`
//! reads the model into the plain structs `session::save` writes. Nothing here
//! touches the disk or the terminal, so any front-end (and a test) can serialize
//! a session.

use crate::app::model::{GroupNode, InstrumentNode, ModSourceSlot, ModulatorSlot, ParamSlot, PluginSlot};
use crate::plugin::chain::MidiBindSource;
use crate::session::{
    SaveEffect, SaveGroup, SaveInstrument, SaveInstrumentSlot, SaveModSource, SaveModTarget, SaveModulator,
    SavePattern,
};

/// The lanes and groups of a session as `session::save` writes them. The piano
/// scale is not part of the model; it goes to `save` separately.
pub fn to_save(instruments: &[InstrumentNode], groups: &[GroupNode]) -> (Vec<SaveInstrumentSlot>, Vec<SaveGroup>) {
    let save_groups = groups
        .iter()
        .map(|g| SaveGroup {
            name: g.name.clone(),
            volume: g.volume,
            pan: g.pan,
            effects: g.effects.iter().map(effect_to_save).collect(),
            modulators: modulators_to_save(&g.modulators),
        })
        .collect();
    let save_instruments = instruments
        .iter()
        .map(|lane| SaveInstrumentSlot {
            range: lane.range,
            transpose: lane.transpose,
            group: lane.group,
            modulators: modulators_to_save(&lane.modulators),
            instrument: lane.instrument.as_ref().map(|inst| SaveInstrument {
                plugin: inst.id.clone(),
                volume: lane.volume,
                preset: inst.current_preset.clone(),
                params: overrides(&inst.params),
                pitch_bend_range: lane.pitch_bend_range,
                remap: lane.remap.clone(),
            }),
            effects: lane.effects.iter().map(effect_to_save).collect(),
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

fn effect_to_save(fx: &PluginSlot) -> SaveEffect {
    SaveEffect {
        plugin: fx.id.clone(),
        mix: fx.mix,
        preset: fx.current_preset.clone(),
        params: overrides(&fx.params),
    }
}

fn modulators_to_save(mods: &[ModulatorSlot]) -> Vec<SaveModulator> {
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
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::model::{ModSourceSlot, SessionModel};
    use crate::app::testutil::{
        CROSS_MOD, DISABLED_PATTERN, GROUPS, LEGACY, MIDI, SETTINGS, assert_same_model, load, try_load,
    };
    use crate::plugin::chain::MidiBindSource;
    use crate::session;

    const BASIC: &str = include_str!("../../examples/basic.toml");
    const SPLIT: &str = include_str!("../../examples/split.toml");

    const SCENES: &[(&str, &str)] = &[
        ("basic", BASIC),
        ("split", SPLIT),
        ("groups", GROUPS),
        ("legacy", LEGACY),
        ("midi", MIDI),
        ("settings", SETTINGS),
        ("cross-mod", CROSS_MOD),
        ("disabled-pattern", DISABLED_PATTERN),
    ];

    /// The file a save of this model writes.
    fn saved(model: &SessionModel) -> String {
        let (instruments, groups) = to_save(&model.instruments, &model.groups);
        session::to_toml(&instruments, &groups, None).unwrap()
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
