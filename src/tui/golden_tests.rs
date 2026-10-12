//! Golden-render tests for the TUI — the regression net for the controller
//! refactor (`docs/web-ui.md`, phase 0).
//!
//! Each test builds a `State` by hand from builtin plugins (until `run_session`'s
//! loading is extracted, there is no other loader that doesn't need an audio
//! engine), drives it with real key events through `process_event`, draws it
//! into ratatui's `TestBackend`, and snapshots the buffer with `insta` —
//! characters *and* styles, because colour carries meaning here (the piano
//! paints held / root / scale keys with background colour on blank cells).
//!
//! After an intentional UI change, regenerate with
//! `INSTA_UPDATE=always cargo test golden`, then review the snapshot diff.

use std::collections::HashMap;

use crossbeam_channel::{Receiver, unbounded};
use crossterm::event::KeyEvent;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use super::*;
use crate::midi_monitor::MidiMonitor;
use crate::plugin::chain::{LfoWaveform, ModTargetKind};
use crate::scale::ScaleSetting;
use crate::session::parse_range;

const SAMPLE_RATE: f32 = 48_000.0;
const BLOCK: usize = 256;
const WIDTH: u16 = 120;
const HEIGHT: u16 = 32;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A `State` wired to stub engine channels, plus a `TestBackend` terminal.
struct Harness {
    s: State,
    terminal: Terminal<TestBackend>,
    /// The audio thread's end of the command channel: what the TUI asked the
    /// engine to do. Kept alive so `send` never fails or blocks.
    cmd_rx: Receiver<GraphCommand>,
}

impl Harness {
    fn new(scene: (Vec<LoadedInstrument>, Vec<LoadedGroup>)) -> Self {
        let (instruments, groups) = scene;
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (midi_tx, _midi_rx) = crossbeam_channel::bounded(1024);
        let (_catalog_tx, catalog_rx) = unbounded();
        let (_pattern_tx, pattern_rx) = unbounded();
        let (_learn_tx, learn_rx) = unbounded();
        let mut s = State::new(
            instruments,
            groups,
            cmd_tx,
            midi_tx,
            plugin::Runtime::default(),
            SAMPLE_RATE,
            BLOCK,
            None,
            catalog_rx,
            pattern_rx,
            learn_rx,
            Arc::new(HeldNotes::new()),
            Arc::new(PianoFilter::new(ScaleSetting::default(), PianoMode::Highlight)),
            Arc::new(MidiMonitor::new(false)),
        );
        // A finished scan with an empty catalog unless a test fills it in.
        s.catalog_scanning = false;
        let terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        Harness { s, terminal, cmd_rx }
    }

    fn press(&mut self, code: KeyCode) -> &mut Self {
        self.press_with(code, KeyModifiers::NONE)
    }

    fn press_with(&mut self, code: KeyCode, modifiers: KeyModifiers) -> &mut Self {
        process_event(&mut self.s, Event::Key(KeyEvent::new(code, modifiers)));
        self
    }

    fn type_chars(&mut self, text: &str) -> &mut Self {
        for c in text.chars() {
            self.press(KeyCode::Char(c));
        }
        self
    }

    fn press_n(&mut self, code: KeyCode, n: usize) -> &mut Self {
        for _ in 0..n {
            self.press(code);
        }
        self
    }

    /// The chain tree as the user reads it.
    fn tree(&self) -> Vec<&str> {
        self.s.tree_entries.iter().map(|e| e.label.as_str()).collect()
    }

    /// Move the chain cursor onto `addr` with real Up/Down presses.
    fn goto(&mut self, addr: TreeAddress) -> &mut Self {
        let target = self
            .s
            .tree_entries
            .iter()
            .position(|e| e.address == addr)
            .unwrap_or_else(|| panic!("{addr:?} is not in the tree"));
        while self.s.chain_state.selected < target {
            self.press(KeyCode::Down);
        }
        while self.s.chain_state.selected > target {
            self.press(KeyCode::Up);
        }
        self
    }

    /// Everything the TUI has sent to the audio thread since the last call.
    fn commands(&self) -> Vec<String> {
        self.cmd_rx.try_iter().map(|c| describe(&c)).collect()
    }

    /// Draw a frame and dump it (text + style map).
    fn dump(&mut self) -> String {
        self.terminal.draw(|frame| draw(frame, &mut self.s)).unwrap();
        dump_buffer(self.terminal.backend().buffer())
    }
}

/// Compare the current frame against `snapshots/<name>.snap`.
fn assert_golden(name: &str, h: &mut Harness) {
    let dump = h.dump();
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.set_omit_expression(true);
    settings.bind(|| insta::assert_snapshot!(name, dump));
}

/// One line per command the scenario tests care about. Anything else shows as
/// `<other>`, which still changes the snapshot when an unexpected command shows
/// up (`GraphCommand` carries boxed plugins, so it has no `Debug`).
fn describe(cmd: &GraphCommand) -> String {
    use GraphCommand as C;
    match cmd {
        C::SetParameter { inst, slot, param_index, value } => {
            format!("SetParameter inst={inst} slot={slot} param={param_index} value={value:.3}")
        }
        C::SetGroupParameter { group, index, param_index, value } => {
            format!("SetGroupParameter group={group} fx={index} param={param_index} value={value:.3}")
        }
        C::SetVolume { inst, value } => format!("SetVolume inst={inst} value={value:.3}"),
        C::SetMix { inst, slot, value } => format!("SetMix inst={inst} slot={slot} value={value:.3}"),
        C::InsertModulator { inst, index, .. } => format!("InsertModulator inst={inst} index={index}"),
        C::RemoveModulator { inst, index } => format!("RemoveModulator inst={inst} index={index}"),
        C::InsertGroupModulator { group, index, .. } => {
            format!("InsertGroupModulator group={group} index={index}")
        }
        C::RemoveGroupModulator { group, index } => {
            format!("RemoveGroupModulator group={group} index={index}")
        }
        C::RemoveGroupEffect { group, index } => format!("RemoveGroupEffect group={group} index={index}"),
        C::RemoveEffect { inst, index } => format!("RemoveEffect inst={inst} index={index}"),
        C::RemoveInstrument { inst } => format!("RemoveInstrument inst={inst}"),
        C::RemoveGroup { group } => format!("RemoveGroup group={group}"),
        C::ClearInstrument { inst } => format!("ClearInstrument inst={inst}"),
        C::SwapInstrument { inst, .. } => format!("SwapInstrument inst={inst}"),
        C::InsertEffect { inst, index, mix, .. } => {
            format!("InsertEffect inst={inst} index={index} mix={mix:.3}")
        }
        C::InsertGroupEffect { group, index, mix, .. } => {
            format!("InsertGroupEffect group={group} index={index} mix={mix:.3}")
        }
        C::SetLaneGroup { inst, group } => format!("SetLaneGroup inst={inst} group={group:?}"),
        C::SetGroupMix { group, index, value } => {
            format!("SetGroupMix group={group} fx={index} value={value:.3}")
        }
        C::SetGroupVolume { group, value } => format!("SetGroupVolume group={group} value={value:.3}"),
        C::SetInstrumentRange { inst, range } => format!("SetInstrumentRange inst={inst} range={range:?}"),
        C::SetModulatorRate { inst, mod_index, rate } => {
            format!("SetModulatorRate inst={inst} mod={mod_index} rate={rate:.3}")
        }
        C::SetModTargetDepth { inst, mod_index, target_index, depth } => {
            format!("SetModTargetDepth inst={inst} mod={mod_index} target={target_index} depth={depth:.3}")
        }
        C::SetGroupModTargetDepth { group, mod_index, target_index, depth } => {
            format!("SetGroupModTargetDepth group={group} mod={mod_index} target={target_index} depth={depth:.3}")
        }
        _ => "<other>".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Buffer dump: characters + a per-cell style map
// ---------------------------------------------------------------------------

const STYLE_SYMBOLS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Render a buffer as three blocks: the characters (trailing blanks trimmed),
/// a map giving every non-default cell a letter, and the legend for those
/// letters (in order of first appearance, reading left→right, top→bottom).
fn dump_buffer(buf: &Buffer) -> String {
    let symbols: Vec<char> = STYLE_SYMBOLS.chars().collect();
    let mut legend: Vec<(Color, Color, Modifier)> = Vec::new();
    let mut text = Vec::new();
    let mut map = Vec::new();
    for y in 0..buf.area.height {
        let mut t = String::new();
        let mut m = String::new();
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            t.push_str(cell.symbol());
            let key = (cell.fg, cell.bg, cell.modifier);
            m.push(if key == (Color::Reset, Color::Reset, Modifier::empty()) {
                '.'
            } else {
                let i = legend.iter().position(|k| *k == key).unwrap_or_else(|| {
                    legend.push(key);
                    legend.len() - 1
                });
                *symbols.get(i).expect("more distinct styles than STYLE_SYMBOLS")
            });
        }
        text.push(t.trim_end().to_string());
        map.push(m.trim_end_matches('.').to_string());
    }
    let legend: Vec<String> = legend
        .iter()
        .enumerate()
        .map(|(i, (fg, bg, modifier))| {
            let modifier = if modifier.is_empty() { "-".to_string() } else { format!("{modifier:?}") };
            format!("{}  fg={fg:?} bg={bg:?} mod={modifier}", symbols[i])
        })
        .collect();
    format!(
        "{}x{}\n── text ──\n{}\n── styles ('.' = default) ──\n{}\n── legend ──\n{}\n",
        buf.area.width,
        buf.area.height,
        text.join("\n"),
        map.join("\n"),
        legend.join("\n"),
    )
}

// ---------------------------------------------------------------------------
// Fixtures: sessions built by hand from builtin plugins
// ---------------------------------------------------------------------------

/// A builtin plugin as the loader hands it to the TUI: baselines are the
/// plugin's own values, `set` are session-file overrides on top.
fn builtin(id: &str, mix: f32, set: &[(&str, f32)]) -> LoadedPlugin {
    let mut p = plugin::builtin::load(id, SAMPLE_RATE, BLOCK).expect("builtin loads");
    let params = p.parameters();
    let baselines: Vec<f32> = params
        .iter()
        .map(|i| p.get_parameter(i.index).unwrap_or(i.default))
        .collect();
    let mut values = baselines.clone();
    for (name, value) in set {
        let i = params
            .iter()
            .position(|i| i.name == *name)
            .unwrap_or_else(|| panic!("{id} has no parameter {name:?}"));
        values[i] = *value;
    }
    LoadedPlugin {
        name: p.name().to_string(),
        id: id.to_string(),
        is_instrument: p.is_instrument(),
        params,
        param_defaults: baselines,
        param_values: values,
        presets: p.presets(),
        current_preset: None,
        mix,
    }
}

fn instrument(plugin: LoadedPlugin) -> LoadedInstrument {
    LoadedInstrument {
        range: None,
        transpose: 0,
        volume: 1.0,
        instrument: Some(plugin),
        effects: vec![],
        modulators: vec![],
        pattern: None,
        pitch_bend_range: 2.0,
        remap: HashMap::new(),
        group: None,
    }
}

fn range(text: &str) -> Option<(u8, u8)> {
    Some(parse_range(text).expect("valid range"))
}

fn lfo(waveform: LfoWaveform, rate: f32, targets: Vec<LoadedModTarget>) -> LoadedModulator {
    LoadedModulator { source: LoadedModSource::Lfo { waveform, rate }, targets }
}

fn adsr(attack: f32, decay: f32, sustain: f32, release: f32, targets: Vec<LoadedModTarget>) -> LoadedModulator {
    LoadedModulator {
        source: LoadedModSource::Envelope { attack, decay, sustain, release },
        targets,
    }
}

fn param<'a>(p: &'a LoadedPlugin, name: &str) -> &'a plugin::ParameterInfo {
    p.params
        .iter()
        .find(|i| i.name == name)
        .unwrap_or_else(|| panic!("{} has no parameter {name:?}", p.name))
}

/// The plugin at chain `slot` (0 = instrument, 1..N = effects).
fn chain_slot(inst: &LoadedInstrument, slot: usize) -> &LoadedPlugin {
    if slot == 0 {
        inst.instrument.as_ref().expect("instrument loaded")
    } else {
        &inst.effects[slot - 1]
    }
}

fn target_on(slot: usize, info: &plugin::ParameterInfo, kind: ModTargetKind, depth: f32) -> LoadedModTarget {
    LoadedModTarget {
        slot,
        param_name: info.name.clone(),
        kind,
        depth,
        param_min: info.min,
        param_max: info.max,
    }
}

/// Lane modulator target: a parameter somewhere on `inst`'s chain.
fn lane_target(inst: &LoadedInstrument, slot: usize, name: &str, depth: f32) -> LoadedModTarget {
    let info = param(chain_slot(inst, slot), name);
    target_on(slot, info, ModTargetKind::PluginParam { slot, param_index: info.index }, depth)
}

/// Group modulator target: a parameter on member `member`'s chain.
fn member_target(
    inst: &LoadedInstrument,
    member: usize,
    slot: usize,
    name: &str,
    depth: f32,
) -> LoadedModTarget {
    let info = param(chain_slot(inst, slot), name);
    target_on(
        slot,
        info,
        ModTargetKind::GroupMember { member, slot, param_index: info.index },
        depth,
    )
}

/// Group modulator target: a parameter on one of the group's bus effects.
fn bus_target(effects: &[LoadedPlugin], effect_index: usize, name: &str, depth: f32) -> LoadedModTarget {
    let info = param(&effects[effect_index], name);
    target_on(0, info, ModTargetKind::GroupBus { effect_index, param_index: info.index }, depth)
}

/// Cross-mod target: a sibling modulator's LFO rate (labelled like the loader does).
fn rate_target(mod_index: usize, depth: f32) -> LoadedModTarget {
    LoadedModTarget {
        slot: 0,
        param_name: format!("Mod {mod_index} rate"),
        kind: ModTargetKind::ModulatorRate { mod_index },
        depth,
        param_min: 0.01,
        param_max: 50.0,
    }
}

/// `examples/split.toml`: a sine bass (C0–B3) and a square lead (C4–C8), each
/// with an LFO on `detune` for vibrato.
fn scene_split() -> (Vec<LoadedInstrument>, Vec<LoadedGroup>) {
    let mut bass = instrument(builtin("builtin:sine", 1.0, &[]));
    bass.range = range("C0-B3");
    let t = lane_target(&bass, 0, "detune", 0.04);
    bass.modulators.push(lfo(LfoWaveform::Sine, 4.0, vec![t]));

    let mut lead = instrument(builtin("builtin:square", 1.0, &[]));
    lead.range = range("C4-C8");
    let t = lane_target(&lead, 0, "detune", 0.15);
    lead.modulators.push(lfo(LfoWaveform::Sine, 7.0, vec![t]));

    (vec![bass, lead], vec![])
}

/// A pad group (two members, a bus reverb, a group LFO and a group ADSR that
/// cross-mods the LFO's rate) next to an ungrouped bass with a filter swept by
/// a lane LFO. Exercises every target-label path in the tree.
fn scene_group() -> (Vec<LoadedInstrument>, Vec<LoadedGroup>) {
    let mut pad_a = instrument(builtin("builtin:osc", 1.0, &[("waveform", 1.0), ("release", 0.8)]));
    pad_a.group = Some(0);

    let mut pad_b = instrument(builtin("builtin:osc", 1.0, &[]));
    pad_b.range = range("C4-");
    pad_b.group = Some(0);
    pad_b.effects.push(builtin("builtin:filter", 1.0, &[("cutoff", 0.6)]));

    let mut bass = instrument(builtin("builtin:osc", 1.0, &[]));
    bass.range = range("-B3");
    bass.volume = 0.8;
    bass.effects.push(builtin("builtin:filter", 1.0, &[("cutoff", 0.35), ("type", 4.0)]));
    let t = lane_target(&bass, 1, "cutoff", 0.5);
    bass.modulators.push(lfo(LfoWaveform::Triangle, 0.2, vec![t]));

    let bus = vec![builtin("builtin:reverb", 1.0, &[])];
    let group = LoadedGroup {
        name: Some("Pad".to_string()),
        volume: 0.8,
        pan: -0.3,
        modulators: vec![
            lfo(
                LfoWaveform::Sine,
                0.5,
                vec![member_target(&pad_a, 0, 0, "detune", 0.4), bus_target(&bus, 0, "damping", 0.25)],
            ),
            adsr(0.01, 0.3, 0.7, 0.5, vec![rate_target(0, 0.3)]),
        ],
        effects: bus,
    };

    (vec![pad_a, pad_b, bass], vec![group])
}

// ---------------------------------------------------------------------------
// Session tab
// ---------------------------------------------------------------------------

#[test]
fn session_split_chain() {
    // The first instrument selected: the chain tree plus its parameter bars,
    // including the enum row (`waveform ◂ sine ▸`).
    let mut h = Harness::new(scene_split());
    assert_golden("session_split_chain", &mut h);
}

#[test]
fn session_split_modulator_param_focus() {
    // A lane LFO selected, parameter pane focused on its Rate row: the
    // modulator pseudo-params (Type / Waveform / Rate / Targets / depth).
    let mut h = Harness::new(scene_split());
    h.goto(TreeAddress::Modulator { inst: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Down);
    assert_golden("session_split_modulator_param_focus", &mut h);
}

#[test]
fn session_group_header() {
    // A group header selected: the bus pane (Volume + Pan), the nested members,
    // the bus effect and both group modulators in the tree.
    let mut h = Harness::new(scene_group());
    assert_golden("session_group_header", &mut h);
}

#[test]
fn session_group_modulator() {
    // A group LFO selected: its two targets (a member param and a bus-effect
    // param) as depth rows.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::GroupModulator { group: 0, index: 0 });
    assert_golden("session_group_modulator", &mut h);
}

#[test]
fn session_filter_param_focus() {
    // An effect's parameters with the pane focused: enum row (`type`) and the
    // modulated-cutoff row.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter);
    assert_golden("session_filter_param_focus", &mut h);
}

// ---------------------------------------------------------------------------
// Session tab popups
// ---------------------------------------------------------------------------

#[test]
fn popup_effect_selector_while_scanning() {
    // `a` on an instrument: the effect picker, mid-scan, with the built-in
    // catalog already in.
    let mut h = Harness::new(scene_split());
    h.s.extend_catalog(plugin::builtin::enumerate_plugins());
    h.s.catalog_scanning = true;
    h.press(KeyCode::Char('a'));
    assert_golden("popup_effect_selector_while_scanning", &mut h);
}

#[test]
fn popup_group_target_selector() {
    // `t` on a group modulator: member params, member modulators, bus effects
    // and sibling modulators, each labelled by owner.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::GroupModulator { group: 0, index: 0 });
    h.press(KeyCode::Char('t'));
    assert_golden("popup_group_target_selector", &mut h);
}

#[test]
fn popup_lane_target_selector() {
    // `t` on a lane modulator: the whole instrument chain, labelled by plugin.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Modulator { inst: 2, index: 0 });
    h.press(KeyCode::Char('t'));
    assert_golden("popup_lane_target_selector", &mut h);
}

#[test]
fn popup_modulate_parameter() {
    // `m` on a focused parameter: new LFO / new envelope / attach to existing.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('m'));
    assert_golden("popup_modulate_parameter", &mut h);
}

#[test]
fn popup_value_entry() {
    // Enter on a focused float parameter: the value popup with its range.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Enter);
    assert_golden("popup_value_entry", &mut h);
}

#[test]
fn popup_group_assign() {
    // `g` on an instrument: new group / ungroup / existing groups.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('g'));
    assert_golden("popup_group_assign", &mut h);
}

// ---------------------------------------------------------------------------
// Piano tab
// ---------------------------------------------------------------------------

#[test]
fn piano_c_major_triad_held() {
    // C4 E4 G4 held in C major: held in-scale keys, the chord readout, the
    // scale strip.
    let mut h = Harness::new(scene_split());
    for note in [60, 64, 67] {
        h.s.held.note_on(note);
    }
    h.press(KeyCode::Char('2'));
    assert_golden("piano_c_major_triad_held", &mut h);
}

#[test]
fn piano_off_scale_note_held() {
    // C#4 held in C major (Highlight mode): the "wrong note" red.
    let mut h = Harness::new(scene_split());
    h.s.held.note_on(61);
    h.press(KeyCode::Char('2'));
    assert_golden("piano_off_scale_note_held", &mut h);
}

#[test]
fn piano_scale_selector() {
    // `k`: the scale picker (12 roots × every scale type).
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Char('2')).press(KeyCode::Char('k'));
    assert_golden("piano_scale_selector", &mut h);
}

#[test]
fn piano_locked_mode_in_d_dorian() {
    // `l` toggles Locked; the scale comes from the shared filter. D dorian,
    // view shifted up an octave.
    let mut h = Harness::new(scene_split());
    h.s.piano_filter.set_scale(ScaleSetting::parse("D dorian").expect("scale parses"));
    h.press(KeyCode::Char('2')).press(KeyCode::Char('l')).press(KeyCode::Char(']'));
    assert_golden("piano_locked_mode_in_d_dorian", &mut h);
}

// ---------------------------------------------------------------------------
// Scenarios: key sequences → engine commands + the tree the user sees
//
// The golden renders above pin what the model *looks like*; these pin what
// the keys *do* — the commands sent to the audio thread and the index
// fix-ups that keep the TUI model and the audio graph in step.
// ---------------------------------------------------------------------------

/// `scene_group()` with the ungrouped bass moved to lane 0, so a group
/// membership change at the front shifts every member ordinal.
fn scene_group_bass_first() -> (Vec<LoadedInstrument>, Vec<LoadedGroup>) {
    let (mut instruments, groups) = scene_group();
    let bass = instruments.remove(2);
    instruments.insert(0, bass);
    (instruments, groups)
}

#[test]
fn arrow_keys_adjust_a_parameter_in_model_and_engine() {
    // Param focus on `detune` (range ±24): Right = +5% of the range, Shift+Left
    // = −1% (fine).
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=1 value=2.400"]);
    h.press_with(KeyCode::Left, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=1 value=1.920"]);

    let detune = &h.s.instruments[0].instrument.as_ref().unwrap().params[1];
    assert_eq!(detune.name, "detune");
    assert!((detune.value - 1.92).abs() < 1e-4, "{}", detune.value);
    assert!(h.s.dirty);
}

#[test]
fn enum_parameters_step_one_value_at_a_time() {
    // `waveform` (sine → triangle) and `type` on a filter (lowpass → highpass):
    // Right moves exactly one label, whatever the modifiers.
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Enter).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=0 value=1.000"]);
}

#[test]
fn typing_a_value_sets_the_parameter() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Enter); // focus params, open the value popup
    h.press_n(KeyCode::Backspace, 4).type_chars("0.8").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetParameter inst=2 slot=1 param=0 value=0.800"]);
    assert!(h.s.editing.is_none(), "the popup closes");
    assert_eq!(h.s.instruments[2].effects[0].params[0].value, 0.8);
}

#[test]
fn the_volume_popup_sets_instrument_volume_clamped_to_four() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 4).type_chars("9").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetVolume inst=2 value=4.000"]);
    assert_eq!(h.s.instruments[2].volume, 4.0);
}

#[test]
fn the_mix_popup_sets_an_effects_dry_wet() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Char('x')).press_n(KeyCode::Backspace, 4).type_chars("0.25").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupMix group=0 fx=0 value=0.250"]);
}

#[test]
fn the_range_popup_sets_an_instrument_range() {
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Char('R')).press_n(KeyCode::Backspace, 5).type_chars("C2-C6").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetInstrumentRange inst=0 range=Some((36, 84))"]);
    assert_eq!(h.tree()[0], "♪ Oscillator  [Built-in]  C2-C6");
}

#[test]
fn adding_an_effect_from_the_selector_uses_the_default_mix() {
    // Reverb defaults to half wet; a filter to fully wet (a half-wet filter
    // barely filters).
    let mut h = Harness::new(scene_split());
    h.s.extend_catalog(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('a')).type_chars("reverb").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["InsertEffect inst=0 index=0 mix=0.500"]);
    h.press(KeyCode::Char('a')).type_chars("filter").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["InsertEffect inst=0 index=1 mix=1.000"]);
    assert_eq!(h.tree()[1], "├ fx Reverb  [Built-in]");
    assert_eq!(h.tree()[2], "├ fx Filter  [Built-in]");
    assert!(h.s.dirty);
}

#[test]
fn adding_a_group_effect_from_the_selector() {
    let mut h = Harness::new(scene_group());
    h.s.extend_catalog(plugin::builtin::enumerate_plugins());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('a')).type_chars("filter").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["InsertGroupEffect group=0 index=1 mix=1.000"]);
    assert!(h.tree().contains(&"  fx Filter  [Built-in]  (bus)"));
}

#[test]
fn replacing_the_instrument_loads_a_fresh_plugin() {
    let mut h = Harness::new(scene_split());
    h.s.extend_catalog(plugin::builtin::enumerate_plugins());
    h.s.instruments[0].instrument.as_mut().unwrap().params[2].value = 0.25; // volume, edited
    h.press(KeyCode::Char('i')).press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SwapInstrument inst=0"]);
    let volume = &h.s.instruments[0].instrument.as_ref().unwrap().params[2];
    assert_eq!((volume.name.as_str(), volume.value), ("volume", 1.0), "edits don't survive a swap");
}

#[test]
fn m_on_an_instrument_adds_an_lfo_to_its_lane() {
    let mut h = Harness::new(scene_split());
    h.goto(TreeAddress::Instrument(1));
    h.press(KeyCode::Char('m'));
    assert_eq!(h.commands(), ["InsertModulator inst=1 index=1"]);
    assert_eq!(h.tree()[4], "╰ ~ LFO 1.0Hz sine");
}

#[test]
fn m_on_a_group_adds_an_lfo_to_the_groups_rack() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('m'));
    assert_eq!(h.commands(), ["InsertGroupModulator group=0 index=2"]);
}

#[test]
fn deleting_a_bus_effect_drops_the_group_modulators_bus_targets() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["RemoveGroupEffect group=0 index=0"]);
    let tree = h.tree();
    assert!(!tree.iter().any(|l| l.contains("Reverb")), "{tree:#?}");
    assert!(tree.contains(&"  ~ LFO 0.5Hz sine → M0 Oscillator: detune (40%)  (bus)"), "{tree:#?}");
}

#[test]
fn deleting_a_group_modulator_drops_cross_mod_targets_pointing_at_it() {
    // The ADSR's only target is "Mod 0 rate"; with Mod 0 gone it has nothing
    // left to drive, and no target may keep pointing at a stale index.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::GroupModulator { group: 0, index: 0 });
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["RemoveGroupModulator group=0 index=0"]);
    assert_eq!(h.s.groups[0].modulators.len(), 1);
    assert!(h.s.groups[0].modulators[0].targets.is_empty());
    assert!(h.tree().contains(&"  ~ ADSR  (bus)"), "{:#?}", h.tree());
}

#[test]
fn deleting_a_group_member_drops_its_modulator_targets() {
    // Lane 0 is group member 0, targeted by the group LFO's `detune` target. The
    // first `d` clears its plugin, the second removes the (now empty) lane; the
    // member-0 target goes with it and the bus target stays.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Char('d')).press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["ClearInstrument inst=0", "RemoveInstrument inst=0"]);
    let lfo = &h.s.groups[0].modulators[0];
    assert_eq!(lfo.targets.len(), 1);
    assert_eq!(lfo.targets[0].param_name, "damping");
    assert!(
        h.tree().contains(&"  ~ LFO 0.5Hz sine → Bus Reverb: damping (25%)  (bus)"),
        "{:#?}",
        h.tree()
    );
}

#[test]
fn a_new_group_member_before_existing_ones_shifts_member_ordinals() {
    // The bass (lane 0) joins the Pad group and becomes its member 0, so the
    // group LFO's "member 0" target must now read "member 1" — the same
    // oscillator it was driving before.
    let mut h = Harness::new(scene_group_bass_first());
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Char('g')).press(KeyCode::Down).press(KeyCode::Enter); // "New group" → "Pad"
    assert_eq!(h.commands(), ["SetLaneGroup inst=0 group=Some(0)"]);
    let target = &h.s.groups[0].modulators[0].targets[0];
    assert!(
        matches!(target.kind, crate::plugin::chain::ModTargetKind::GroupMember { member: 1, .. }),
        "{:?}",
        target.kind
    );
    assert!(
        h.tree().iter().any(|l| l.starts_with("  ~ LFO 0.5Hz sine → M1 Oscillator: detune")),
        "{:#?}",
        h.tree()
    );
}

#[test]
fn deleting_a_group_ungroups_its_members() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["RemoveGroup group=0"]);
    assert!(h.s.groups.is_empty());
    assert!(h.s.instruments.iter().all(|n| n.group.is_none()));
    assert_eq!(h.tree().iter().filter(|l| l.starts_with("♪")).count(), 3);
}

#[test]
fn the_param_cursor_survives_moving_between_nodes_and_is_clamped() {
    // Moving the chain cursor swaps the parameter pane's contents but keeps the
    // parameter cursor's row, clamped to the new pane's length (a group's bus
    // pane has two rows, an oscillator eight).
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Enter).press_n(KeyCode::Down, 7).press(KeyCode::Esc);
    assert_eq!((h.s.param_state.selected, h.s.param_state.len), (7, 8));
    h.goto(TreeAddress::Instrument(1));
    assert_eq!((h.s.param_state.selected, h.s.param_state.len), (7, 8), "same row on the next oscillator");
    h.goto(TreeAddress::Group(0));
    assert_eq!((h.s.param_state.selected, h.s.param_state.len), (1, 2), "clamped to the bus pane");
}

// ---------------------------------------------------------------------------
// Smoke
// ---------------------------------------------------------------------------

#[test]
fn harness_sends_nothing_until_asked() {
    // Constructing and drawing a `State` must not talk to the engine
    // (`State::new` is pure; the initial transpose / pattern push lives in `run`).
    let mut h = Harness::new(scene_group());
    let _ = h.dump();
    assert!(h.commands().is_empty());
}
