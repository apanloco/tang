//! Golden-render tests for the TUI — the regression net for the controller
//! refactor (`docs/web-ui.md`, phase 0).
//!
//! Each test loads a session (TOML text) through the real loader against a stub
//! command channel, builds a `State` from the resulting model, drives it with real
//! key events through `process_event`, draws it into ratatui's `TestBackend`, and
//! snapshots the buffer with `insta` —
//! characters *and* styles, because colour carries meaning here (the piano
//! paints held / root / scale keys with background colour on blank cells).
//!
//! After an intentional UI change, regenerate with
//! `INSTA_UPDATE=always cargo test golden`, then review the snapshot diff.

use std::path::Path;

use crossbeam_channel::{Receiver, unbounded};
use crossterm::event::KeyEvent;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use super::*;
use crate::app::load::load_session;
use crate::app::testutil::{DropProbe, described, recording, stub_engine};
use crate::plugin::PluginInfo;
use crate::plugin::chain::{MidiBindSource, MidiLearnedNotification, ModulatorLocation};
use crate::session;

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
    /// The notes the controller would play; kept so that sending them cannot fail.
    _midi_rx: Receiver<crate::audio::MidiEvent>,
    /// The audio thread's end of the channel that hands swapped-out plugins back.
    return_tx: crossbeam_channel::Sender<Box<dyn plugin::Plugin>>,
    /// The audio thread's ends of the notification channels.
    pattern_tx: crossbeam_channel::Sender<crate::plugin::chain::PatternNotification>,
    learn_tx: crossbeam_channel::Sender<crate::plugin::chain::MidiLearnedNotification>,
    /// The scan thread's end; `None` unless a test started a scan with
    /// `start_scan`, and again once the scan is over.
    catalog_tx: Option<crossbeam_channel::Sender<Vec<PluginInfo>>>,
}

impl Harness {
    fn new(session_toml: impl AsRef<str>) -> Self {
        let config = session::parse(session_toml.as_ref()).expect("scene parses");
        // The loader sends the audio thread its commands; nothing listens here, but
        // the receiver has to stay alive for the sends to succeed.
        let (load_tx, _load_rx) = unbounded();
        let model = load_session(
            &config,
            Path::new("."),
            SAMPLE_RATE,
            BLOCK,
            &plugin::Runtime::default(),
            &load_tx,
            &mut |_| {},
        )
        .expect("scene loads");
        // What the TUI sends from here on is what `commands()` reports.
        let (handles, engine) = stub_engine();
        // No scan has run: an empty, finished catalog unless a test fills it in.
        let s = State::new(Controller::new(model, handles, None));
        let terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        Harness {
            s,
            terminal,
            cmd_rx: engine.cmd_rx,
            _midi_rx: engine.midi_rx,
            return_tx: engine.return_tx,
            pattern_tx: engine.pattern_tx,
            learn_tx: engine.learn_tx,
            catalog_tx: None,
        }
    }

    /// A plugin scan is under way: what a test sends on `catalog_tx` is folded
    /// into the catalog by the next `tick`, and the scan is over when the
    /// sender is dropped.
    fn start_scan(&mut self) -> &mut Self {
        let (tx, rx) = unbounded();
        self.s.ctl.catalog.follow(rx);
        self.catalog_tx = Some(tx);
        self
    }

    fn press(&mut self, code: KeyCode) -> &mut Self {
        self.press_with(code, KeyModifiers::NONE)
    }

    fn press_with(&mut self, code: KeyCode, modifiers: KeyModifiers) -> &mut Self {
        process_event(&mut self.s, Event::Key(KeyEvent::new(code, modifiers)));
        // Whatever the key did, every node still has an id of its own.
        if let Err(e) = self.s.ctl.model.check_ids() {
            panic!("after {code:?} {modifiers:?}: {e}");
        }
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
        described(self.cmd_rx.try_iter())
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
// Fixtures: sessions, as TOML, loaded by the real loader
// ---------------------------------------------------------------------------

/// `examples/split.toml`: a sine bass (C0–B3) and a square lead (C4–C8), each
/// with an LFO on `detune` for vibrato.
fn scene_split() -> &'static str {
    include_str!("../../examples/split.toml")
}

/// The Pad group: a bus reverb, a group LFO on a member's `detune` and the bus
/// `damping`, and a group ADSR that cross-mods the LFO's rate.
const GROUP: &str = r#"
[[group]]
name = "Pad"
volume = 0.8
pan = -0.3

[[group.effect]]
plugin = "builtin:reverb"

[[group.modulator]]
rate = 0.5
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.4
[[group.modulator.target]]
bus = 0
param = "damping"
depth = 0.25

[[group.modulator]]
type = "envelope"
[[group.modulator.target]]
mod_rate = 0
depth = 0.3
"#;

/// The first pad member, a triangle with a longer release.
const PAD_A: &str = r#"
[[instrument]]
plugin = "builtin:osc"
group = 0
[instrument.params]
waveform = 1.0
release = 0.8
"#;

/// The second pad member: C4 and up, with a filter.
const PAD_B: &str = r#"
[[instrument]]
plugin = "builtin:osc"
range = "C4-"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[instrument.effect.params]
cutoff = 0.6
"#;

/// An ungrouped bass up to B3 with a ladder filter swept by a lane LFO.
const BASS: &str = r#"
[[instrument]]
plugin = "builtin:osc"
range = "-B3"
volume = 0.8
[[instrument.effect]]
plugin = "builtin:filter"
[instrument.effect.params]
cutoff = 0.35
type = 4.0
[[instrument.modulator]]
waveform = "triangle"
rate = 0.2
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.5
"#;

/// The pad group (two members, a bus reverb, a group LFO and a group ADSR that
/// cross-mods the LFO's rate) next to an ungrouped bass with a filter swept by a
/// lane LFO. Exercises every target-label path in the tree.
fn scene_group() -> String {
    [GROUP, PAD_A, PAD_B, BASS].concat()
}

/// `scene_group()` with the ungrouped bass first (lane 0), so a group membership
/// change at the front shifts every member ordinal.
fn scene_group_bass_first() -> String {
    [GROUP, BASS, PAD_A, PAD_B].concat()
}

/// One oscillator with a filter and a reverb in series, and a single LFO driving
/// both (`cutoff` on the filter at slot 1, `damping` on the reverb at slot 2).
fn scene_two_effects() -> &'static str {
    r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"
mix = 0.5
[[instrument.modulator]]
rate = 0.5
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
[[instrument.modulator.target]]
slot = 2
param = "damping"
depth = 0.3
"#
}

/// A group with a filter and a reverb on its bus, and a group LFO driving both.
fn scene_bus_pair() -> &'static str {
    r#"
[[group]]
name = "Pad"
[[group.effect]]
plugin = "builtin:filter"
[[group.effect]]
plugin = "builtin:reverb"
mix = 0.5
[[group.modulator]]
rate = 0.5
[[group.modulator.target]]
bus = 0
param = "cutoff"
depth = 0.4
[[group.modulator.target]]
bus = 1
param = "damping"
depth = 0.3

[[instrument]]
plugin = "builtin:osc"
group = 0
"#
}

/// A group of two oscillators with effects, and a group LFO driving three of those
/// plugins: member 0's filter (`cutoff`, slot 1) and reverb (`damping`, slot 2),
/// and member 1's filter (`cutoff`, slot 1).
fn scene_member_effects() -> &'static str {
    r#"
[[group]]
name = "Pad"
[[group.modulator]]
rate = 0.5
[[group.modulator.target]]
member = 0
slot = 1
param = "cutoff"
depth = 0.4
[[group.modulator.target]]
member = 0
slot = 2
param = "damping"
depth = 0.3
[[group.modulator.target]]
member = 1
slot = 1
param = "cutoff"
depth = 0.2

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
"#
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

/// A lane with the three kinds of modulator, each driving the filter and told apart by what it is set to.
fn scene_three_kinds_of_modulator() -> &'static str {
    r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 2.5
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
[[instrument.modulator]]
type = "envelope"
attack = 0.02
decay = 0.3
sustain = 0.6
release = 0.5
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = -0.25
[[instrument.modulator]]
type = "midi"
midi_cc = 74
[[instrument.modulator.target]]
slot = 1
param = "resonance"
depth = 1.0
"#
}

#[test]
fn session_envelope_modulator_param_focus() {
    // An envelope selected, the parameter pane focused on its Sustain row: Type, the four stages, Targets, a depth.
    let mut h = Harness::new(scene_three_kinds_of_modulator());
    h.goto(TreeAddress::Modulator { inst: 0, index: 1 });
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Down).press(KeyCode::Down);
    assert_golden("session_envelope_modulator_param_focus", &mut h);
}

#[test]
fn session_midi_modulator_param_focus() {
    // A MIDI control selected, the parameter pane focused on its depth: what it is bound to, Targets, a depth.
    let mut h = Harness::new(scene_three_kinds_of_modulator());
    h.goto(TreeAddress::Modulator { inst: 0, index: 2 });
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Down);
    assert_golden("session_midi_modulator_param_focus", &mut h);
}

#[test]
fn enter_on_a_row_of_a_modulators_pane_opens_the_value_for_the_numbers_and_nothing_for_the_rest() {
    // (modulator, row) -> the value popup's title, the text it starts with, and the range it is held to; None where
    // the row is not a number to type.
    type Popup = (&'static str, &'static str, f32, f32);
    let rows: [(usize, usize, Option<Popup>); 15] = [
        (0, 0, None), // the kind
        (0, 1, None), // the waveform
        (0, 2, Some(("Rate (Hz)", "2.50", 0.01, 50.0))),
        (0, 3, None), // the separator
        (0, 4, Some(("cutoff depth", "0.40", -1.0, 1.0))),
        (1, 0, None),
        (1, 1, Some(("Attack (s)", "0.020", 0.001, 10.0))),
        (1, 2, Some(("Decay (s)", "0.300", 0.001, 10.0))),
        (1, 3, Some(("Sustain", "0.600", 0.0, 1.0))),
        (1, 4, Some(("Release (s)", "0.500", 0.001, 10.0))),
        (1, 5, None),
        (1, 6, Some(("cutoff depth", "-0.250", -1.0, 1.0))),
        (2, 0, None), // what it is bound to
        (2, 1, None),
        (2, 2, Some(("resonance depth", "1.00", -1.0, 1.0))),
    ];
    for (modulator, row, expected) in rows {
        let mut h = Harness::new(scene_three_kinds_of_modulator());
        h.goto(TreeAddress::Modulator { inst: 0, index: modulator });
        h.press(KeyCode::Enter);
        for _ in 0..row {
            h.press(KeyCode::Down);
        }
        h.press(KeyCode::Enter);
        let got = h.s.editing.as_ref().map(|e| (e.param_name.as_str(), e.input.value.as_str(), e.param_min, e.param_max));
        assert_eq!(got, expected, "modulator {modulator}, row {row}");
    }
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
    h.start_scan();
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
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
        h.s.ctl.held.note_on(note);
    }
    h.press(KeyCode::Char('2'));
    assert_golden("piano_c_major_triad_held", &mut h);
}

#[test]
fn piano_off_scale_note_held() {
    // C#4 held in C major (Highlight mode): the "wrong note" red.
    let mut h = Harness::new(scene_split());
    h.s.ctl.held.note_on(61);
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
    h.s.ctl.piano_filter.set_scale(ScaleSetting::parse("D dorian").expect("scale parses"));
    h.press(KeyCode::Char('2')).press(KeyCode::Char('l')).press(KeyCode::Char(']'));
    assert_golden("piano_locked_mode_in_d_dorian", &mut h);
}

#[test]
fn choosing_a_scale_in_the_picker_is_an_intent_that_the_controller_carries_out() {
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Char('2')).press(KeyCode::Char('k'));
    h.type_chars("d dorian").press(KeyCode::Enter);
    assert_eq!(h.s.ctl.piano_filter.scale().short(), "D dorian");
    // One intent, so a front-end that watches the session hears of it, and a change to save.
    assert_eq!((h.s.ctl.rev, h.s.ctl.dirty), (1, true));
    assert_eq!(h.commands(), Vec::<String>::new(), "the scale is nothing of the audio thread's");

    // Choosing the scale that is set again is no change.
    h.press(KeyCode::Char('k')).press(KeyCode::Enter);
    assert_eq!(h.s.ctl.rev, 1);
}

#[test]
fn the_lock_key_is_an_intent_that_the_controller_carries_out_and_a_second_press_undoes() {
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Char('2')).press(KeyCode::Char('l'));
    assert_eq!(h.s.ctl.piano_filter.mode(), PianoMode::Locked);
    assert_eq!((h.s.ctl.rev, h.s.ctl.dirty), (1, true));
    h.press(KeyCode::Char('l'));
    assert_eq!(h.s.ctl.piano_filter.mode(), PianoMode::Highlight);
    assert_eq!(h.s.ctl.rev, 2);
}

// ---------------------------------------------------------------------------
// Scenarios: key sequences → engine commands + the tree the user sees
//
// The golden renders above pin what the model *looks like*; these pin what
// the keys *do* — the commands sent to the audio thread and the index
// fix-ups that keep the TUI model and the audio graph in step.
// ---------------------------------------------------------------------------

#[test]
fn arrow_keys_adjust_a_parameter_in_model_and_engine() {
    // Param focus on `detune` (range ±24): Right = +5% of the range, Shift+Left
    // = −1% (fine).
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=1 value=2.400"]);
    h.press_with(KeyCode::Left, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=1 value=1.920"]);

    let detune = &h.s.ctl.model.instruments[0].instrument.as_ref().unwrap().params[1];
    assert_eq!(detune.name, "detune");
    assert!((detune.value - 1.92).abs() < 1e-4, "{}", detune.value);
    assert!(h.s.ctl.dirty);
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
    assert_eq!(h.s.ctl.model.instruments[2].effects[0].params[0].value, 0.8);
}

/// What `str::parse` takes for a number and no number can be: `NaN` (which `clamp` keeps) and
/// the infinities.
const NOT_NUMBERS: [&str; 4] = ["nan", "NaN", "inf", "-infinity"];

#[test]
fn a_number_typed_into_a_popup_is_a_finite_number_with_or_without_spaces_around_it() {
    use super::parse_number;
    assert_eq!(parse_number("0.5"), Some(0.5));
    assert_eq!(parse_number("  -12 "), Some(-12.0));
    assert_eq!(parse_number("1e3"), Some(1000.0));
    for text in NOT_NUMBERS.into_iter().chain(["", "abc", "1,5", "0.5.1"]) {
        assert_eq!(parse_number(text), None, "{text:?}");
    }
}

#[test]
fn a_tempo_that_is_not_a_number_is_not_taken() {
    for text in NOT_NUMBERS {
        let mut h = Harness::new(scene_split());
        h.type_chars("b").press_n(KeyCode::Backspace, 3).type_chars(text).press(KeyCode::Enter);
        assert!(h.s.bpm_editing.is_none(), "{text}: the popup closes");
        assert_eq!(h.s.ctl.global_bpm, 120.0, "{text}");
        assert_eq!(h.commands(), Vec::<String>::new(), "{text}");
        assert!(!h.s.ctl.dirty, "{text}");
    }
}

#[test]
fn a_value_that_is_not_a_number_is_not_taken_by_a_modulators_rate() {
    for text in NOT_NUMBERS {
        let mut h = Harness::new(scene_split());
        h.goto(TreeAddress::Modulator { inst: 0, index: 0 });
        h.press(KeyCode::Enter).press_n(KeyCode::Down, 2).press(KeyCode::Enter); // focus params, the rate row, the value popup
        assert!(h.s.editing.is_some(), "{text}: the popup opens");
        h.press_n(KeyCode::Backspace, 6).type_chars(text).press(KeyCode::Enter);
        assert!(h.s.editing.is_none(), "{text}: the popup closes");
        assert_eq!(h.commands(), Vec::<String>::new(), "{text}");
        let ModSourceSlot::Lfo { rate, .. } = h.s.ctl.model.instruments[0].modulators[0].source else { panic!("an LFO") };
        assert_eq!(rate, 4.0, "{text}");
    }
}

#[test]
fn arrow_keys_and_typing_set_a_bus_effect_parameter() {
    let mut h = Harness::new(scene_bus_pair());
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Left); // the filter's cutoff, 5% down from fully open
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.950"]);
    let cutoff = &h.s.ctl.model.groups[0].effects[0].params[0];
    assert_eq!((cutoff.name.as_str(), cutoff.value), ("cutoff", 0.95));
    assert!(h.s.ctl.dirty);

    h.s.ctl.dirty = false;
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 6).type_chars("0.3").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.300"]);
    assert_eq!(h.s.ctl.model.groups[0].effects[0].params[0].value, 0.3);
    assert!(h.s.ctl.dirty);

    // Past the ends of the range (0..1), by the arrows and by typing.
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 6).type_chars("0.99").press(KeyCode::Enter);
    h.commands();
    h.press_with(KeyCode::Right, KeyModifiers::CONTROL);
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=1.000"]);
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 6).type_chars("9").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=1.000"]);
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 6).type_chars("-9").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.000"]);
}

#[test]
fn an_enum_parameter_snaps_to_a_whole_value_when_it_was_between_two() {
    // A preset or a plugin can leave an enum at a fraction; the next step lands
    // on a label.
    let mut h = Harness::new(scene_bus_pair());
    h.s.ctl.model.instruments[0].instrument.as_mut().unwrap().params[0].value = 0.4; // waveform
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Enter).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=0 value=1.000"]);

    h.press(KeyCode::Esc);
    h.s.ctl.model.groups[0].effects[0].params[2].value = 0.4; // the filter's type
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Down).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=2 value=1.000"]);
}

#[test]
fn the_command_carries_the_plugins_own_parameter_index() {
    // The row and the plugin's index for it differ when a plugin hides
    // parameters; here one is renumbered to tell them apart.
    let mut h = Harness::new(scene_bus_pair());
    h.s.ctl.model.instruments[0].instrument.as_mut().unwrap().params[1].index = 42; // detune
    h.s.ctl.model.groups[0].effects[0].params[0].index = 17; // the filter's cutoff
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=42 value=2.400"]);
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 8).type_chars("3").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=42 value=3.000"]);

    h.press(KeyCode::Esc);
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Up).press(KeyCode::Left); // the parameter cursor was on row 1
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=17 value=0.950"]);
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 8).type_chars("0.5").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=17 value=0.500"]);
}

#[test]
fn a_float_parameter_stops_at_the_ends_of_its_range() {
    // `volume` on the oscillator: 0..1, starting at 1. The command goes out
    // whether or not the value moved.
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Down);
    h.press_with(KeyCode::Right, KeyModifiers::CONTROL);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=2 value=1.000"; 2]);

    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 8).type_chars("99").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=2 value=1.000"]);
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 8).type_chars("-5").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=2 value=0.000"]);
    assert_eq!(h.s.ctl.model.instruments[0].instrument.as_ref().unwrap().params[2].value, 0.0);
}

#[test]
fn an_enum_parameter_stops_at_its_first_and_last_label() {
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Enter); // `waveform`: sine, triangle, square, saw
    for _ in 0..5 {
        h.press(KeyCode::Right);
    }
    let values: Vec<String> = h.commands();
    assert_eq!(
        values,
        [1.0, 2.0, 3.0, 3.0, 3.0].map(|v| format!("SetParameter inst=0 slot=0 param=0 value={v:.3}"))
    );
    for _ in 0..5 {
        h.press(KeyCode::Left);
    }
    assert_eq!(
        h.commands(),
        [2.0, 1.0, 0.0, 0.0, 0.0].map(|v| format!("SetParameter inst=0 slot=0 param=0 value={v:.3}"))
    );
}

#[test]
fn arrow_keys_edit_the_parameter_the_filter_shows() {
    // With "res" typed into the parameter filter the first row is `resonance`
    // (parameter 1 of the filter), not `cutoff`.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter);
    h.press(KeyCode::Char('/')).type_chars("res").press(KeyCode::Enter);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetParameter inst=2 slot=1 param=1 value=1.182"]);
    assert!((h.s.ctl.model.instruments[2].effects[0].params[1].value - 1.182).abs() < 1e-3);
}

#[test]
fn a_change_of_a_modulators_setting_made_elsewhere_leaves_the_parameter_filter_being_typed_as_it_is() {
    // The TUI is filtering the filter's parameters ("cut" typed, not yet confirmed) when a browser turns the LFO's
    // rate and the depth of its cable. The tree shows both, so it is built again, and that is not a new node.
    let session = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n\
        [[instrument.modulator]]\nrate = 1.0\n[[instrument.modulator.target]]\nslot = 1\nparam = \"cutoff\"\ndepth = 0.4\n";
    let mut h = Harness::new(session);
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Enter);
    h.press(KeyCode::Char('/')).type_chars("cut");
    assert!(h.s.param_filtering);
    assert!(!h.dump().contains("resonance"), "(only the cutoff is let through)");

    let lfo = h.s.ctl.model.instruments[0].modulators[0].node;
    let cable = h.s.ctl.model.instruments[0].modulators[0].targets[0].edge;
    for intent in [
        Intent::SetModulatorField { node: lfo, field: ModulatorField::Rate, value: 7.0 },
        Intent::SetEdgeDepth { edge: cable, depth: 0.9 },
    ] {
        let applied = h.s.ctl.apply(intent).unwrap();
        h.s.react_to(&applied);
    }
    assert!(h.tree().iter().any(|line| line.contains("7.0Hz") && line.contains("(90%)")), "the tree shows them: {:#?}", h.tree());
    assert!(h.s.param_filtering, "still being typed");
    assert_eq!(h.s.param_filter_input.value, "cut");
    assert_eq!(h.s.chain_state.selected, h.s.tree_entries.iter().position(|e| e.address == TreeAddress::Effect { inst: 0, index: 0 }).unwrap());
    // And on the screen: what is typed, and only the rows it lets through.
    let after = h.dump();
    assert!(after.contains("/ cut") && after.contains("cutoff") && !after.contains("resonance"), "{after}");
}

#[test]
fn the_parameter_filter_goes_when_the_node_under_the_cursor_is_another_one() {
    // "res" is typed and confirmed for the filter, and the cursor is in its parameters. The filter is taken out (by
    // another front-end) and the reverb is at its place in the tree: the cursor is at the same address, and on another
    // node, which is not what the filter was typed for.
    let mut h = Harness::new(scene_two_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Enter);
    h.press(KeyCode::Char('/')).type_chars("res").press(KeyCode::Enter);
    assert_eq!(h.s.param_filter_input.value, "res");
    h.s.ctl.model.remove_effect(0, 0);
    h.s.rebuild_tree();
    assert_eq!(h.s.tree_entries.get(h.s.chain_state.selected).map(|e| e.address), Some(TreeAddress::Effect { inst: 0, index: 0 }));
    assert_eq!(h.s.ctl.model.instruments[0].effects[0].name, "Reverb");
    assert_eq!(h.s.param_filter_input.value, "", "the reverb is shown whole");
    assert!(!h.s.param_filtering);
}

const LANE_WITH_FILTER_AND_LFO_AND_A_PLAIN_ONE: &str =
    "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument.modulator]]\nrate = 2.0\n\
     [[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.5\n";

#[test]
fn shift_down_and_up_on_an_instrument_move_the_whole_lane_with_its_effects_and_modulators() {
    let mut h = Harness::new(LANE_WITH_FILTER_AND_LFO_AND_A_PLAIN_ONE);
    h.goto(TreeAddress::Instrument(0));
    assert!(h.tree()[1].contains("fx Filter") && h.tree()[2].contains("LFO"), "{:#?}", h.tree());
    h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["SwapInstruments a=0 b=1"]);
    // The lane with the filter and the LFO is the second now, and has them under it; the cursor stays on it.
    assert!(h.tree()[0].starts_with('♪') && h.tree()[1].starts_with('♪'), "{:#?}", h.tree());
    assert!(h.tree()[2].contains("fx Filter") && h.tree()[3].contains("LFO"), "{:#?}", h.tree());
    assert_eq!(h.s.selected_address(), Some(&TreeAddress::Instrument(1)));
    assert_eq!(h.s.ctl.model.instruments[0].volume, 0.5);
    // And back.
    h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["SwapInstruments a=1 b=0"]);
    assert!(h.tree()[1].contains("fx Filter") && h.tree()[2].contains("LFO"), "{:#?}", h.tree());
    assert_eq!(h.s.selected_address(), Some(&TreeAddress::Instrument(0)));
    // The first cannot go up and the last cannot go down.
    h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
    h.goto(TreeAddress::Instrument(1)).press_with(KeyCode::Down, KeyModifiers::SHIFT);
    assert!(h.commands().is_empty(), "{:?}", h.commands());
}

#[test]
fn d_on_an_instrument_empties_it_and_leaves_the_lanes_effects_and_modulators() {
    let mut h = Harness::new(LANE_WITH_FILTER_AND_LFO_AND_A_PLAIN_ONE);
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["ClearInstrument inst=0"]);
    assert!(h.s.ctl.model.instruments[0].instrument.is_none());
    assert!(h.tree().iter().any(|line| line.contains("fx Filter")) && h.tree().iter().any(|line| line.contains("LFO")), "{:#?}", h.tree());
    // A second `d` removes the lane (that has no plugin now), with what it had.
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["RemoveInstrument inst=0 keep"]);
    assert_eq!(h.s.ctl.model.instruments.len(), 1);
}

#[test]
fn clicking_and_dragging_a_parameter_bar_sets_its_value() {
    use crossterm::event::{MouseEvent, MouseEventKind};
    let mut h = Harness::new(scene_split());
    h.dump(); // draws once, which lays out the panes
    let area = h.s.areas.param_inner;
    // `volume` is the third row; the bar starts 27 columns in and spans the
    // width less 36.
    let (row, bar_start, bar_width) = (area.y + 2, area.x + 27, area.width - 36);
    let mouse = |kind, column| Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });

    let left = crossterm::event::MouseButton::Left;
    process_event(&mut h.s, mouse(MouseEventKind::Down(left), bar_start + bar_width - 1));
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=2 value=1.000"], "the right end of the bar");
    process_event(&mut h.s, mouse(MouseEventKind::Drag(left), bar_start));
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=2 value=0.000"], "dragged to the left end");
    process_event(&mut h.s, mouse(MouseEventKind::Drag(left), bar_start + bar_width / 2));
    let halfway = h.commands();
    let value: f32 = halfway[0].rsplit('=').next().unwrap().parse().unwrap();
    assert!((value - 0.5).abs() < 0.05, "dragged to the middle: {halfway:?}");
    process_event(&mut h.s, mouse(MouseEventKind::Up(crossterm::event::MouseButton::Left), bar_start));
    process_event(&mut h.s, mouse(MouseEventKind::Drag(crossterm::event::MouseButton::Left), bar_start));
    assert_eq!(h.commands(), Vec::<String>::new(), "the drag is over");
    assert!(h.s.ctl.dirty);
}

#[test]
fn the_gain_popups_open_on_the_current_value() {
    let mut h = Harness::new(scene_group());
    let prefill = |h: &Harness| h.s.gain_editing.as_ref().map(|g| g.edit.input.value.clone());
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('v'));
    assert_eq!(prefill(&h).as_deref(), Some("0.80"), "the bass's volume");
    h.press(KeyCode::Esc);
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('v'));
    assert_eq!(prefill(&h).as_deref(), Some("0.80"), "the group's volume");
    h.press(KeyCode::Esc);
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Char('x'));
    assert_eq!(prefill(&h).as_deref(), Some("1.00"), "the filter's mix");
}

#[test]
fn the_mix_popup_sets_a_lane_effects_dry_wet_within_zero_and_one() {
    let mut h = Harness::new(scene_two_effects()); // a filter, then a reverb at 0.5
    h.goto(TreeAddress::Effect { inst: 0, index: 1 });
    h.press(KeyCode::Char('x'));
    assert_eq!(h.s.gain_editing.as_ref().unwrap().edit.input.value, "0.50");
    h.press_n(KeyCode::Backspace, 6).type_chars("0.25").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetMix inst=0 slot=2 value=0.250"]);
    assert_eq!(h.s.ctl.model.instruments[0].effects[1].mix, 0.25);
    assert_eq!(h.s.ctl.model.instruments[0].effects[0].mix, 1.0, "the other effect is untouched");
    assert!(h.s.ctl.dirty);

    for (typed, command, mix) in [("5", "SetMix inst=0 slot=2 value=1.000", 1.0), ("-2", "SetMix inst=0 slot=2 value=0.000", 0.0)] {
        h.press(KeyCode::Char('x')).press_n(KeyCode::Backspace, 6).type_chars(typed).press(KeyCode::Enter);
        assert_eq!(h.commands(), [command]);
        assert_eq!(h.s.ctl.model.instruments[0].effects[1].mix, mix);
    }
}

#[test]
fn the_mix_popup_sets_a_bus_effects_dry_wet_within_zero_and_one() {
    let mut h = Harness::new(scene_bus_pair()); // a filter, then a reverb at 0.5
    h.goto(TreeAddress::GroupEffect { group: 0, index: 1 });
    h.press(KeyCode::Char('x')).press_n(KeyCode::Backspace, 6).type_chars("0.1").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupMix group=0 fx=1 value=0.100"]);
    assert_eq!(h.s.ctl.model.groups[0].effects[1].mix, 0.1);
    assert_eq!(h.s.ctl.model.groups[0].effects[0].mix, 1.0, "the other bus effect is untouched");
    assert!(h.s.ctl.dirty);
    h.press(KeyCode::Char('x')).press_n(KeyCode::Backspace, 6).type_chars("7").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupMix group=0 fx=1 value=1.000"]);
}

#[test]
fn the_volume_popup_sets_a_groups_volume_within_zero_and_four() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 6).type_chars("1.5").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupVolume group=0 value=1.500"]);
    assert_eq!(h.s.ctl.model.groups[0].volume, 1.5);
    assert!(h.s.ctl.dirty);
    for (typed, command) in [("9", "SetGroupVolume group=0 value=4.000"), ("-1", "SetGroupVolume group=0 value=0.000")] {
        h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 6).type_chars(typed).press(KeyCode::Enter);
        assert_eq!(h.commands(), [command]);
    }
}

#[test]
fn the_volume_popup_on_any_node_of_a_lane_sets_the_lanes_volume() {
    let mut h = Harness::new(scene_group());
    for addr in [TreeAddress::Effect { inst: 2, index: 0 }, TreeAddress::Modulator { inst: 2, index: 0 }] {
        h.goto(addr);
        h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 6).type_chars("0.6").press(KeyCode::Enter);
        assert_eq!(h.commands(), ["SetVolume inst=2 value=0.600"], "{addr:?}");
    }
    assert_eq!(h.s.ctl.model.instruments[2].volume, 0.6);
    assert_eq!(h.s.ctl.model.instruments[0].volume, 1.0, "the other lanes keep theirs");
}

#[test]
fn the_mix_popup_opens_only_on_an_effect() {
    let mut h = Harness::new(scene_group());
    for addr in [TreeAddress::Instrument(2), TreeAddress::Group(0), TreeAddress::Modulator { inst: 2, index: 0 }] {
        h.goto(addr);
        h.press(KeyCode::Char('x'));
        assert!(h.s.gain_editing.is_none(), "{addr:?}");
    }
}

#[test]
fn a_gain_popup_ignores_what_is_not_a_number_and_escape_cancels() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 6).type_chars("abc").press(KeyCode::Enter);
    assert!(h.s.gain_editing.is_none(), "the popup closes");
    assert_eq!(h.commands(), Vec::<String>::new());
    assert_eq!(h.s.ctl.model.instruments[2].volume, 0.8);
    assert!(!h.s.ctl.dirty);

    h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 6).type_chars("2").press(KeyCode::Esc);
    assert!(h.s.gain_editing.is_none());
    assert_eq!(h.commands(), Vec::<String>::new());
    assert_eq!(h.s.ctl.model.instruments[2].volume, 0.8);
    assert!(!h.s.ctl.dirty);
}

#[test]
fn the_parameter_pane_sets_a_groups_volume_and_pan() {
    // The group's two rows, Volume (0..4, 0.8 to start) and Pan (-1..1, -0.3).
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Enter).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetGroupVolume group=0 value=0.850"]);
    h.press_with(KeyCode::Right, KeyModifiers::CONTROL);
    assert_eq!(h.commands(), ["SetGroupVolume group=0 value=0.950"]);
    assert!((h.s.ctl.model.groups[0].volume - 0.95).abs() < 1e-5);
    assert!(h.s.ctl.dirty);

    h.press(KeyCode::Down).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetGroupPan group=0 value=-0.250"]);
    assert!((h.s.ctl.model.groups[0].pan + 0.25).abs() < 1e-5);
    h.press_n(KeyCode::Right, 30);
    assert_eq!(h.commands().last().map(String::as_str), Some("SetGroupPan group=0 value=1.000"), "stops at hard right");

    // Typed values are clamped to each row's range.
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 8).type_chars("-5").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupPan group=0 value=-1.000"]);
    h.press(KeyCode::Up).press(KeyCode::Enter).press_n(KeyCode::Backspace, 8).type_chars("9").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupVolume group=0 value=4.000"]);
    assert_eq!(h.s.ctl.model.groups[0].volume, 4.0);
}

#[test]
fn the_group_header_shows_the_volume_after_an_edit() {
    // The header carries the group's volume, so it has to follow the edits,
    // whichever way they are made.
    let mut h = Harness::new(scene_group());
    assert!(h.tree().contains(&"▦ Pad  vol 0.80"), "{:#?}", h.tree());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 6).type_chars("0.25").press(KeyCode::Enter);
    assert_eq!(h.s.ctl.model.groups[0].volume, 0.25);
    assert!(h.tree().contains(&"▦ Pad  vol 0.25"), "after the popup: {:#?}", h.tree());

    // From the parameter pane: Volume is its first row.
    h.press(KeyCode::Enter).press(KeyCode::Right);
    assert_eq!(h.s.ctl.model.groups[0].volume, 0.3);
    assert!(h.tree().contains(&"▦ Pad  vol 0.30"), "after a nudge: {:#?}", h.tree());
}

#[test]
fn the_volume_popup_sets_instrument_volume_clamped_to_four() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 4).type_chars("9").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetVolume inst=2 value=4.000"]);
    assert_eq!(h.s.ctl.model.instruments[2].volume, 4.0);
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
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('a')).type_chars("reverb").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["InsertEffect inst=0 index=0 plugin=Reverb mix=0.500"]);
    h.press(KeyCode::Char('a')).type_chars("filter").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["InsertEffect inst=0 index=1 plugin=Filter mix=1.000"]);
    assert_eq!(h.tree()[1], "├ fx Reverb  [Built-in]");
    assert_eq!(h.tree()[2], "├ fx Filter  [Built-in]");
    assert!(h.s.ctl.dirty);
}

#[test]
fn an_effect_from_the_selector_comes_with_its_presets_and_enum_parameters() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('a')).type_chars("reverb").press(KeyCode::Enter);
    h.press(KeyCode::Char('a')).type_chars("filter").press(KeyCode::Enter);

    let effects = &h.s.ctl.model.instruments[0].effects;
    let reverb = &effects[0];
    assert_eq!((reverb.name.as_str(), reverb.id.as_str(), reverb.is_instrument), ("Reverb", "builtin:reverb", false));
    assert!(!reverb.presets.is_empty(), "the preset list comes with the plugin");
    assert_eq!(reverb.current_preset, None);
    let filter = &effects[1];
    let kind = filter.params.iter().find(|p| p.name == "type").map(|p| &p.kind);
    assert!(matches!(kind, Some(ParamKind::Enum(labels)) if labels.len() > 4), "the filter's type is an enum");
    assert!(filter.params.iter().all(|p| p.value == p.default));
}

/// The catalog names a plugin that is not there to load (a scan found it, and it has gone since).
fn catalog_with_a_plugin_that_has_gone(h: &mut Harness) {
    h.s.ctl.catalog.add(vec![plugin::PluginInfo {
        name: "Vanished".into(),
        id: "./no-such-dir/Vanished.clap".into(),
        is_instrument: false,
        param_count: 3,
        preset_count: 0,
        path: "./no-such-dir/Vanished.clap".into(),
        scan_ms: 0,
    }]);
}

#[test]
fn an_effect_that_cannot_be_added_leaves_the_session_as_it_was() {
    let mut h = Harness::new(scene_split());
    catalog_with_a_plugin_that_has_gone(&mut h);
    let tree: Vec<String> = h.tree().iter().map(|label| label.to_string()).collect();
    h.press(KeyCode::Char('a')).type_chars("vanish").press(KeyCode::Enter);
    assert!(h.commands().is_empty(), "nothing is sent to the audio thread");
    assert_eq!(h.tree(), tree, "the tree is as it was");
    assert!(!h.s.ctl.dirty, "nothing was changed");
    assert!(h.s.selector.is_none(), "the popup is closed");
    assert!(h.s.ctl.model.instruments[0].effects.is_empty());
}

#[test]
fn a_bus_effect_that_cannot_be_added_leaves_the_session_as_it_was() {
    let mut h = Harness::new(scene_group());
    catalog_with_a_plugin_that_has_gone(&mut h);
    h.goto(TreeAddress::Group(0));
    let tree: Vec<String> = h.tree().iter().map(|label| label.to_string()).collect();
    let buses = h.s.ctl.model.groups[0].effects.len();
    h.press(KeyCode::Char('a')).type_chars("vanish").press(KeyCode::Enter);
    assert!(h.commands().is_empty(), "nothing is sent to the audio thread");
    assert_eq!(h.tree(), tree, "the tree is as it was");
    assert!(!h.s.ctl.dirty, "nothing was changed");
    assert_eq!(h.s.ctl.model.groups[0].effects.len(), buses);
}

#[test]
fn adding_a_group_effect_from_the_selector() {
    let mut h = Harness::new(scene_group());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('a')).type_chars("filter").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["InsertGroupEffect group=0 index=1 plugin=Filter mix=1.000"]);
    assert!(h.tree().contains(&"  fx Filter  [Built-in]  (bus)"));
}

#[test]
fn n_adds_a_full_range_oscillator_lane_and_opens_the_instrument_selector() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    let lanes = h.s.ctl.model.instruments.len();
    h.press(KeyCode::Char('n'));

    // The lane exists on the audio thread with an oscillator in it already, so
    // that it makes sound even if the selector is dismissed.
    assert_eq!(
        h.commands(),
        ["AddInstrument range=None", "SwapInstrument inst=2 plugin=Oscillator bufs=2 remap=false"]
    );
    assert_eq!(h.s.ctl.model.instruments.len(), lanes + 1);
    let lane = &h.s.ctl.model.instruments[lanes];
    assert_eq!((lane.range, lane.transpose, lane.volume, lane.group), (None, 0, 1.0, None));
    assert!(lane.effects.is_empty() && lane.modulators.is_empty() && lane.pattern.is_none());
    let osc = lane.instrument.as_ref().expect("the lane defaults to the oscillator");
    assert_eq!((osc.name.as_str(), osc.id.as_str(), osc.format.as_str()), ("Oscillator", "builtin:osc", "Built-in"));
    assert!(osc.is_instrument);
    assert_eq!((osc.mix, osc.current_preset.as_deref()), (1.0, None));
    assert!(osc.params.iter().all(|p| p.value == p.default), "a new plugin starts at its defaults");
    let waveform = osc.params.iter().find(|p| p.name == "waveform").unwrap();
    assert!(matches!(&waveform.kind, ParamKind::Enum(labels) if labels.len() == 4), "labelled parameters are enums");
    assert!(h.s.ctl.dirty);
    assert_eq!(h.s.selected_address().copied(), Some(TreeAddress::Instrument(lanes)), "the cursor is on the new lane");
    assert_eq!(h.s.selector.as_ref().map(|sel| sel.mode), Some(SelectorMode::Instrument));
}

#[test]
fn cancelling_the_selector_after_n_leaves_the_oscillator() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('n'));
    h.commands();
    h.press(KeyCode::Esc);
    assert!(h.s.selector.is_none());
    assert_eq!(h.commands(), Vec::<String>::new(), "nothing more is sent");
    assert_eq!(h.s.ctl.model.instruments[2].instrument.as_ref().unwrap().name, "Oscillator");
}

#[test]
fn choosing_a_plugin_after_n_swaps_it_into_the_new_lane() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('n'));
    h.commands();
    h.type_chars("oscillator").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SwapInstrument inst=2 plugin=Oscillator bufs=2 remap=false keep"]);
    assert!(h.s.selector.is_none());
}

#[test]
fn replacing_the_instrument_loads_a_fresh_plugin() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.s.ctl.model.instruments[0].instrument.as_mut().unwrap().params[2].value = 0.25; // volume, edited
    h.press(KeyCode::Char('i')).press(KeyCode::Enter);
    // (The lane's vibrato cable into the old plugin goes first: see the next test.)
    assert_eq!(
        h.commands(),
        ["RemoveModTarget inst=0 mod=0 target=0", "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false keep"]
    );
    let volume = &h.s.ctl.model.instruments[0].instrument.as_ref().unwrap().params[2];
    assert_eq!((volume.name.as_str(), volume.value), ("volume", 1.0), "edits don't survive a swap");
}

#[test]
fn replacing_the_instrument_takes_out_the_cables_into_the_old_one() {
    // The lane's LFO drives the old instrument's `detune` by that parameter's place in the
    // old plugin: left in place it would drive whatever sits there in the new one (f51).
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Char('i')).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        ["RemoveModTarget inst=0 mod=0 target=0", "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false keep"]
    );
    let lfo = &h.s.ctl.model.instruments[0].modulators[0];
    assert!(lfo.targets.is_empty(), "the cable into the old plugin is gone from the model too");
    assert_eq!(h.s.ctl.model.instruments[1].modulators[0].targets.len(), 1, "the other lane's cable stays");
}

#[test]
fn m_on_an_instrument_adds_an_lfo_to_its_lane() {
    let mut h = Harness::new(scene_split());
    h.goto(TreeAddress::Instrument(1));
    h.press(KeyCode::Char('m'));
    assert_eq!(h.commands(), ["InsertModulator inst=1 index=1 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0 }"]);
    assert_eq!(h.tree()[4], "╰ ~ LFO 1.0Hz sine");
}

#[test]
fn m_on_a_group_adds_an_lfo_to_the_groups_rack() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('m'));
    assert_eq!(h.commands(), ["InsertGroupModulator group=0 index=2 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0 }"]);
}

#[test]
fn deleting_a_bus_effect_drops_the_group_modulators_bus_targets() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Char('d'));
    // (The group LFO's cable to the reverb's damping goes first, then the reverb.)
    assert_eq!(h.commands(), ["RemoveGroupModTarget group=0 mod=0 target=1", "RemoveGroupEffect group=0 index=0 keep"]);
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
    // The ADSR's cable to its rate goes first, and then the LFO, with its own cables; what those drove, a member's
    // detune and a bus effect's parameter, has the value the user set.
    assert_eq!(
        h.commands(),
        [
            "RemoveGroupModTarget group=0 mod=1 target=0",
            "RemoveGroupModulator group=0 index=0",
            "SetParameter inst=0 slot=0 param=1 value=0.000",
            "SetGroupParameter group=0 fx=0 param=2 value=0.350"
        ]
    );
    assert_eq!(h.s.ctl.model.groups[0].modulators.len(), 1);
    assert!(h.s.ctl.model.groups[0].modulators[0].targets.is_empty());
    assert!(h.tree().contains(&"  ~ ADSR  (bus)"), "{:#?}", h.tree());
}

#[test]
fn deleting_a_group_member_drops_its_modulator_targets() {
    // Lane 0 is group member 0, targeted by the group LFO's `detune` target. The
    // first `d` takes the cable into its plugin out and clears the plugin, the second
    // removes the (now empty) lane; the member-0 target goes with the plugin and the
    // bus target stays.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Char('d')).press(KeyCode::Char('d'));
    // (The group's cable to the member goes first, then the plugin, then the lane.)
    assert_eq!(h.commands(), ["RemoveGroupModTarget group=0 mod=0 target=0", "ClearInstrument inst=0", "RemoveInstrument inst=0 keep"]);
    let lfo = &h.s.ctl.model.groups[0].modulators[0];
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
    let target = &h.s.ctl.model.groups[0].modulators[0].targets[0];
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
fn the_group_popup_takes_a_lane_out_of_its_group_and_moves_it_to_another() {
    // Lane 1 is the Pad group's second member; the group's cables are all to the first.
    let mut h = Harness::new([scene_group(), "[[group]]\nname = \"Lead\"\n".to_string()].concat());
    h.goto(TreeAddress::Instrument(1));
    // The popup lists "New group", "Ungroup" (the lane is in one), then "Pad (current)" and "Lead".
    h.press(KeyCode::Char('g')).press(KeyCode::Down).press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetLaneGroup inst=1 group=None"]);
    assert_eq!(h.s.ctl.model.instruments[1].group, None);
    assert!(h.s.ctl.dirty);
    // Now in no group, it is offered "New group", then the two groups: "Lead" is the third.
    h.goto(TreeAddress::Instrument(1));
    h.press(KeyCode::Char('g')).press_n(KeyCode::Down, 2).press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetLaneGroup inst=1 group=Some(1)"]);
    assert_eq!(h.s.ctl.model.instruments[1].group, Some(1));
    assert_eq!(h.s.ctl.model.instruments[0].group, Some(0), "the others are where they were");
}

#[test]
fn the_group_popup_takes_a_member_the_group_drives_out_of_it_and_the_group_s_cable_goes_and_gives_the_value_back() {
    // Lane 0 is the Pad group's first member, and the group's LFO drives its detune.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Char('g')).press(KeyCode::Down).press(KeyCode::Enter); // "New group", then "Ungroup"
    assert_eq!(
        h.commands(),
        ["RemoveGroupModTarget group=0 mod=0 target=0", "SetLaneGroup inst=0 group=None", "SetParameter inst=0 slot=0 param=1 value=0.000"]
    );
    assert_eq!(h.s.ctl.model.instruments[0].group, None);
    assert!(h.tree().contains(&"  ~ LFO 0.5Hz sine → Bus Reverb: damping (25%)  (bus)"), "{:#?}", h.tree());
}

#[test]
fn the_rename_popup_names_a_group_and_a_name_of_nothing_gives_the_default_name_back() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('R')).press_n(KeyCode::Backspace, 3); // "Pad" is in the popup
    for ch in "Strings".chars() {
        h.press(KeyCode::Char(ch));
    }
    h.press(KeyCode::Enter);
    assert_eq!(h.s.ctl.model.groups[0].name.as_deref(), Some("Strings"));
    assert!(h.tree().iter().any(|l| l.starts_with("▦ Strings")), "{:#?}", h.tree());
    assert!(h.s.ctl.dirty);
    assert!(h.commands().is_empty(), "the audio thread has no use for a name");
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('R')).press_n(KeyCode::Backspace, 7).press(KeyCode::Enter);
    assert_eq!(h.s.ctl.model.groups[0].name, None);
    assert!(h.tree().iter().any(|l| l.starts_with("▦ Group 1")), "{:#?}", h.tree());
}

#[test]
fn the_rename_popup_takes_no_more_characters_than_a_name_can_have() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('R')).press_n(KeyCode::Backspace, 3);
    h.press_n(KeyCode::Char('x'), 70).press(KeyCode::Enter);
    assert_eq!(h.s.ctl.model.groups[0].name.as_deref().map(|name| name.chars().count()), Some(64));
}

#[test]
fn deleting_a_group_ungroups_its_members() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('d'));
    // The group's three cables go (the LFO's to the first member's detune and to the bus, the ADSR's to the LFO's rate),
    // then the group; and the member that the LFO drove has the detune the user set.
    assert_eq!(
        h.commands(),
        [
            "RemoveGroupModTarget group=0 mod=0 target=0",
            "RemoveGroupModTarget group=0 mod=0 target=0",
            "RemoveGroupModTarget group=0 mod=1 target=0",
            "RemoveGroup group=0 keep",
            "SetParameter inst=0 slot=0 param=1 value=0.000",
        ]
    );
    assert!(h.s.ctl.model.groups.is_empty());
    assert!(h.s.ctl.model.instruments.iter().all(|n| n.group.is_none()));
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

// The engine remaps or drops a modulator's plugin targets when its effects are
// removed or reordered, so that modulation keeps following the same plugin. The
// model has to do the same, or the tree shows the wrong plugin and a saved
// session attaches the modulation to a different effect on reload.

/// The `(slot, kind)` of every target of lane modulator 0 on lane 0.
fn lane_targets(h: &Harness) -> Vec<(usize, String)> {
    h.s.ctl.model.instruments[0].modulators[0].targets.iter().map(|t| (t.slot, format!("{:?}", t.kind))).collect()
}

#[test]
fn deleting_an_effect_drops_its_modulator_targets_and_shifts_later_slots() {
    let mut h = Harness::new(scene_two_effects());
    assert_eq!(h.tree()[3], "╰ ~ LFO 0.5Hz sine → Filter: cutoff (40%), Reverb: damping (30%)");
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('d'));
    // (The cable to the filter goes first, then the filter.)
    assert_eq!(h.commands(), ["RemoveModTarget inst=0 mod=0 target=0", "RemoveEffect inst=0 index=0 keep"]);
    // The reverb's `damping` (parameter 2) moved from slot 2 to slot 1 and the filter's target is gone.
    assert_eq!(lane_targets(&h), [(1, "PluginParam { slot: 1, param_index: 2 }".to_string())]);
    assert_eq!(
        h.tree(),
        ["♪ Oscillator  [Built-in]", "├ fx Reverb  [Built-in]", "╰ ~ LFO 0.5Hz sine → Reverb: damping (30%)"]
    );
}

#[test]
fn deleting_the_last_effect_keeps_the_earlier_targets() {
    let mut h = Harness::new(scene_two_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 1 });
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["RemoveModTarget inst=0 mod=0 target=1", "RemoveEffect inst=0 index=1 keep"]);
    assert_eq!(
        h.tree(),
        ["♪ Oscillator  [Built-in]", "├ fx Filter  [Built-in]", "╰ ~ LFO 0.5Hz sine → Filter: cutoff (40%)"]
    );
}

#[test]
fn reordering_effects_moves_modulator_targets_with_their_plugins() {
    let mut h = Harness::new(scene_two_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    assert!(!h.s.ctl.dirty);
    h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["ReorderEffect inst=0 from=0 to=1"]);
    assert!(h.s.ctl.dirty, "a session with its effects in another order has something to save");
    // Reverb is now first, but the LFO still drives the same two plugins.
    assert_eq!(h.tree()[1], "├ fx Reverb  [Built-in]");
    assert_eq!(h.tree()[2], "├ fx Filter  [Built-in]");
    assert_eq!(h.tree()[3], "╰ ~ LFO 0.5Hz sine → Filter: cutoff (40%), Reverb: damping (30%)");
    let slots: Vec<usize> = lane_targets(&h).iter().map(|t| t.0).collect();
    assert_eq!(slots, [2, 1], "cutoff follows the filter to slot 2, damping the reverb to slot 1");

    // The filter is the last now, and has nowhere to go: nothing is sent, and nothing is left to save.
    h.s.ctl.dirty = false;
    h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
    assert!(h.commands().is_empty());
    assert!(!h.s.ctl.dirty);

    // ...and moving it back restores the original slots.
    h.s.ctl.dirty = false;
    h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["ReorderEffect inst=0 from=1 to=0"]);
    assert!(h.s.ctl.dirty);
    let slots: Vec<usize> = lane_targets(&h).iter().map(|t| t.0).collect();
    assert_eq!(slots, [1, 2]);
    assert_eq!(h.tree()[3], "╰ ~ LFO 0.5Hz sine → Filter: cutoff (40%), Reverb: damping (30%)");
    // Nor does the first go further up.
    h.s.ctl.dirty = false;
    h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
    assert!(h.commands().is_empty());
    assert!(!h.s.ctl.dirty);
}

#[test]
fn reordering_bus_effects_moves_group_modulator_targets_with_their_effects() {
    let mut h = Harness::new(scene_bus_pair());
    let label = "  ~ LFO 0.5Hz sine → Bus Filter: cutoff (40%), Bus Reverb: damping (25%)  (bus)";
    let label = label.replace("(25%)", "(30%)");
    assert!(h.tree().contains(&label.as_str()), "{:#?}", h.tree());
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    assert!(!h.s.ctl.dirty);
    h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["ReorderGroupEffect group=0 from=0 to=1"]);
    assert!(h.s.ctl.dirty, "a session with its bus effects in another order has something to save");
    assert_eq!(h.tree()[2], "  fx Reverb  [Built-in]  (bus)");
    assert_eq!(h.tree()[3], "  fx Filter  [Built-in]  (bus)");
    assert!(h.tree().contains(&label.as_str()), "targets follow their effects: {:#?}", h.tree());
    let kinds: Vec<String> =
        h.s.ctl.model.groups[0].modulators[0].targets.iter().map(|t| format!("{:?}", t.kind)).collect();
    assert_eq!(
        kinds,
        [
            "GroupBus { effect_index: 1, param_index: 0 }".to_string(),
            "GroupBus { effect_index: 0, param_index: 2 }".to_string(),
        ]
    );

    // The filter is the last now, and has nowhere to go: nothing is sent, and nothing is left to save.
    h.s.ctl.dirty = false;
    h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
    assert!(h.commands().is_empty());
    assert!(!h.s.ctl.dirty);

    // Moving the filter back up restores the original indices.
    h.s.ctl.dirty = false;
    h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["ReorderGroupEffect group=0 from=1 to=0"]);
    assert!(h.s.ctl.dirty);
    let kinds: Vec<String> =
        h.s.ctl.model.groups[0].modulators[0].targets.iter().map(|t| format!("{:?}", t.kind)).collect();
    assert_eq!(
        kinds,
        [
            "GroupBus { effect_index: 0, param_index: 0 }".to_string(),
            "GroupBus { effect_index: 1, param_index: 2 }".to_string(),
        ]
    );
    assert!(h.tree().contains(&label.as_str()), "{:#?}", h.tree());
    // Nor does the first go further up.
    h.s.ctl.dirty = false;
    h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
    assert!(h.commands().is_empty());
    assert!(!h.s.ctl.dirty);
}

#[test]
fn a_saved_session_does_not_keep_targets_of_a_deleted_effect() {
    // The symptom the stale model produced: the old target was written out with
    // its old slot, and on reload attached to whichever effect now sits there.
    let mut h = Harness::new(scene_two_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('d'));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.toml");
    h.s.ctl.session_path = Some(path.clone());
    h.s.ctl.save().unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("cutoff"), "the deleted filter's target was saved:\n{saved}");
    assert!(saved.contains("param = \"damping\""), "{saved}");
    assert!(saved.contains("slot = 1"), "the reverb moved to slot 1:\n{saved}");
    assert!(!saved.contains("slot = 2"), "no target may point past the chain:\n{saved}");
}

// A group modulator addresses a member's plugins by (member, slot) too, so the
// same has to hold for it when a member's effects go or move.

/// The `(member, slot, param_index, row slot)` of every target of group modulator
/// 0 that addresses a member's plugin, in the target order.
fn member_targets(h: &Harness) -> Vec<(usize, usize, u32, usize)> {
    use crate::plugin::chain::ModTargetKind;
    h.s.ctl.model.groups[0].modulators[0]
        .targets
        .iter()
        .filter_map(|t| match t.kind {
            ModTargetKind::GroupMember { member, slot, param_index } => Some((member, slot, param_index, t.slot)),
            _ => None,
        })
        .collect()
}

/// The tree line of group modulator 0.
fn group_lfo_line(h: &Harness) -> &str {
    h.tree().into_iter().find(|l| l.contains("~ LFO")).expect("the group LFO is in the tree")
}

const MEMBER_LFO_LINE: &str =
    "  ~ LFO 0.5Hz sine → M0 Filter: cutoff (40%), M0 Reverb: damping (30%), M1 Filter: cutoff (20%)  (bus)";

#[test]
fn a_member_effect_scene_labels_each_target_with_its_plugin() {
    let h = Harness::new(scene_member_effects());
    assert_eq!(group_lfo_line(&h), MEMBER_LFO_LINE);
    // cutoff is parameter 0 of the filter, damping parameter 2 of the reverb.
    assert_eq!(member_targets(&h), [(0, 1, 0, 1), (0, 2, 2, 2), (1, 1, 0, 1)]);
}

#[test]
fn deleting_a_member_effect_drops_its_group_targets_and_shifts_the_later_slots() {
    let mut h = Harness::new(scene_member_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["RemoveGroupModTarget group=0 mod=0 target=0", "RemoveEffect inst=0 index=0 keep"]);
    // Member 0's filter and its target are gone and its reverb is in slot 1 now;
    // member 1 is another lane and keeps its filter's target in slot 1.
    assert_eq!(member_targets(&h), [(0, 1, 2, 1), (1, 1, 0, 1)]);
    assert_eq!(group_lfo_line(&h), "  ~ LFO 0.5Hz sine → M0 Reverb: damping (30%), M1 Filter: cutoff (20%)  (bus)");
}

#[test]
fn deleting_the_last_member_effect_keeps_the_earlier_group_targets() {
    let mut h = Harness::new(scene_member_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 1 });
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["RemoveGroupModTarget group=0 mod=0 target=1", "RemoveEffect inst=0 index=1 keep"]);
    assert_eq!(member_targets(&h), [(0, 1, 0, 1), (1, 1, 0, 1)]);
    assert_eq!(group_lfo_line(&h), "  ~ LFO 0.5Hz sine → M0 Filter: cutoff (40%), M1 Filter: cutoff (20%)  (bus)");
}

#[test]
fn deleting_an_effect_of_the_second_member_leaves_the_first_members_targets() {
    // Lane 1 is member 1: the ordinal, not the lane index, finds its targets.
    let mut h = Harness::new(scene_member_effects());
    h.goto(TreeAddress::Effect { inst: 1, index: 0 });
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["RemoveGroupModTarget group=0 mod=0 target=2", "RemoveEffect inst=1 index=0 keep"]);
    assert_eq!(member_targets(&h), [(0, 1, 0, 1), (0, 2, 2, 2)]);
}

#[test]
fn deleting_an_effect_of_a_lane_outside_the_group_leaves_the_group_targets() {
    // The same scene with an ungrouped lane in front: its effect is not a member's.
    let scene = format!(
        "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n{}",
        scene_member_effects()
    );
    let mut h = Harness::new(scene);
    assert_eq!(member_targets(&h).len(), 3);
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["RemoveEffect inst=0 index=0 keep"]);
    assert_eq!(member_targets(&h), [(0, 1, 0, 1), (0, 2, 2, 2), (1, 1, 0, 1)]);
}

#[test]
fn reordering_member_effects_moves_the_group_targets_with_their_plugins() {
    let mut h = Harness::new(scene_member_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["ReorderEffect inst=0 from=0 to=1"]);
    // The reverb is first now, but the LFO still drives the same two plugins:
    // cutoff follows the filter to slot 2, damping the reverb to slot 1.
    assert_eq!(member_targets(&h), [(0, 2, 0, 2), (0, 1, 2, 1), (1, 1, 0, 1)]);
    assert_eq!(group_lfo_line(&h), MEMBER_LFO_LINE);

    // ...and moving it back restores the original slots.
    h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["ReorderEffect inst=0 from=1 to=0"]);
    assert_eq!(member_targets(&h), [(0, 1, 0, 1), (0, 2, 2, 2), (1, 1, 0, 1)]);
    assert_eq!(group_lfo_line(&h), MEMBER_LFO_LINE);
}

#[test]
fn a_saved_session_does_not_keep_group_targets_of_a_deleted_member_effect() {
    // The symptom the stale model produced: the reverb's target was written out
    // with its old slot 2, and the filter's with slot 1, so on reload the filter's
    // modulation landed on the reverb and the reverb's on nothing.
    let mut h = Harness::new(scene_member_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('d'));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.toml");
    h.s.ctl.session_path = Some(path.clone());
    h.s.ctl.save().unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("slot = 2"), "no target may point past the member's chain:\n{saved}");
    assert_eq!(saved.matches("param = \"cutoff\"").count(), 1, "only member 1's cutoff is left:\n{saved}");
    assert_eq!(saved.matches("param = \"damping\"").count(), 1, "{saved}");
    assert_eq!(saved.matches("slot = 1").count(), 2, "member 0's damping and member 1's cutoff:\n{saved}");
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

// ---------------------------------------------------------------------------
// The CLIP light
// ---------------------------------------------------------------------------

#[test]
fn the_status_bar_shows_clip_while_the_master_meter_is_clipping() {
    use crate::telemetry::MASTER;
    let mut h = Harness::new(scene_split());
    let t0 = Instant::now();
    tick_at(&mut h.s, t0);
    assert!(!h.dump().contains("CLIP"), "dark until the audio thread counts a clipped buffer");

    h.s.ctl.telemetry.bump(MASTER.clips);
    tick_at(&mut h.s, t0 + Duration::from_millis(100));
    assert_golden("status_clip", &mut h);

    tick_at(&mut h.s, t0 + Duration::from_millis(2_099));
    assert!(h.dump().contains("CLIP"), "still on just under two seconds after the last clip");
    tick_at(&mut h.s, t0 + Duration::from_millis(2_100));
    assert!(!h.dump().contains("CLIP"), "and off after");
    assert!(h.dump().contains("120 BPM"), "the tempo is still there");
}

#[test]
fn the_tick_the_event_loop_runs_lights_the_clip_light_too() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.telemetry.bump(crate::telemetry::MASTER.clips);
    tick(&mut h.s);
    assert!(h.s.clip.is_lit());
    assert!(h.dump().contains("CLIP"));
}

#[test]
fn clips_counted_before_the_tui_started_do_not_light_it() {
    use crate::app::model::SessionModel;
    use crate::telemetry::MASTER;
    let (engine, _audio) = stub_engine();
    engine.telemetry.bump(MASTER.clips); // the engine clipped while the splash was up
    let mut s = State::new(Controller::new(SessionModel::default(), engine, None));
    tick_at(&mut s, Instant::now());
    assert!(!s.clip.is_lit());
    s.ctl.telemetry.bump(MASTER.clips);
    tick_at(&mut s, Instant::now());
    assert!(s.clip.is_lit(), "a clip after it started watching does");
}

#[test]
fn on_a_narrow_terminal_the_clip_light_takes_the_right_end_of_the_tab_bar_like_the_tempo() {
    use crate::telemetry::MASTER;
    let mut h = Harness::new(scene_split());
    h.s.ctl.telemetry.bump(MASTER.clips);
    tick_at(&mut h.s, Instant::now());
    let first_row = |h: &mut Harness, width: u16| {
        h.terminal = Terminal::new(TestBackend::new(width, HEIGHT)).unwrap();
        // The dump starts with its size and a heading; the first row of text follows.
        h.dump().lines().nth(2).unwrap().to_string()
    };
    let row = first_row(&mut h, 30);
    assert!(row.starts_with("(1) Session"), "{row}");
    assert!(row.contains("CLIP  120 BPM"), "the light is not lost for want of room: {row}");
    // With no room for the status at all, it is left out (as the tempo always was):
    // it wants its 13 columns and two to spare.
    let row = first_row(&mut h, 15);
    assert!(!row.contains("CLIP") && !row.contains("BPM"), "{row}");
    let row = first_row(&mut h, 16);
    assert!(row.contains("CLIP  120 BPM"), "{row}");
}

/// A real audio graph playing a session of two oscillator lanes at `lane_volume`
/// each, which publishes into the controller's telemetry, with one note held for a
/// few buffers: the loudest sample it produced, and whether the status bar shows
/// CLIP afterwards. (One oscillator peaks at a quarter of full scale at volume 1.)
fn clip_light_after_playing(lane_volume: f32) -> (f32, bool) {
    let lane = format!("[[instrument]]\nplugin = \"builtin:osc\"\nvolume = {lane_volume}\n");
    let config = session::parse(&format!("{lane}{lane}")).unwrap();
    let (handles, engine) = stub_engine();
    let model = load_session(
        &config,
        Path::new("."),
        SAMPLE_RATE,
        BLOCK,
        &plugin::Runtime::default(),
        &handles.cmd_tx,
        &mut |_| {},
    )
    .unwrap();
    let mut graph = crate::plugin::chain::AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
    graph.set_telemetry(handles.telemetry.clone());
    let mut s = State::new(Controller::new(model, handles, None));

    let mut out = vec![vec![0.0f32; BLOCK]; 2];
    let t0 = Instant::now();
    tick_at(&mut s, t0);
    graph.process(&[(0, [0x90, 60, 127])], &mut out).unwrap();
    let mut loudest = 0.0f32;
    for _ in 0..8 {
        graph.process(&[], &mut out).unwrap();
        loudest = out[0].iter().fold(loudest, |loudest, sample| loudest.max(sample.abs()));
    }
    tick_at(&mut s, t0 + Duration::from_millis(100));
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    terminal.draw(|frame| draw(frame, &mut s)).unwrap();
    (loudest, dump_buffer(terminal.backend().buffer()).contains("CLIP"))
}

#[test]
fn an_engine_that_clips_lights_the_clip_light_and_one_that_does_not_leaves_it_dark() {
    let (loudest, lit) = clip_light_after_playing(4.0);
    assert!(loudest > 1.0, "the oscillator at four times the volume is beyond full scale: {loudest}");
    assert!(lit);

    let (loudest, lit) = clip_light_after_playing(0.5);
    assert!(loudest > 0.1 && loudest <= 1.0, "a note that is heard and within full scale: {loudest}");
    assert!(!lit);
}

// The ways the TUI makes a modulator or binds a target, pinned command for
// command: the modulate popup (`m` on a parameter), the target selector (`t` on a
// modulator) and MIDI-learn (`l` on a parameter). It asks the controller by the
// intents a browser asks by (`AddModulator`, `AddModEdge`, `SetEdgeDepth`).

/// What the target selector lists, one line for each row: the cells the list shows, then the way the rack addresses the target. A run of
/// plugin parameters (a chain's) is one line, with its first.
fn selector_rows(h: &Harness) -> Vec<String> {
    let selector = h.s.target_selector.as_ref().expect("the selector is open");
    let mut rows: Vec<String> = Vec::new();
    for (item, entry) in selector.items.iter().zip(&selector.entries) {
        let is_param = matches!(
            entry.kind,
            crate::plugin::chain::ModTargetKind::PluginParam { .. }
                | crate::plugin::chain::ModTargetKind::GroupMember { .. }
                | crate::plugin::chain::ModTargetKind::GroupBus { .. }
        );
        let row = if is_param {
            format!("params of {}", item.cells[0])
        } else {
            format!("{} | {:?}", item.cells.join(" | "), entry.kind)
        };
        if !(is_param && rows.last() == Some(&row)) {
            rows.push(row);
        }
    }
    rows
}

#[test]
fn the_target_selector_of_a_lane_modulator_lists_the_chain_and_the_siblings_settings_and_cable_depths() {
    let mut h = Harness::new(crate::app::testutil::CROSS_MOD_RACKS);
    h.goto(TreeAddress::Modulator { inst: 0, index: 0 });
    h.press(KeyCode::Char('t'));
    let expected: Vec<&str> = vec![
        "params of Oscillator",
        "Mod | Mod 1 attack | ModulatorAttack { mod_index: 1 }",
        "Mod | Mod 1 decay | ModulatorDecay { mod_index: 1 }",
        "Mod | Mod 1 sustain | ModulatorSustain { mod_index: 1 }",
        "Mod | Mod 1 release | ModulatorRelease { mod_index: 1 }",
        "Mod | Mod 1 volume depth | ModulatorDepth { mod_index: 1, target_index: 0 }",
        "Mod | Mod 1 detune depth | ModulatorDepth { mod_index: 1, target_index: 1 }",
        "Mod | Mod 2 pan depth | ModulatorDepth { mod_index: 2, target_index: 0 }",
    ];
    assert_eq!(selector_rows(&h), expected);
}

#[test]
fn the_target_selector_of_a_group_modulator_lists_the_members_the_members_modulators_the_bus_and_the_siblings() {
    let mut h = Harness::new(crate::app::testutil::CROSS_MOD_RACKS);
    h.goto(TreeAddress::GroupModulator { group: 0, index: 0 });
    h.press(KeyCode::Char('t'));
    let expected: Vec<&str> = vec![
        "params of M0 Oscillator",
        "params of M1 Oscillator",
        "M0 Mod0 | rate | GroupMemberMod { member: 0, mod_index: 0, field: Rate }",
        "M0 Mod0 | detune depth | GroupMemberMod { member: 0, mod_index: 0, field: Depth(0) }",
        "M0 Mod1 | attack | GroupMemberMod { member: 0, mod_index: 1, field: Attack }",
        "M0 Mod1 | decay | GroupMemberMod { member: 0, mod_index: 1, field: Decay }",
        "M0 Mod1 | sustain | GroupMemberMod { member: 0, mod_index: 1, field: Sustain }",
        "M0 Mod1 | release | GroupMemberMod { member: 0, mod_index: 1, field: Release }",
        "M0 Mod1 | volume depth | GroupMemberMod { member: 0, mod_index: 1, field: Depth(0) }",
        "M0 Mod1 | detune depth | GroupMemberMod { member: 0, mod_index: 1, field: Depth(1) }",
        "M0 Mod2 | pan depth | GroupMemberMod { member: 0, mod_index: 2, field: Depth(0) }",
        "M1 Mod0 | rate | GroupMemberMod { member: 1, mod_index: 0, field: Rate }",
        "params of Bus Reverb",
        "Mod | Mod 1 attack | ModulatorAttack { mod_index: 1 }",
        "Mod | Mod 1 decay | ModulatorDecay { mod_index: 1 }",
        "Mod | Mod 1 sustain | ModulatorSustain { mod_index: 1 }",
        "Mod | Mod 1 release | ModulatorRelease { mod_index: 1 }",
        "Mod | Mod 1 damping depth | ModulatorDepth { mod_index: 1, target_index: 0 }",
        "Mod | Mod 2 volume depth | ModulatorDepth { mod_index: 2, target_index: 0 }",
    ];
    assert_eq!(selector_rows(&h), expected);
}

/// `(slot, param_name, kind, depth, min, max)` of every target of a modulator.
fn target_fields(targets: &[crate::app::model::ModTargetSlot]) -> Vec<(usize, String, String, f32, f32, f32)> {
    targets
        .iter()
        .map(|t| (t.slot, t.param_name.clone(), format!("{:?}", t.kind), t.depth, t.param_min, t.param_max))
        .collect()
}

/// The bass lane's filter with its `cutoff` (0.35, 0 to 1) selected in the parameter pane.
fn on_the_bass_filters_cutoff() -> Harness {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter);
    h
}

#[test]
fn the_modulate_popup_makes_an_lfo_in_the_lane_and_binds_the_parameter_to_it() {
    let mut h = on_the_bass_filters_cutoff();
    h.press(KeyCode::Char('m')).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        [
            "InsertModulator inst=2 index=1 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0 }",
            "AddModTarget inst=2 mod=1 kind=PluginParam { slot: 1, param_index: 0 } depth=0.500 base=0.350 range=[0.000, 1.000]",
        ]
    );
    let made = &h.s.ctl.model.instruments[2].modulators[1];
    assert!(matches!(made.source, ModSourceSlot::Lfo { rate, .. } if rate == 1.0));
    assert_eq!(
        target_fields(&made.targets),
        [(1, "cutoff".to_string(), "PluginParam { slot: 1, param_index: 0 }".to_string(), 0.5, 0.0, 1.0)]
    );
    assert!(h.s.ctl.dirty);
}

#[test]
fn the_modulate_popup_can_make_an_envelope_instead() {
    let mut h = on_the_bass_filters_cutoff();
    h.press(KeyCode::Char('m')).press(KeyCode::Down).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        [
            "InsertModulator inst=2 index=1 source=Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }",
            "AddModTarget inst=2 mod=1 kind=PluginParam { slot: 1, param_index: 0 } depth=0.500 base=0.350 range=[0.000, 1.000]",
        ]
    );
    let made = &h.s.ctl.model.instruments[2].modulators[1];
    assert!(
        matches!(
            made.source,
            ModSourceSlot::Envelope { attack, decay, sustain, release }
                if (attack, decay, sustain, release) == (0.01, 0.3, 0.7, 0.5)
        ),
        "{:?}",
        made.source
    );
    assert_eq!(made.targets.len(), 1);
}

#[test]
fn the_modulate_popup_can_attach_the_parameter_to_a_modulator_that_is_there() {
    let mut h = on_the_bass_filters_cutoff();
    h.press(KeyCode::Down); // the filter's resonance, which the bass's LFO does not drive
    h.press(KeyCode::Char('m')).press(KeyCode::Down).press(KeyCode::Down).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        ["AddModTarget inst=2 mod=0 kind=PluginParam { slot: 1, param_index: 1 } depth=0.500 base=0.707 range=[0.500, 10.000]"],
        "no new modulator, a second target on the bass's LFO"
    );
    let modulators = &h.s.ctl.model.instruments[2].modulators;
    assert_eq!(modulators.len(), 1);
    assert_eq!(target_fields(&modulators[0].targets).len(), 2);
    assert_eq!(target_fields(&modulators[0].targets)[1].1, "resonance");
}

#[test]
fn the_modulate_popup_does_not_attach_a_parameter_to_a_modulator_that_drives_it_already() {
    // (The controller refuses the second cable between the two, in the browser as here.)
    let mut h = on_the_bass_filters_cutoff();
    h.press(KeyCode::Char('m')).press(KeyCode::Down).press(KeyCode::Down).press(KeyCode::Enter);
    assert!(h.commands().is_empty());
    assert_eq!(h.s.ctl.model.instruments[2].modulators[0].targets.len(), 1, "the LFO's cable to the cutoff, and no more");
    assert_eq!(h.s.ctl.history_state().undo, None);
}

#[test]
fn the_modulate_popup_on_a_bus_parameter_makes_a_group_modulator() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('m')).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        [
            "InsertGroupModulator group=0 index=2 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0 }",
            "AddGroupModTarget group=0 mod=2 kind=GroupBus { effect_index: 0, param_index: 0 } depth=0.500 base=0.880 range=[0.100, 0.990]",
        ]
    );
    let made = &h.s.ctl.model.groups[0].modulators[2];
    assert_eq!(
        target_fields(&made.targets),
        [(0, "decay".to_string(), "GroupBus { effect_index: 0, param_index: 0 }".to_string(), 0.5, 0.1, 0.99)]
    );
}

#[test]
fn the_target_selector_binds_the_chosen_parameter_to_the_modulator() {
    // The first row of each list: the lane's own oscillator `waveform`, and the
    // first member's.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Modulator { inst: 2, index: 0 });
    h.press(KeyCode::Char('t')).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        ["AddModTarget inst=2 mod=0 kind=PluginParam { slot: 0, param_index: 0 } depth=0.500 base=0.000 range=[0.000, 3.000]"]
    );
    let bound = target_fields(&h.s.ctl.model.instruments[2].modulators[0].targets);
    assert_eq!(bound.len(), 2, "the LFO had one target already");
    assert_eq!(
        bound[1],
        (0, "waveform".to_string(), "PluginParam { slot: 0, param_index: 0 }".to_string(), 0.5, 0.0, 3.0)
    );

    // Further down the list are the lane's effects: the oscillator's eight
    // parameters, then the filter's `cutoff` (which the LFO drives already: the controller refuses a second cable) and its `resonance` (chain slot 1).
    h.goto(TreeAddress::Modulator { inst: 2, index: 0 });
    h.press(KeyCode::Char('t')).press_n(KeyCode::Down, 9).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        ["AddModTarget inst=2 mod=0 kind=PluginParam { slot: 1, param_index: 1 } depth=0.500 base=0.707 range=[0.500, 10.000]"]
    );
    let bound = target_fields(&h.s.ctl.model.instruments[2].modulators[0].targets);
    assert_eq!(bound.len(), 3);
    assert_eq!(
        bound[2],
        (1, "resonance".to_string(), "PluginParam { slot: 1, param_index: 1 }".to_string(), 0.5, 0.5, 10.0),
        "the chain slot of a filter's parameter is 1"
    );

    h.goto(TreeAddress::GroupModulator { group: 0, index: 0 });
    h.press(KeyCode::Char('t')).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        ["AddGroupModTarget group=0 mod=0 kind=GroupMember { member: 0, slot: 0, param_index: 0 } depth=0.500 base=1.000 range=[0.000, 3.000]"]
    );
    let bound = target_fields(&h.s.ctl.model.groups[0].modulators[0].targets);
    assert_eq!(bound.len(), 3, "the group LFO had two targets already");
    assert_eq!(
        bound[2],
        (0, "waveform".to_string(), "GroupMember { member: 0, slot: 0, param_index: 0 }".to_string(), 0.5, 0.0, 3.0)
    );
}

#[test]
fn the_target_selector_binds_to_the_modulator_it_was_opened_for_whichever_place_it_has_in_its_rack() {
    // The envelope, the second of three in the first lane of the session.
    let mut h = Harness::new(crate::app::testutil::CROSS_MOD_RACKS);
    h.goto(TreeAddress::Modulator { inst: 0, index: 1 });
    h.press(KeyCode::Char('t')).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        ["AddModTarget inst=0 mod=1 kind=PluginParam { slot: 0, param_index: 0 } depth=0.500 base=0.000 range=[0.000, 3.000]"]
    );
    assert_eq!(h.s.ctl.model.instruments[0].modulators[1].targets.len(), 3);
}

#[test]
fn the_modulate_popup_attaches_the_parameter_to_the_modulator_that_was_chosen_whichever_place_it_has_in_its_rack() {
    // The envelope again: the popup lists a new LFO, a new envelope, and then the three that are there.
    let mut h = Harness::new(crate::app::testutil::CROSS_MOD_RACKS);
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Enter).press(KeyCode::Char('m')).press_n(KeyCode::Down, 3).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        ["AddModTarget inst=0 mod=1 kind=PluginParam { slot: 0, param_index: 0 } depth=0.500 base=0.000 range=[0.000, 3.000]"]
    );
    assert_eq!(h.s.ctl.model.instruments[0].modulators[1].targets.len(), 3);
}

#[test]
fn learning_a_parameter_arms_a_midi_modulator_bound_to_it_at_full_depth() {
    let mut h = on_the_bass_filters_cutoff();
    h.press(KeyCode::Char('l'));
    assert_eq!(
        h.commands(),
        [
            "InsertModulator inst=2 index=1 source=MidiLearn { source: None, value: 0.0, learning: true }",
            "AddModTarget inst=2 mod=1 kind=PluginParam { slot: 1, param_index: 0 } depth=0.500 base=0.350 range=[0.000, 1.000]",
            "SetModTargetDepth inst=2 mod=1 target=0 depth=1.000",
        ]
    );
    let armed = &h.s.ctl.model.instruments[2].modulators[1];
    assert!(matches!(armed.source, ModSourceSlot::MidiLearn { source: None, learning: true }));
    assert_eq!(
        target_fields(&armed.targets),
        [(1, "cutoff".to_string(), "PluginParam { slot: 1, param_index: 0 }".to_string(), 1.0, 0.0, 1.0)]
    );

    // On a group's bus effect it goes in the group's rack.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('l'));
    assert_eq!(
        h.commands(),
        [
            "InsertGroupModulator group=0 index=2 source=MidiLearn { source: None, value: 0.0, learning: true }",
            "AddGroupModTarget group=0 mod=2 kind=GroupBus { effect_index: 0, param_index: 0 } depth=0.500 base=0.880 range=[0.100, 0.990]",
            "SetGroupModTargetDepth group=0 mod=2 target=0 depth=1.000",
        ]
    );
    assert_eq!(h.s.ctl.model.groups[0].modulators[2].targets.len(), 1);
}

// ---------------------------------------------------------------------------
// Between frames
// ---------------------------------------------------------------------------

#[test]
fn the_tui_reaps_plugins_the_audio_thread_hands_back() {
    // Nothing read this channel while the TUI ran: removed plugins piled up in its
    // 16 slots and, once it was full, the audio thread dropped them itself.
    let mut h = Harness::new(scene_split());
    let (probe, dropped_on) = DropProbe::recording();
    h.return_tx.send(Box::new(probe)).unwrap();
    let before = *dropped_on.lock().unwrap();
    assert!(before.is_none());

    tick(&mut h.s);
    let after = *dropped_on.lock().unwrap();
    assert_eq!(after, Some(std::thread::current().id()));
}

// ---------------------------------------------------------------------------
// Modulator rows: what Left / Right and typing a value do to a modulator
// ---------------------------------------------------------------------------
//
// An LFO has the rows Type, Waveform, Rate, a separator and then one depth a target;
// an envelope Type, Attack, Decay, Sustain, Release, a separator and the depths; a MIDI
// control a row about its source, a separator and the depths. A lane's modulator and a
// group's are the same to the user and different to the engine, which is told with the
// commands of the lane or of the group, so both are pinned.

/// The cursor on the modulator at `addr`, in the parameter pane (from the chain), on row
/// `row`. (The pane keeps the row it was on, so it is moved from there.)
fn on_modulator_row(h: &mut Harness, addr: TreeAddress, row: usize) {
    h.goto(addr);
    h.press(KeyCode::Enter);
    while h.s.param_state.selected < row {
        h.press(KeyCode::Down);
    }
    while h.s.param_state.selected > row {
        h.press(KeyCode::Up);
    }
    assert_eq!(h.s.param_state.selected, row);
}

/// Enter a value in the popup of the row the cursor is on.
fn enter_value(h: &mut Harness, text: &str) {
    h.press(KeyCode::Enter);
    assert!(h.s.editing.is_some(), "the value popup opens");
    h.press_n(KeyCode::Backspace, 8).type_chars(text).press(KeyCode::Enter);
    assert!(h.s.editing.is_none(), "and closes");
}

/// A lane with an envelope (A 0.1, D 0.2, S 0.5, R 0.4) and a MIDI control bound to CC 74,
/// each with a target.
const LANE_ENVELOPE_AND_CONTROL: &str = r#"
[[instrument]]
plugin = "builtin:osc"

[[instrument.modulator]]
type = "envelope"
attack = 0.1
decay = 0.2
sustain = 0.5
release = 0.4
[[instrument.modulator.target]]
param = "volume"
depth = 0.5

[[instrument.modulator]]
type = "midi"
midi_cc = 74
[[instrument.modulator.target]]
param = "volume"
depth = 0.5
"#;

fn lane_source(h: &Harness, inst: usize, index: usize) -> &ModSourceSlot {
    &h.s.ctl.model.instruments[inst].modulators[index].source
}

fn group_source(h: &Harness, group: usize, index: usize) -> &ModSourceSlot {
    &h.s.ctl.model.groups[group].modulators[index].source
}

fn lane_depth(h: &Harness, inst: usize, index: usize, target: usize) -> f32 {
    h.s.ctl.model.instruments[inst].modulators[index].targets[target].depth
}

fn group_depth(h: &Harness, group: usize, index: usize, target: usize) -> f32 {
    h.s.ctl.model.groups[group].modulators[index].targets[target].depth
}

/// The `(attack, decay, sustain, release)` of an envelope source.
fn stages(source: &ModSourceSlot) -> (f32, f32, f32, f32) {
    match *source {
        ModSourceSlot::Envelope { attack, decay, sustain, release } => (attack, decay, sustain, release),
        ref other => panic!("not an envelope: {other:?}"),
    }
}

fn lfo_rate(source: &ModSourceSlot) -> f32 {
    match *source {
        ModSourceSlot::Lfo { rate, .. } => rate,
        ref other => panic!("not an LFO: {other:?}"),
    }
}

fn lfo_waveform(source: &ModSourceSlot) -> crate::plugin::chain::LfoWaveform {
    match *source {
        ModSourceSlot::Lfo { waveform, .. } => waveform,
        ref other => panic!("not an LFO: {other:?}"),
    }
}

#[test]
fn left_and_right_step_an_lfos_rate_by_a_twentieth_with_shift_a_hundredth_with_ctrl_a_tenth() {
    // (The step is that of a range of 1, whatever the range of the row: here 0.01 to 50.)
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 0 }, 2);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetModulatorRate inst=0 mod=0 rate=4.050"]);
    h.press_with(KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["SetModulatorRate inst=0 mod=0 rate=4.060"]);
    h.press_with(KeyCode::Left, KeyModifiers::CONTROL);
    assert_eq!(h.commands(), ["SetModulatorRate inst=0 mod=0 rate=3.960"]);
    h.press(KeyCode::Left);
    assert_eq!(h.commands(), ["SetModulatorRate inst=0 mod=0 rate=3.910"]);
    assert!((lfo_rate(lane_source(&h, 0, 0)) - 3.91).abs() < 1e-5);
    assert!(h.s.ctl.dirty);
    assert!(h.tree().iter().any(|l| l.contains("LFO 3.9Hz")), "the tree shows the new rate: {:#?}", h.tree());
    // The other lane's LFO is as it was.
    assert_eq!(lfo_rate(lane_source(&h, 1, 0)), 7.0);
}

#[test]
fn a_value_typed_for_an_lfos_rate_is_clamped_to_its_range() {
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 0 }, 2);
    for (text, rate) in [("12.5", 12.5), ("999", 50.0), ("0", 0.01), ("-3", 0.01)] {
        enter_value(&mut h, text);
        assert_eq!(h.commands(), [format!("SetModulatorRate inst=0 mod=0 rate={rate:.3}")], "{text}");
        assert_eq!(lfo_rate(lane_source(&h, 0, 0)), rate, "{text}");
    }
    // At the end of its range a step sends the value that stands.
    enter_value(&mut h, "50");
    h.commands();
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetModulatorRate inst=0 mod=0 rate=50.000"]);
    assert_eq!(lfo_rate(lane_source(&h, 0, 0)), 50.0);
}

#[test]
fn left_and_right_on_an_lfos_waveform_go_round_the_four_whatever_the_modifiers() {
    use crate::plugin::chain::LfoWaveform::{Saw, Sine, Square, Triangle};
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 0 }, 1);
    let mut seen = Vec::new();
    for (code, modifiers) in [
        (KeyCode::Right, KeyModifiers::NONE),
        (KeyCode::Right, KeyModifiers::SHIFT),
        (KeyCode::Right, KeyModifiers::CONTROL),
        (KeyCode::Right, KeyModifiers::NONE), // round to the first again
        (KeyCode::Left, KeyModifiers::NONE),  // and back round to the last
        (KeyCode::Left, KeyModifiers::CONTROL),
    ] {
        h.press_with(code, modifiers);
        seen.push((h.commands(), lfo_waveform(lane_source(&h, 0, 0))));
    }
    let said = |w: &str| vec![format!("SetModulatorWaveform inst=0 mod=0 waveform={w}")];
    assert_eq!(
        seen,
        [
            (said("Triangle"), Triangle),
            (said("Saw"), Saw),
            (said("Square"), Square),
            (said("Sine"), Sine),
            (said("Square"), Square),
            (said("Saw"), Saw),
        ]
    );
    assert!(h.tree().iter().any(|l| l.contains("LFO 4.0Hz saw")), "the tree shows it: {:#?}", h.tree());
}

#[test]
fn no_popup_opens_for_a_waveform_or_a_type_or_a_separator_and_a_separator_does_not_step() {
    let mut h = Harness::new(scene_split());
    for row in [0, 1, 3] {
        on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 0 }, row);
        h.press(KeyCode::Enter);
        assert!(h.s.editing.is_none(), "row {row}: there is no number to type");
        assert_eq!(h.commands(), Vec::<String>::new(), "row {row}");
        h.press(KeyCode::Esc);
    }
    on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 0 }, 3);
    h.press(KeyCode::Right).press(KeyCode::Left);
    assert_eq!(h.commands(), Vec::<String>::new(), "the separator");
    assert_eq!(format!("{:?}", lane_source(&h, 0, 0)), "Lfo { waveform: Sine, rate: 4.0 }");
}

#[test]
fn left_and_right_step_the_depth_of_a_target_and_a_typed_depth_goes_from_minus_one_to_one() {
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 0 }, 4);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetModTargetDepth inst=0 mod=0 target=0 depth=0.090"]);
    h.press_with(KeyCode::Left, KeyModifiers::CONTROL);
    assert_eq!(h.commands(), ["SetModTargetDepth inst=0 mod=0 target=0 depth=-0.010"]);
    for (text, depth) in [("0.5", 0.5), ("5", 1.0), ("-5", -1.0), ("-0.25", -0.25)] {
        enter_value(&mut h, text);
        assert_eq!(h.commands(), [format!("SetModTargetDepth inst=0 mod=0 target=0 depth={depth:.3}")], "{text}");
        assert_eq!(lane_depth(&h, 0, 0, 0), depth, "{text}");
    }
    assert!(h.s.ctl.dirty);
    // At the end of its range a step sends the value that stands.
    enter_value(&mut h, "1");
    h.commands();
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetModTargetDepth inst=0 mod=0 target=0 depth=1.000"]);
}

#[test]
fn the_tree_shows_the_depth_of_a_target_as_soon_as_it_is_edited() {
    // The tree says how deep each target goes (`(4%)`): a lane's modulator and a group's.
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 0 }, 4);
    h.press(KeyCode::Right);
    assert!(h.tree().contains(&"╰ ~ LFO 4.0Hz sine → Oscillator: detune (9%)"), "{:#?}", h.tree());
    enter_value(&mut h, "0.5");
    assert!(h.tree().contains(&"╰ ~ LFO 4.0Hz sine → Oscillator: detune (50%)"), "{:#?}", h.tree());

    let mut h = Harness::new(scene_group());
    on_modulator_row(&mut h, TreeAddress::GroupModulator { group: 0, index: 0 }, 4);
    h.press(KeyCode::Right);
    assert!(h.tree().iter().any(|l| l.contains("M0 Oscillator: detune (45%)")), "{:#?}", h.tree());
    enter_value(&mut h, "0.1");
    assert!(h.tree().iter().any(|l| l.contains("M0 Oscillator: detune (10%)")), "{:#?}", h.tree());
}

#[test]
fn a_modulator_edited_in_the_tui_is_told_to_the_subscribers_as_events_and_not_again_as_a_snapshot() {
    use crate::app::intent::{Event, ModulatorField};
    let mut h = Harness::new(scene_split());
    let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::<crate::app::intent::Applied>::new()));
    let sink = heard.clone();
    h.s.ctl.subscribe(Box::new(move |applied: &crate::app::intent::Applied| sink.lock().unwrap().push(applied.clone())));
    let (osc_lfo, edge) = (h.s.ctl.model.instruments[0].modulators[0].node, h.s.ctl.model.instruments[0].modulators[0].targets[0].edge);

    on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 0 }, 2);
    h.press(KeyCode::Right);
    enter_value(&mut h, "99");
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 0 }, 1);
    h.press(KeyCode::Right);
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 0 }, 4);
    enter_value(&mut h, "-0.5");

    let events: Vec<(u64, Vec<Event>)> = heard.lock().unwrap().iter().map(|a| (a.rev, a.events.clone())).collect();
    assert_eq!(
        events,
        [
            (1, vec![Event::ModulatorFieldSet { node: osc_lfo, field: ModulatorField::Rate, value: 4.05 }]),
            (2, vec![Event::ModulatorFieldSet { node: osc_lfo, field: ModulatorField::Rate, value: 50.0 }]),
            (3, vec![Event::ModulatorFieldSet { node: osc_lfo, field: ModulatorField::Waveform, value: 1.0 }]),
            (4, vec![Event::EdgeDepthSet { edge, depth: -0.5 }]),
        ]
    );
    assert!(!h.s.ctl.resync(), "what the subscribers heard is all there is to tell: no snapshot follows");
    assert_eq!(h.s.ctl.rev, 4);
}

#[test]
fn the_type_row_turns_an_lfo_into_an_envelope_and_back_with_the_defaults_of_each() {
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 0 }, 0);
    h.press(KeyCode::Right);
    assert_eq!(
        h.commands(),
        ["SetModulatorSource inst=0 mod=0 source=Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }"]
    );
    assert_eq!(stages(lane_source(&h, 0, 0)), (0.01, 0.3, 0.7, 0.5));
    assert_eq!(h.s.param_state.selected, 0);
    assert!(h.tree().iter().any(|l| l.contains("ADSR")), "{:#?}", h.tree());
    h.press(KeyCode::Left);
    assert_eq!(
        h.commands(),
        ["SetModulatorSource inst=0 mod=0 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0 }"]
    );
    assert_eq!(format!("{:?}", lane_source(&h, 0, 0)), "Lfo { waveform: Sine, rate: 1.0 }");
    assert!(h.s.ctl.dirty);
}

#[test]
fn left_and_right_step_one_stage_of_an_envelope_and_the_engine_is_given_all_four() {
    let mut h = Harness::new(LANE_ENVELOPE_AND_CONTROL);
    let addr = TreeAddress::Modulator { inst: 0, index: 0 };
    for (row, expected, command) in [
        (1, (0.15, 0.2, 0.5, 0.4), "attack=0.150 decay=0.200 sustain=0.500 release=0.400"),
        (2, (0.15, 0.25, 0.5, 0.4), "attack=0.150 decay=0.250 sustain=0.500 release=0.400"),
        (3, (0.15, 0.25, 0.55, 0.4), "attack=0.150 decay=0.250 sustain=0.550 release=0.400"),
        (4, (0.15, 0.25, 0.55, 0.45), "attack=0.150 decay=0.250 sustain=0.550 release=0.450"),
    ] {
        on_modulator_row(&mut h, addr, row);
        h.press(KeyCode::Right);
        assert_eq!(h.commands(), [format!("SetModulatorEnvelopeParam inst=0 mod=0 {command}")], "row {row}");
        let (a, d, s, r) = stages(lane_source(&h, 0, 0));
        let close = |x: f32, y: f32| (x - y).abs() < 1e-5;
        assert!(close(a, expected.0) && close(d, expected.1) && close(s, expected.2) && close(r, expected.3), "row {row}: {:?}", (a, d, s, r));
        h.press(KeyCode::Esc);
    }
    assert!(h.s.ctl.dirty);
}

#[test]
fn a_value_typed_for_an_envelope_stage_is_clamped_to_its_range() {
    let mut h = Harness::new(LANE_ENVELOPE_AND_CONTROL);
    let addr = TreeAddress::Modulator { inst: 0, index: 0 };
    for (row, text, expected) in [
        (1, "99", (10.0, 0.2, 0.5, 0.4)),
        (1, "0", (0.001, 0.2, 0.5, 0.4)),
        (2, "2.5", (0.001, 2.5, 0.5, 0.4)),
        (3, "5", (0.001, 2.5, 1.0, 0.4)),
        (3, "-5", (0.001, 2.5, 0.0, 0.4)),
        (4, "7", (0.001, 2.5, 0.0, 7.0)),
    ] {
        on_modulator_row(&mut h, addr, row);
        enter_value(&mut h, text);
        let (a, d, s, r) = expected;
        assert_eq!(
            h.commands(),
            [format!("SetModulatorEnvelopeParam inst=0 mod=0 attack={a:.3} decay={d:.3} sustain={s:.3} release={r:.3}")],
            "row {row}: {text}"
        );
        assert_eq!(stages(lane_source(&h, 0, 0)), expected, "row {row}: {text}");
        h.press(KeyCode::Esc);
    }
}

#[test]
fn the_depth_rows_of_an_envelope_and_a_midi_control_are_stepped_and_typed_like_an_lfos() {
    let mut h = Harness::new(LANE_ENVELOPE_AND_CONTROL);
    // The envelope's rows are Type, A, D, S, R, a separator and then the depth; the control's, its source, a separator and the depth.
    on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 0 }, 6);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetModTargetDepth inst=0 mod=0 target=0 depth=0.550"]);
    enter_value(&mut h, "9");
    assert_eq!(h.commands(), ["SetModTargetDepth inst=0 mod=0 target=0 depth=1.000"]);
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 1 }, 2);
    h.press(KeyCode::Left);
    assert_eq!(h.commands(), ["SetModTargetDepth inst=0 mod=1 target=0 depth=0.450"]);
    enter_value(&mut h, "-9");
    assert_eq!(h.commands(), ["SetModTargetDepth inst=0 mod=1 target=0 depth=-1.000"]);
    assert_eq!((lane_depth(&h, 0, 0, 0), lane_depth(&h, 0, 1, 0)), (1.0, -1.0));
}

#[test]
fn the_rows_of_a_midi_control_that_are_not_depths_do_nothing() {
    let mut h = Harness::new(LANE_ENVELOPE_AND_CONTROL);
    for row in [0, 1] {
        on_modulator_row(&mut h, TreeAddress::Modulator { inst: 0, index: 1 }, row);
        h.press(KeyCode::Right).press(KeyCode::Left).press(KeyCode::Enter);
        assert!(h.s.editing.is_none(), "row {row}: there is no number to type");
        assert_eq!(h.commands(), Vec::<String>::new(), "row {row}");
        h.press(KeyCode::Esc);
    }
    assert!(matches!(lane_source(&h, 0, 1), ModSourceSlot::MidiLearn { .. }), "still a control");
    assert_eq!(lane_depth(&h, 0, 1, 0), 0.5);
}

#[test]
fn a_group_lfo_is_stepped_and_typed_with_the_commands_of_the_group() {
    use crate::plugin::chain::LfoWaveform::Triangle;
    let mut h = Harness::new(scene_group());
    let addr = TreeAddress::GroupModulator { group: 0, index: 0 };
    on_modulator_row(&mut h, addr, 2);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetGroupModulatorRate group=0 mod=0 rate=0.550"]);
    enter_value(&mut h, "99");
    assert_eq!(h.commands(), ["SetGroupModulatorRate group=0 mod=0 rate=50.000"]);
    assert_eq!(lfo_rate(group_source(&h, 0, 0)), 50.0);
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, addr, 1);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetGroupModulatorWaveform group=0 mod=0 waveform=Triangle"]);
    assert_eq!(lfo_waveform(group_source(&h, 0, 0)), Triangle);
    h.press(KeyCode::Esc);
    // Two targets, a member's detune and the bus's damping: a depth row each.
    on_modulator_row(&mut h, addr, 4);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetGroupModTargetDepth group=0 mod=0 target=0 depth=0.450"]);
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, addr, 5);
    enter_value(&mut h, "-4");
    assert_eq!(h.commands(), ["SetGroupModTargetDepth group=0 mod=0 target=1 depth=-1.000"]);
    assert!((group_depth(&h, 0, 0, 0) - 0.45).abs() < 1e-5);
    assert_eq!(group_depth(&h, 0, 0, 1), -1.0);
    assert!(h.s.ctl.dirty);
    // (None of it went to a lane.)
    assert_eq!(lfo_rate(lane_source(&h, 2, 0)), 0.2);
}

#[test]
fn a_group_envelope_is_stepped_and_typed_with_the_commands_of_the_group() {
    let mut h = Harness::new(scene_group());
    let addr = TreeAddress::GroupModulator { group: 0, index: 1 };
    on_modulator_row(&mut h, addr, 3);
    h.press(KeyCode::Left);
    assert_eq!(h.commands(), ["SetGroupModulatorEnvelopeParam group=0 mod=1 attack=0.010 decay=0.300 sustain=0.650 release=0.500"]);
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, addr, 1);
    enter_value(&mut h, "99");
    assert_eq!(h.commands(), ["SetGroupModulatorEnvelopeParam group=0 mod=1 attack=10.000 decay=0.300 sustain=0.650 release=0.500"]);
    h.press(KeyCode::Esc);
    // Its only target is the LFO's rate, a modulator's field: a depth row like the others.
    on_modulator_row(&mut h, addr, 6);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetGroupModTargetDepth group=0 mod=1 target=0 depth=0.350"]);
    let (a, d, s, r) = stages(group_source(&h, 0, 1));
    assert_eq!((a, d, r), (10.0, 0.3, 0.5));
    assert!((s - 0.65).abs() < 1e-5);
}

#[test]
fn the_type_row_of_a_group_modulator_switches_it_with_the_commands_of_the_group() {
    let mut h = Harness::new(scene_group());
    on_modulator_row(&mut h, TreeAddress::GroupModulator { group: 0, index: 0 }, 0);
    h.press(KeyCode::Right);
    assert_eq!(
        h.commands(),
        ["SetGroupModulatorSource group=0 mod=0 source=Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }"]
    );
    assert_eq!(stages(group_source(&h, 0, 0)), (0.01, 0.3, 0.7, 0.5));
}

// ---------------------------------------------------------------------------
// What the audio thread and the plugin scan report back
// ---------------------------------------------------------------------------
//
// `Controller::poll` folds the reports into the model and the catalog (its tests
// cover that); `tick` makes the view catch up, which is what these check.

#[test]
fn a_finished_recording_shows_in_the_tree_at_the_tempo_set_with_b() {
    let mut h = Harness::new(scene_split());
    assert!(!h.tree().iter().any(|l| l.contains("Pattern")));
    h.type_chars("b").press_n(KeyCode::Backspace, 3).type_chars("95").press(KeyCode::Enter);
    h.pattern_tx.send(recording(1)).unwrap();
    tick(&mut h.s);

    assert!(h.tree().contains(&"├ ▶ Pattern  2 beats, 1 notes"), "{:#?}", h.tree());
    assert_eq!(h.s.ctl.model.instruments[1].pattern.as_ref().unwrap().bpm, 95.0);
}

#[test]
fn a_finished_recording_marks_the_session_changed() {
    // `r` marks the session when the recording starts, but the events only
    // arrive when it ends, and a save in between (during the count-in, say)
    // clears the mark.
    let mut h = Harness::new(scene_split());
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Char('r'));
    assert!(h.s.ctl.dirty, "`r` marks the session changed");
    h.s.ctl.dirty = false; // what a save does
    h.pattern_tx.send(recording(0)).unwrap();
    tick(&mut h.s);
    assert!(h.s.ctl.dirty, "the recording is part of the session");
}

#[test]
fn a_captured_control_shows_in_the_tree() {
    for (source, label) in [
        (MidiBindSource::Cc(74), "CC 74"),
        (MidiBindSource::PitchBend, "Pitch Bend"),
        (MidiBindSource::Aftertouch, "Aftertouch"),
    ] {
        let mut h = Harness::new(scene_split());
        // Arm a MIDI-learn modulator on lane 0's first parameter the way a user does.
        h.goto(TreeAddress::Instrument(0));
        h.press(KeyCode::Enter).press(KeyCode::Char('l'));
        let index = h.s.ctl.model.instruments[0].modulators.len() - 1;
        assert!(h.tree().iter().any(|l| l.contains("MIDI (learning…)")), "`l` arms a modulator");

        h.learn_tx
            .send(MidiLearnedNotification { location: ModulatorLocation::Lane(0), mod_index: index, source })
            .unwrap();
        tick(&mut h.s);
        assert!(h.tree().iter().any(|l| l.contains(label)), "{label}: {:#?}", h.tree());
        assert!(!h.tree().iter().any(|l| l.contains("MIDI (learning…)")), "{label}: {:#?}", h.tree());
    }
}

#[test]
fn a_captured_control_shows_in_the_tree_for_a_group_modulator() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('l'));
    let index = h.s.ctl.model.groups[0].modulators.len() - 1;

    h.learn_tx
        .send(MidiLearnedNotification {
            location: ModulatorLocation::Group(0),
            mod_index: index,
            source: MidiBindSource::Cc(1),
        })
        .unwrap();
    tick(&mut h.s);
    assert!(h.tree().iter().any(|l| l.contains("CC 1 →")), "{:#?}", h.tree());
}

fn plugin_info(name: &str, is_instrument: bool) -> PluginInfo {
    PluginInfo {
        name: name.to_string(),
        id: format!("builtin:{}", name.to_lowercase()),
        is_instrument,
        param_count: 3,
        preset_count: 0,
        path: String::new(),
        scan_ms: 0,
    }
}

#[test]
fn an_open_selector_gains_rows_as_the_scan_goes_on_and_keeps_its_filter() {
    let mut h = Harness::new(scene_split());
    h.start_scan();
    h.catalog_tx.as_ref().unwrap().send(vec![plugin_info("Reverb", false)]).unwrap();
    tick(&mut h.s);
    h.press(KeyCode::Char('a')).type_chars("rev");
    let sel = h.s.selector.as_ref().expect("`a` opens the effect selector");
    assert_eq!((sel.items.len(), sel.filter.filtered.len()), (1, 1));

    h.catalog_tx
        .as_ref()
        .unwrap()
        .send(vec![plugin_info("Revolver", false), plugin_info("Delay", false), plugin_info("Synth", true)])
        .unwrap();
    tick(&mut h.s);
    let sel = h.s.selector.as_ref().unwrap();
    assert_eq!(sel.items.len(), 3, "the instrument is not an effect");
    assert_eq!(sel.filter.input.value, "rev");
    assert_eq!(sel.filter.filtered.len(), 2, "Reverb and Revolver match, Delay does not");
}

// ---------------------------------------------------------------------------
// Saving
// ---------------------------------------------------------------------------

/// A harness whose session came from `song.toml` in a fresh directory.
fn harness_with_a_file() -> (tempfile::TempDir, std::path::PathBuf, Harness) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("song.toml");
    let mut h = Harness::new(scene_split());
    h.s.ctl.session_path = Some(path.clone());
    (dir, path, h)
}

fn ctrl_shift() -> KeyModifiers {
    KeyModifiers::CONTROL | KeyModifiers::SHIFT
}

#[test]
fn ctrl_s_writes_the_session_to_its_file_and_clears_the_mark() {
    let (_dir, path, mut h) = harness_with_a_file();
    h.s.ctl.piano_filter.set_scale(ScaleSetting::parse("D dorian").unwrap());
    h.s.ctl.piano_filter.set_mode(PianoMode::Locked);
    h.s.ctl.dirty = true;
    h.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL);

    assert!(!h.s.ctl.dirty);
    let config = session::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(config.instruments.len(), 2, "both lanes are in the file");
    let piano = config.piano.expect("the Piano tab's scale and mode are saved with the session");
    assert_eq!((piano.scale.as_deref(), piano.locked), (Some("D dorian"), true));
}

#[test]
fn the_tui_does_not_show_where_a_session_draws_its_nodes_but_keeps_it_through_an_edit_and_a_save() {
    use crate::app::testutil::PLACED;
    let unplaced: String = PLACED
        .lines()
        .filter(|line| !line.starts_with("ui = ") && !line.starts_with("master = ") && *line != "[ui]")
        .map(|line| format!("{line}\n"))
        .collect();
    assert_eq!(Harness::new(PLACED).tree(), Harness::new(&unplaced).tree(), "the tree is the same with the places and without");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("song.toml");
    let mut h = Harness::new(PLACED);
    h.s.ctl.session_path = Some(path.clone());
    // The filter goes, and its place with it; every other node is where the file put it.
    h.goto(TreeAddress::Effect { inst: 0, index: 0 }).press(KeyCode::Char('d'));
    h.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL);

    let text = std::fs::read_to_string(&path).unwrap();
    let config = session::parse(&text).unwrap();
    let at = |x: f32, y: f32| Some(session::NodeUi { x, y });
    assert_eq!(config.master_ui, at(1200.0, 300.5), "{text}");
    assert_eq!(config.groups[0].ui, at(800.0, 40.0), "{text}");
    assert_eq!(config.instruments[0].ui, at(0.0, 0.0), "{text}");
    assert_eq!(config.instruments[0].modulators[0].ui, at(376.0, -200.0), "{text}");
    assert!(config.instruments[0].effects.is_empty(), "{text}");
    assert!(!text.contains("y = 0.25"), "{text}");
}

#[test]
fn ctrl_s_without_a_session_file_leaves_the_mark() {
    let mut h = Harness::new(scene_split());
    assert_eq!(h.s.ctl.session_path, None);
    h.s.ctl.dirty = true;
    h.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(h.s.ctl.dirty, "nothing was saved");
}

#[test]
fn ctrl_shift_s_asks_for_a_name_starting_from_the_current_one() {
    let (_dir, _path, mut h) = harness_with_a_file();
    h.press_with(KeyCode::Char('S'), ctrl_shift());
    let popup = h.s.save_as.as_ref().expect("the popup opens");
    assert_eq!(popup.input.value, "song.toml");
}

#[test]
fn save_as_writes_a_new_file_next_to_the_old_one_and_switches_to_it() {
    let (dir, old, mut h) = harness_with_a_file();
    h.s.ctl.dirty = true;
    h.press_with(KeyCode::Char('S'), ctrl_shift());
    h.press_n(KeyCode::Backspace, 20).type_chars("copy").press(KeyCode::Enter);

    let new = dir.path().join("copy.toml");
    assert!(new.exists(), "`.toml` is appended");
    assert!(!old.exists(), "the old file is not written");
    assert_eq!(h.s.ctl.session_path.as_deref(), Some(new.as_path()), "Ctrl+S now targets the new file");
    assert!(h.s.save_as.is_none(), "the popup closes");
    assert!(!h.s.ctl.dirty);
}

#[test]
fn save_as_keeps_a_toml_extension_and_trims_the_name() {
    let (dir, _old, mut h) = harness_with_a_file();
    h.press_with(KeyCode::Char('S'), ctrl_shift());
    h.press_n(KeyCode::Backspace, 20).type_chars("  mix.toml  ").press(KeyCode::Enter);
    assert!(dir.path().join("mix.toml").exists());
    assert!(h.s.save_as.is_none());
}

#[test]
fn save_as_waits_for_a_name() {
    let (dir, old, mut h) = harness_with_a_file();
    h.s.ctl.dirty = true;
    h.press_with(KeyCode::Char('S'), ctrl_shift());
    h.press_n(KeyCode::Backspace, 20).press(KeyCode::Enter);
    let popup = h.s.save_as.as_ref().expect("the popup stays open");
    assert_eq!(popup.error, None, "no complaint, it just waits");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing was written");
    assert_eq!(h.s.ctl.session_path.as_deref(), Some(old.as_path()));
    assert!(h.s.ctl.dirty);
}

#[test]
fn a_failed_save_as_shows_the_error_and_changes_nothing() {
    let (dir, old, mut h) = harness_with_a_file();
    std::fs::write(dir.path().join("blocker"), "a file, not a directory").unwrap();
    h.s.ctl.dirty = true;
    h.press_with(KeyCode::Char('S'), ctrl_shift());
    h.press_n(KeyCode::Backspace, 20).type_chars("blocker/x").press(KeyCode::Enter);

    let popup = h.s.save_as.as_ref().expect("the popup stays open");
    assert!(popup.error.is_some(), "the error is shown in the popup");
    assert_eq!(h.s.ctl.session_path.as_deref(), Some(old.as_path()), "the old file stays the session's");
    assert!(h.s.ctl.dirty);

    // Editing the name clears the error, so that it can be fixed.
    h.press(KeyCode::Char('y'));
    assert_eq!(h.s.save_as.as_ref().unwrap().error, None, "typing");
    h.press(KeyCode::Enter);
    assert!(h.s.save_as.as_ref().unwrap().error.is_some(), "still a file in the way");
    h.press(KeyCode::Backspace);
    assert_eq!(h.s.save_as.as_ref().unwrap().error, None, "Backspace");
}

#[test]
fn escape_closes_save_as_without_saving() {
    let (dir, _old, mut h) = harness_with_a_file();
    h.press_with(KeyCode::Char('S'), ctrl_shift());
    h.press(KeyCode::Esc);
    assert!(h.s.save_as.is_none());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

// ---------------------------------------------------------------------------
// Node ids
// ---------------------------------------------------------------------------
//
// The harness checks after every key that each node has an id of its own; these
// check that an id stays with its node while others come, go and move.

use crate::app::model::{NodeId, NodePath};

fn node_at(h: &Harness, path: NodePath) -> NodeId {
    h.s.ctl
        .model
        .nodes()
        .find(|(_, p)| *p == path)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("no node at {path:?}"))
}

fn all_nodes(h: &Harness) -> Vec<NodeId> {
    h.s.ctl.model.nodes().map(|(id, _)| id).collect()
}

#[test]
fn an_added_effect_gets_a_new_id_and_the_others_keep_theirs() {
    let mut h = Harness::new(scene_two_effects());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    let before = all_nodes(&h);
    h.press(KeyCode::Char('a')).type_chars("reverb").press(KeyCode::Enter);

    let added = node_at(&h, NodePath::Effect { inst: 0, index: 2 });
    assert!(!before.contains(&added), "the new effect has an id of its own");
    assert!(before.iter().all(|id| all_nodes(&h).contains(id)), "the others keep theirs");
}

#[test]
fn a_new_lane_shares_one_id_between_itself_and_its_oscillator() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    let before = all_nodes(&h);
    h.press(KeyCode::Char('n'));
    let lane = &h.s.ctl.model.instruments[2];
    assert!(!before.contains(&lane.node));
    assert_eq!(lane.instrument.as_ref().unwrap().node, lane.node);

    // Choosing another instrument for it keeps the lane's node.
    h.s.selector = None;
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('i')).type_chars("oscillator").press(KeyCode::Enter);
    let lane = &h.s.ctl.model.instruments[2];
    assert_eq!(lane.instrument.as_ref().unwrap().node, lane.node);
}

#[test]
fn replacing_the_instrument_keeps_the_lanes_id() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    let lane = node_at(&h, NodePath::Instrument(0));
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Char('i')).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        ["RemoveModTarget inst=0 mod=0 target=0", "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false keep"]
    );
    assert_eq!(node_at(&h, NodePath::Instrument(0)), lane);
    assert_eq!(h.s.ctl.model.instruments[0].instrument.as_ref().unwrap().node, lane);
}

#[test]
fn new_modulators_and_groups_get_ids_of_their_own() {
    let mut h = Harness::new(scene_group());
    let before = all_nodes(&h);

    // A lane modulator and a group modulator, with `m` on the chain.
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('m'));
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('m'));
    // A MIDI-learn modulator in each rack, with `l` on a parameter.
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Enter).press(KeyCode::Char('l')).press(KeyCode::Esc);
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('l')).press(KeyCode::Esc);
    // A new group, for the bass.
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('g')).press(KeyCode::Enter);

    let after = all_nodes(&h);
    let added: Vec<_> = after.iter().filter(|id| !before.contains(id)).collect();
    assert_eq!(added.len(), 5, "two `m`, two `l` and a group: {added:?}");
    assert!(before.iter().all(|id| after.contains(id)));
}

#[test]
fn m_on_a_parameter_makes_a_modulator_of_its_own_bound_to_it() {
    // "New LFO" is the first row of the popup. The bass has one lane modulator
    // already; the pad's bus has two group modulators.
    let mut h = Harness::new(scene_group());
    let before = all_nodes(&h);

    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('m')).press(KeyCode::Enter);
    let lane_mods = &h.s.ctl.model.instruments[2].modulators;
    assert_eq!(lane_mods.len(), 2);
    let new = lane_mods.last().unwrap();
    assert!(!before.contains(&new.node), "the lane modulator has an id of its own");
    assert_eq!(new.targets.len(), 1, "it is bound to the parameter it was made from");
    assert!(h.commands()[0].starts_with("InsertModulator inst=2 index=1"));

    h.press(KeyCode::Esc);
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('m')).press(KeyCode::Enter);
    let group_mods = &h.s.ctl.model.groups[0].modulators;
    assert_eq!(group_mods.len(), 3);
    let new = group_mods.last().unwrap();
    assert!(!before.contains(&new.node), "the group modulator has an id of its own");
    assert_eq!(new.targets.len(), 1);
}

#[test]
fn ids_follow_their_nodes_when_effects_move_or_go() {
    let mut h = Harness::new(scene_two_effects());
    let filter = node_at(&h, NodePath::Effect { inst: 0, index: 0 });
    let reverb = node_at(&h, NodePath::Effect { inst: 0, index: 1 });
    let lfo = node_at(&h, NodePath::Modulator { at: ModulatorLocation::Lane(0), index: 0 });

    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(node_at(&h, NodePath::Effect { inst: 0, index: 0 }), reverb, "the reverb moved up");
    assert_eq!(node_at(&h, NodePath::Effect { inst: 0, index: 1 }), filter, "the filter moved down");

    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('d'));
    assert_eq!(node_at(&h, NodePath::Effect { inst: 0, index: 0 }), filter, "the filter is first now");
    assert!(h.s.ctl.model.locate(reverb).is_none(), "the deleted effect's id is gone, and not handed out again");
    assert_eq!(node_at(&h, NodePath::Modulator { at: ModulatorLocation::Lane(0), index: 0 }), lfo);
}

#[test]
fn deleting_a_lane_leaves_the_others_their_ids() {
    let mut h = Harness::new(scene_split());
    let first = node_at(&h, NodePath::Instrument(0));
    let second = node_at(&h, NodePath::Instrument(1));
    h.goto(TreeAddress::Instrument(0));
    // The first `d` empties the lane, the second removes it.
    h.press(KeyCode::Char('d'));
    assert_eq!(node_at(&h, NodePath::Instrument(0)), first, "an emptied lane is still the same node");
    h.press(KeyCode::Char('d'));
    assert_eq!(h.s.ctl.model.instruments.len(), 1);
    assert_eq!(node_at(&h, NodePath::Instrument(0)), second, "the survivor is lane 0 now, with its old id");
    assert!(h.s.ctl.model.locate(first).is_none());
}

// ---------------------------------------------------------------------------
// Presets
// ---------------------------------------------------------------------------

/// An oscillator (which has no presets) and a reverb on "Vintage Plate".
const PRESET_SCENE: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
preset = "Vintage Plate"
"#;

const REVERB_PRESETS: [(&str, &str); 5] = [
    ("arcadia", "Arcadia Dream Hall"),
    ("cathedral", "Cathedral"),
    ("plate", "Vintage Plate"),
    ("room", "Small Room"),
    ("ambient", "Ambient Wash"),
];

/// The value of each of the reverb's parameters, by its own index, once `preset`
/// is loaded: what the model has to mirror.
fn reverb_values_with(preset: &str) -> Vec<(u32, f32)> {
    let mut reverb = plugin::load("builtin:reverb", SAMPLE_RATE, BLOCK, &plugin::Runtime::default()).unwrap();
    session::apply_preset(&mut reverb, preset);
    let indexes: Vec<u32> = reverb.parameters().iter().map(|p| p.index).collect();
    indexes.into_iter().map(|i| (i, reverb.get_parameter(i).unwrap())).collect()
}

#[test]
fn p_lists_the_plugins_presets_and_starts_on_the_current_one() {
    let mut h = Harness::new(PRESET_SCENE);
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('p'));
    let ps = h.s.preset_selector.as_ref().expect("the reverb has presets");
    let listed: Vec<(&str, &str)> = ps.presets.iter().map(|p| (p.id.as_str(), p.name.as_str())).collect();
    assert_eq!(listed, REVERB_PRESETS);
    assert_eq!((ps.inst, ps.slot), (0, 1), "effect 0 is slot 1 for the audio thread");
    assert_eq!(ps.filter.selected_item(&ps.items).map(|item| item.index), Some(2), "Vintage Plate, the current one");
}

#[test]
fn p_opens_only_on_a_lane_plugin_that_has_presets() {
    let mut h = Harness::new(PRESET_SCENE);
    h.goto(TreeAddress::Instrument(0)); // the oscillator has none
    h.press(KeyCode::Char('p'));
    assert!(h.s.preset_selector.is_none());

    let mut h = Harness::new(scene_bus_pair());
    for addr in [TreeAddress::Group(0), TreeAddress::GroupEffect { group: 0, index: 1 }, TreeAddress::GroupModulator { group: 0, index: 0 }] {
        h.goto(addr);
        h.press(KeyCode::Char('p'));
        assert!(h.s.preset_selector.is_none(), "{addr:?}");
    }
}

#[test]
fn choosing_a_preset_loads_it_and_mirrors_its_values_into_the_model() {
    let mut h = Harness::new(PRESET_SCENE);
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('p')).type_chars("cathed").press(KeyCode::Enter);

    assert_eq!(h.commands(), ["LoadPreset inst=0 slot=1 id=cathedral"]);
    assert!(h.s.preset_selector.is_none(), "the popup closes");
    assert!(h.s.ctl.dirty);
    let reverb = &h.s.ctl.model.instruments[0].effects[0];
    assert_eq!(reverb.current_preset.as_deref(), Some("Cathedral"));
    let expected = reverb_values_with("Cathedral");
    assert_eq!(reverb.params.len(), expected.len());
    for row in &reverb.params {
        let want = expected.iter().find(|(i, _)| *i == row.index).unwrap().1;
        assert_eq!((row.value, row.default), (want, want), "{}: the preset's value, and the baseline", row.name);
    }
}

#[test]
fn the_arrows_choose_among_the_presets() {
    let mut h = Harness::new(PRESET_SCENE); // starts on Vintage Plate, the third
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('p')).press(KeyCode::Down).press(KeyCode::Enter);
    assert_eq!(h.commands(), ["LoadPreset inst=0 slot=1 id=room"]);
    h.press(KeyCode::Char('p')).press(KeyCode::Up).press(KeyCode::Up).press(KeyCode::Enter);
    assert_eq!(h.commands(), ["LoadPreset inst=0 slot=1 id=cathedral"]);
}

#[test]
fn a_loaded_preset_is_saved_by_name_with_no_parameter_overrides() {
    let mut h = Harness::new(PRESET_SCENE);
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('p')).type_chars("ambient").press(KeyCode::Enter);
    let (lanes, _) = crate::app::save::to_save(&h.s.ctl.model);
    let reverb = &lanes[0].effects[0];
    assert_eq!(reverb.preset.as_deref(), Some("Ambient Wash"));
    assert!(reverb.params.is_empty(), "the preset's values are the baseline, not edits: {:?}", reverb.params);

    // A tweak afterwards is an edit on top of the preset, and the only one.
    h.press(KeyCode::Enter).press(KeyCode::Right);
    h.commands();
    let (lanes, _) = crate::app::save::to_save(&h.s.ctl.model);
    assert_eq!(lanes[0].effects[0].params.len(), 1);
}

#[test]
fn escape_and_a_filter_with_no_match_load_nothing() {
    let mut h = Harness::new(PRESET_SCENE);
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('p')).press(KeyCode::Down).press(KeyCode::Esc);
    assert!(h.s.preset_selector.is_none());
    assert_eq!(h.commands(), Vec::<String>::new());
    assert!(!h.s.ctl.dirty);

    h.press(KeyCode::Char('p')).type_chars("zzz").press(KeyCode::Enter);
    assert!(h.s.preset_selector.is_none(), "the popup closes");
    assert_eq!(h.commands(), Vec::<String>::new());
    assert!(!h.s.ctl.dirty);
    assert_eq!(h.s.ctl.model.instruments[0].effects[0].current_preset.as_deref(), Some("Vintage Plate"));
}

#[test]
fn groups_without_a_name_are_called_group_and_their_number() {
    // Three groups, none named: each place that names a group counts from one by
    // the group's own position.
    let mut h = Harness::new(
        "[[group]]\n[[group]]\n[[group]]\n\
         [[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n\
         [[instrument]]\nplugin = \"builtin:osc\"\ngroup = 1\n\
         [[instrument]]\nplugin = \"builtin:osc\"\ngroup = 2\n",
    );
    let headers: Vec<&str> = h.tree().into_iter().filter(|l| l.starts_with('▦')).collect();
    assert_eq!(headers, ["▦ Group 1", "▦ Group 2", "▦ Group 3"], "the tree");

    // The parameter pane is titled by the group under the cursor.
    h.goto(TreeAddress::Group(2));
    let screen = h.dump();
    assert!(screen.lines().any(|l| l.contains("┐┌ Group 3 ")), "the parameter pane's title: {screen}");

    // The group popup lists them all, and marks the one the lane is in.
    h.goto(TreeAddress::Instrument(1));
    h.press(KeyCode::Char('g'));
    let screen = h.dump();
    for name in ["Group 1", "Group 2 (current)", "Group 3"] {
        assert!(screen.contains(name), "the group popup lacks {name}: {screen}");
    }
}

// ---------------------------------------------------------------------------
// The loop: what wakes it, and what it does
// ---------------------------------------------------------------------------

fn terminal_key(code: KeyCode, modifiers: KeyModifiers) -> io::Result<Event> {
    Ok(Event::Key(KeyEvent::new(code, modifiers)))
}

fn terminal_quit() -> io::Result<Event> {
    terminal_key(KeyCode::Char('q'), KeyModifiers::CONTROL)
}

#[test]
fn the_loop_handles_what_the_terminal_sends_in_order_draws_it_and_stops_on_quit() {
    let mut h = Harness::new(scene_split());
    let (tx, rx) = unbounded();
    tx.send(terminal_key(KeyCode::Down, KeyModifiers::NONE)).unwrap();
    tx.send(terminal_key(KeyCode::Down, KeyModifiers::NONE)).unwrap();
    tx.send(terminal_quit()).unwrap();

    event_loop(&mut h.terminal, &mut h.s, &rx, None).unwrap();

    assert!(h.s.quit);
    assert_eq!(h.s.chain_state.selected, 2, "both keys were handled");
    let screen = dump_buffer(h.terminal.backend().buffer());
    assert!(screen.contains("Oscillator"), "the last frame drawn shows the session: {screen}");
}

#[test]
fn the_loop_wakes_by_itself_to_fold_in_what_the_audio_thread_reports() {
    let mut h = Harness::new(scene_split());
    let (tx, rx) = unbounded();
    assert!(h.s.ctl.model.instruments[0].pattern.is_none());
    // The audio thread reports a recording while the loop waits for a key that does not
    // come. The loop is told to quit once it has taken the report, or if it has not within
    // three seconds: the last pass of a loop that is quitting would take it too, so only
    // that it was taken *before* the quit shows that the loop wakes by itself.
    let reported = h.pattern_tx.clone();
    let reporter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        reported.send(recording(0)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !reported.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let taken = reported.is_empty();
        tx.send(terminal_quit()).unwrap();
        taken
    });

    event_loop(&mut h.terminal, &mut h.s, &rx, None).unwrap();
    assert!(reporter.join().unwrap(), "the loop did not take the report while it waited, with no key pressed");

    assert!(h.s.ctl.model.instruments[0].pattern.is_some(), "the recording was folded in");
    assert!(h.tree().iter().any(|l| l.contains('▶')), "and the view caught up: {:?}", h.tree());
}

#[test]
fn a_loop_whose_terminal_has_gone_ends_with_an_error() {
    let mut h = Harness::new(scene_split());
    let (tx, rx) = unbounded::<io::Result<Event>>();
    drop(tx);
    let error = event_loop(&mut h.terminal, &mut h.s, &rx, None).unwrap_err();
    assert!(error.to_string().contains("terminal"), "{error}");
}

#[test]
fn a_failure_of_the_terminal_ends_the_loop_with_it() {
    let mut h = Harness::new(scene_split());
    let (tx, rx) = unbounded();
    tx.send(Err(io::Error::new(io::ErrorKind::BrokenPipe, "the terminal went"))).unwrap();
    tx.send(terminal_quit()).unwrap();
    let error = event_loop(&mut h.terminal, &mut h.s, &rx, None).unwrap_err();
    assert_eq!((error.kind(), error.to_string().as_str()), (io::ErrorKind::BrokenPipe, "the terminal went"));
}

#[test]
fn a_failure_that_comes_behind_other_events_is_not_lost() {
    let mut h = Harness::new(scene_split());
    let (tx, rx) = unbounded();
    tx.send(terminal_key(KeyCode::Down, KeyModifiers::NONE)).unwrap();
    tx.send(Err(io::Error::new(io::ErrorKind::BrokenPipe, "the terminal went"))).unwrap();
    // (A quit after it, so that a loop that carries on ends the test instead of hanging it.)
    tx.send(terminal_quit()).unwrap();
    let error = event_loop(&mut h.terminal, &mut h.s, &rx, None).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(h.s.chain_state.selected, 1, "what came before it was handled");
    assert!(!h.s.quit, "and what came after it was not");
}

#[test]
fn the_events_that_are_waiting_are_all_handled_before_the_next_frame_is_drawn() {
    // A paste, or a key held down: a frame for each key would fall behind.
    let mut h = Harness::new(scene_split());
    let (tx, rx) = unbounded();
    for _ in 0..40 {
        tx.send(terminal_key(KeyCode::Down, KeyModifiers::NONE)).unwrap();
    }
    tx.send(terminal_quit()).unwrap();

    event_loop(&mut h.terminal, &mut h.s, &rx, None).unwrap();

    // (The cursor wraps round the end of the tree.)
    assert_eq!(h.s.chain_state.selected, 40 % h.s.tree_entries.len(), "every key was handled");
    // (One frame before the keys and one after them; a few more if the machine stalled long enough for the tick to come.)
    let frames = h.terminal.get_frame().count();
    assert!(frames <= 4, "{frames} frames for 41 events");
}

// ---------------------------------------------------------------------------
// The web interface in the TUI
// ---------------------------------------------------------------------------

fn web_status(address: &str, clients: usize) -> WebStatus {
    WebStatus { address: address.parse().unwrap(), url: format!("http://{address}/#token=abc"), clients }
}

/// The first row of the screen: the tab bar, and the status at its right. (The dump
/// starts with the size of the screen and a heading.)
fn tab_bar(h: &mut Harness) -> String {
    h.dump().lines().nth(2).unwrap().to_string()
}

#[test]
fn the_tab_bar_says_where_the_web_interface_is_and_how_many_browsers_are_on_it() {
    let mut h = Harness::new(scene_split());
    assert!(!tab_bar(&mut h).contains("web"), "not unless it is served");
    assert!(tab_bar(&mut h).ends_with("120 BPM"));

    h.s.web = Some(web_status("127.0.0.1:8765", 0));
    let bar = tab_bar(&mut h);
    assert!(bar.ends_with("web 127.0.0.1:8765  120 BPM"), "{bar}");
    h.s.web.as_mut().unwrap().clients = 2;
    let bar = tab_bar(&mut h);
    assert!(bar.ends_with("web 127.0.0.1:8765 (2)  120 BPM"), "{bar}");
}

#[test]
fn the_web_label_comes_before_the_clip_light() {
    let mut h = Harness::new(scene_split());
    h.s.web = Some(web_status("127.0.0.1:8765", 1));
    h.s.clip = clip::ClipLight::new(0);
    h.s.clip.update(1, Instant::now());
    assert!(h.s.clip.is_lit());
    let bar = tab_bar(&mut h);
    assert!(bar.ends_with("web 127.0.0.1:8765 (1)  CLIP  120 BPM"), "{bar}");
}

#[test]
fn the_web_label_gives_way_before_the_tabs_the_clip_light_and_the_tempo_do() {
    let mut h = Harness::new(scene_split());
    h.s.web = Some(web_status("127.0.0.1:8765", 3));
    let tabs = "(1) Session │ (2) Piano │ (3) Scope │ (4) Help";
    for lit in [false, true] {
        h.s.clip = clip::ClipLight::new(0);
        if lit {
            h.s.clip.update(1, Instant::now());
        }
        assert_eq!(h.s.clip.is_lit(), lit);
        let tail = if lit { "CLIP  120 BPM" } else { "120 BPM" };
        let mut shown = Vec::new();
        for width in 60..=130u16 {
            h.terminal = Terminal::new(TestBackend::new(width, HEIGHT)).unwrap();
            let bar = tab_bar(&mut h);
            let context = format!("{width} columns, clip light {}: {bar}", if lit { "on" } else { "off" });
            assert!(bar.starts_with(tabs), "the tabs are whole: {context}");
            assert!(bar.ends_with(tail), "the clip light and the tempo are there: {context}");
            shown.push(if bar.contains("web 127.0.0.1:8765 (3)") {
                "full"
            } else if bar.contains("web (3)") {
                "short"
            } else {
                assert!(!bar.contains("web"), "{context}");
                "none"
            });
            if shown.last() != Some(&"none") {
                assert!(bar.starts_with(&format!("{tabs}  ")), "two blanks between the tabs and the label: {context}");
                assert!(bar.ends_with(&format!("  {tail}")), "and two between the label and the rest: {context}");
            }
        }
        // The narrower the terminal, the less of it: all, the short one, none; each in one stretch of widths.
        let mut order = shown.clone();
        order.dedup();
        assert_eq!(order, ["none", "short", "full"], "clip light {lit}: {shown:?}");
    }
}

#[test]
fn the_help_tab_warns_about_an_address_beyond_this_machine_and_only_then() {
    let mut h = Harness::new(scene_split());
    h.s.set_web(web_status("127.0.0.1:8765", 0));
    assert!(!h.s.help_lines.iter().any(|l| l.contains("beyond this machine")), "{:?}", h.s.help_lines);

    h.s.set_web(web_status("0.0.0.0:8765", 0));
    assert!(h.s.help_lines.iter().any(|l| l.contains("beyond this machine")), "{:?}", h.s.help_lines);
    assert!(h.s.help_lines.iter().any(|l| l.contains("rather than 0.0.0.0")), "{:?}", h.s.help_lines);
    h.press(KeyCode::Char('4'));
    let screen = h.dump();
    assert!(screen.contains("beyond this machine"), "{screen}");
}

#[test]
fn the_help_tab_says_where_the_web_interface_is_when_there_is_one() {
    let mut h = Harness::new(scene_split());
    assert!(!h.s.help_lines.iter().any(|l| l.to_lowercase().contains("web")), "not unless it is served");

    h.s.set_web(web_status("127.0.0.1:8765", 0));
    assert!(h.s.help_lines.iter().any(|l| l.contains("http://127.0.0.1:8765/#token=abc")), "{:?}", h.s.help_lines);
    h.press(KeyCode::Char('4'));
    let screen = h.dump();
    assert!(screen.contains("Web interface"), "{screen}");
    assert!(screen.contains("http://127.0.0.1:8765/#token=abc"), "the address to open, with the token if it has one: {screen}");
}

#[cfg(feature = "web")]
mod with_web {
    //! The TUI loop and a browser on one controller.

    use std::future::Future;
    use std::net::SocketAddr;
    use std::time::Duration;

    use crossbeam_channel::Sender;
    use serde_json::{Value, json};
    use tokio::sync::oneshot;

    use super::*;
    use crate::cli::WebArgs;
    use crate::tui::web::Web;
    use crate::web::testclient::{self, said, tell};

    const TRACER: &str = include_str!("../../web/src/client/fixtures/tracer.toml");

    fn key(code: KeyCode) -> io::Result<Event> {
        terminal_key(code, KeyModifiers::NONE)
    }

    fn start(h: &mut Harness) -> Web {
        Web::start(&mut h.s.ctl, &WebArgs { web_bind: "127.0.0.1:0".parse().unwrap(), web_origins: vec![] }).expect("the web interface starts")
    }

    /// What a browser's side of a test gets: where to connect, the terminal (to press
    /// keys in the TUI as a user would: they go to the loop as the terminal's events),
    /// and a signal that comes when the test has looked at the TUI and the browser may
    /// go.
    struct Browser {
        addr: SocketAddr,
        keys: Sender<io::Result<Event>>,
        done: oneshot::Receiver<()>,
    }

    impl Browser {
        /// Connect, subscribe, and take the snapshot (and the catalog that comes after it).
        async fn join(&self) -> (testclient::Client, Value) {
            let mut client = testclient::connect(self.addr).await;
            assert_eq!(said(&mut client).await["type"], "hello");
            let snapshot = testclient::subscribe(&mut client).await;
            (client, snapshot)
        }

        /// End the loop, and keep the connection open until the test is done with the screen.
        async fn quit(self) {
            self.keys.send(terminal_quit()).unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(10), self.done).await;
        }
    }

    /// Run the loop of `h` with the web interface up, and `script` as a browser on its
    /// own thread. The loop ends when the script quits (or fails, which is reported
    /// when `after` has had its look at the screen).
    fn run<F, Fut>(h: &mut Harness, script: F, after: impl FnOnce(&mut Harness, &Web))
    where
        F: FnOnce(Browser) -> Fut + Send + 'static,
        Fut: Future<Output = ()>,
    {
        let mut web = start(h);
        let addr = web.status().address;
        let (tx, rx) = unbounded();
        let (done_tx, done) = oneshot::channel();
        let keys = tx.clone();
        let browser = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let quit = keys.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(script(Browser { addr, keys, done }))));
            // A browser that failed ends the loop too, rather than leave the test waiting for a quit.
            let _ = quit.send(terminal_quit());
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
        });

        let looped = event_loop(&mut h.terminal, &mut h.s, &rx, Some(&mut web));
        // (The browser's side is still connected: it waits for this.)
        after(h, &web);
        let _ = done_tx.send(());
        browser.join().expect("the browser's side passed");
        looped.unwrap();
    }

    fn node<'a>(snapshot: &'a Value, name: &str) -> &'a Value {
        snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["name"] == name).unwrap_or_else(|| panic!("no node {name}"))
    }

    #[test]
    fn both_front_ends_share_one_controller() {
        let mut h = Harness::new(TRACER);
        // (The lane that `n` adds is by an intent, which loads from the catalog.)
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        // The TUI's cursor is on the filter.
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });

        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let (filter, cutoff) = (node(&snapshot, "Filter")["id"].as_u64().unwrap(), node(&snapshot, "Filter")["plugin"]["params"][0]["index"].as_u64().unwrap());
                assert_eq!(snapshot["snapshot"]["rev"], 0);

                // The browser turns the cutoff up; the TUI is looking at the filter.
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setParam", "node": filter, "param": cutoff, "value": 0.9}})).await;
                assert_eq!(said(&mut client).await["event"], json!({"rev": 1, "type": "paramSet", "node": filter, "param": cutoff, "value": 0.9}));
                assert_eq!(said(&mut client).await, json!({"type": "history", "undo": "set cutoff", "redo": null}));
                assert_eq!(said(&mut client).await, json!({"type": "ack", "reqId": 1, "rev": 1}));

                // The TUI turns it down a step (Enter takes the parameters' pane; the cutoff is the first). The
                // step starts from where the browser left it, and the browser hears it.
                browser.keys.send(key(KeyCode::Enter)).unwrap();
                browser.keys.send(key(KeyCode::Left)).unwrap();
                let event = said(&mut client).await;
                assert_eq!(event["type"], "event", "{event}");
                let (rev, value) = (event["event"]["rev"].as_u64().unwrap(), event["event"]["value"].as_f64().unwrap());
                assert_eq!((event["event"]["type"].clone(), event["event"]["node"].clone(), rev), (json!("paramSet"), json!(filter), 2));
                assert!(value < 0.9 && value > 0.8, "a step down from what the browser set, not from what the TUI had: {value}");

                // A browser that comes now is told where the session is, though no snapshot has been sent since:
                // the loop keeps it current (every 100 ms or so).
                tokio::time::sleep(Duration::from_millis(350)).await;
                let mut late = testclient::connect(browser.addr).await;
                assert_eq!(said(&mut late).await["rev"], 2, "hello");
                drop(late);

                // The TUI sets the tempo with `b`, which no intent does: the browser is given a snapshot.
                browser.keys.send(key(KeyCode::Esc)).unwrap();
                browser.keys.send(key(KeyCode::Char('b'))).unwrap();
                // (The popup starts with 120, which goes.)
                for _ in 0..3 {
                    browser.keys.send(key(KeyCode::Backspace)).unwrap();
                }
                for c in ['9', '0'] {
                    browser.keys.send(key(KeyCode::Char(c))).unwrap();
                }
                browser.keys.send(key(KeyCode::Enter)).unwrap();
                let told = said(&mut client).await;
                assert_eq!(told["type"], "snapshot", "{told}");
                assert_eq!((told["snapshot"]["rev"].clone(), told["snapshot"]["meta"]["bpm"].clone()), (json!(3), json!(90.0)));
                assert_eq!(told["snapshot"]["meta"]["dirty"], true);
                let seen = told["snapshot"]["graph"]["nodes"].as_array().unwrap().len();

                // And adds a lane with `n` (and leaves the oscillator it comes with): an intent, told as a snapshot at the revision after it.
                browser.keys.send(key(KeyCode::Char('n'))).unwrap();
                browser.keys.send(key(KeyCode::Esc)).unwrap();
                let told = said(&mut client).await;
                assert_eq!(told["type"], "snapshot", "{told}");
                assert_eq!(told["snapshot"]["rev"], 5);
                assert_eq!(told["snapshot"]["graph"]["nodes"].as_array().unwrap().len(), seen + 1, "a lane");
                // (What was told before is in it: the cutoff the TUI set.)
                assert_eq!(node(&told, "Filter")["plugin"]["params"][0]["value"].as_f64().unwrap(), value);

                browser.quit().await;
            },
            |h, web| {
                // What the browser set and the TUI changed after it, in the model and so on the screen.
                let cutoff = h.s.ctl.model.instruments[0].effects[0].params[0].value;
                assert!(cutoff < 0.9 && cutoff > 0.8, "{cutoff}");
                assert_eq!(h.s.ctl.global_bpm, 90.0);
                assert_eq!(h.s.ctl.model.instruments.len(), 2, "the lane");
                assert_eq!(h.s.ctl.rev, 5, "an intent from the browser, two from the TUI (the lane is told with a snapshot after it), and a snapshot for the tempo");
                let bar = tab_bar(h);
                assert!(bar.contains(&format!("web {} (1)", web.status().address)), "the browser is counted: {bar}");
            },
        );
    }

    #[test]
    fn a_group_volume_set_in_a_browser_shows_in_the_trees_group_header() {
        let mut h = Harness::new(scene_group());
        assert!(h.tree().contains(&"▦ Pad  vol 0.80"), "{:#?}", h.tree());
        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let group = node(&snapshot, "Pad")["id"].as_u64().unwrap();
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setGain", "node": group, "gain": "groupVolume", "value": 2.5}})).await;
                assert_eq!(said(&mut client).await["type"], "event");
                assert_eq!(said(&mut client).await["type"], "history");
                assert_eq!(said(&mut client).await["type"], "ack");
                browser.quit().await;
            },
            |h, _| {
                assert_eq!(h.s.ctl.model.groups[0].volume, 2.5);
                assert!(h.tree().contains(&"▦ Pad  vol 2.50"), "the header follows what the browser set: {:#?}", h.tree());
            },
        );
    }

    #[test]
    fn a_range_set_in_a_browser_shows_on_the_lane_in_the_tree() {
        let mut h = Harness::new("[[instrument]]\nplugin = \"builtin:osc\"\n");
        assert!(h.tree().iter().all(|line| !line.contains("C3-B4")), "{:#?}", h.tree());
        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let lane = node(&snapshot, "Oscillator")["id"].as_u64().unwrap();
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setRange", "node": lane, "range": {"low": 48, "high": 71}}})).await;
                let told = said(&mut client).await;
                assert_eq!((told["type"].clone(), told["event"]["type"].clone()), (json!("event"), json!("rangeSet")), "{told}");
                assert_eq!(said(&mut client).await["type"], "history");
                assert_eq!(said(&mut client).await["type"], "ack");
                browser.quit().await;
            },
            |h, _| {
                assert_eq!(h.s.ctl.model.instruments[0].range, Some((48, 71)));
                assert!(h.tree().iter().any(|line| line.contains("Oscillator") && line.contains("C3-B4")), "the lane's label follows what the browser set: {:#?}", h.tree());
            },
        );
    }

    #[test]
    fn a_preset_loaded_in_a_browser_changes_the_parameters_the_tui_shows() {
        let mut h = Harness::new("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n");
        let before: Vec<f32> = h.s.ctl.model.instruments[0].effects[0].params.iter().map(|p| p.value).collect();
        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let reverb = node(&snapshot, "Reverb")["id"].as_u64().unwrap();
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "loadPreset", "node": reverb, "preset": "cathedral"}})).await;
                let event = said(&mut client).await;
                assert_eq!(event["event"]["type"], "presetLoaded");
                // A step of its own, in the words of the preset.
                assert_eq!(said(&mut client).await, json!({"type": "history", "undo": "load preset Cathedral", "redo": null}));
                assert_eq!(said(&mut client).await["type"], "ack");
                browser.quit().await;
            },
            |h, _| {
                let slot = &h.s.ctl.model.instruments[0].effects[0];
                assert_eq!(slot.current_preset.as_deref(), Some("Cathedral"));
                let after: Vec<f32> = slot.params.iter().map(|p| p.value).collect();
                assert_ne!(before, after, "the preset's values are in what the TUI shows");
            },
        );
    }

    #[test]
    fn a_cable_a_browser_takes_out_is_gone_from_the_tui_at_once_and_the_browser_is_given_the_new_structure() {
        let mut h = Harness::new(TRACER);
        assert!(h.tree().iter().any(|line| line.contains('→')), "the LFO drives the cutoff: {:#?}", h.tree());
        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let edges = snapshot["snapshot"]["graph"]["edges"].as_array().unwrap();
                let cable = edges.iter().find(|e| e["kind"] == "modulation").unwrap()["id"].as_u64().unwrap();
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "removeEdge", "edge": cable}})).await;
                // The structure is told as a snapshot, at the revision after the intent's, and the answer follows it.
                let told = said(&mut client).await;
                assert_eq!((told["type"].clone(), told["snapshot"]["rev"].clone()), (json!("snapshot"), json!(2)), "{told}");
                assert!(told["snapshot"]["graph"]["edges"].as_array().unwrap().iter().all(|e| e["kind"] != "modulation"));
                // What can be undone now (the removal is a step of the history, which puts the cable back) is told before the answer.
                assert_eq!(said(&mut client).await, json!({"type": "history", "undo": "remove a cable", "redo": null}));
                assert_eq!(said(&mut client).await, json!({"type": "ack", "reqId": 1, "rev": 1}));
                browser.quit().await;
            },
            |h, _| {
                assert!(h.s.ctl.model.instruments[0].modulators[0].targets.is_empty());
                assert!(h.tree().iter().all(|line| !line.contains('→')), "the tree is built again: {:#?}", h.tree());
            },
        );
    }

    #[test]
    fn an_effect_a_browser_adds_is_in_the_tuis_tree_at_once_and_the_browser_is_given_the_new_structure() {
        let mut h = Harness::new(TRACER);
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        assert!(!h.tree().iter().any(|line| line.contains("fx Reverb")));
        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let oscillator = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["name"] == "Oscillator").unwrap()["id"].clone();
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addEffect", "parent": oscillator, "plugin": "builtin:reverb"}})).await;
                // The structure is told as a snapshot, at the revision after the intent's, and the answer follows it.
                let told = said(&mut client).await;
                assert_eq!((told["type"].clone(), told["snapshot"]["rev"].clone()), (json!("snapshot"), json!(2)), "{told}");
                let names: Vec<_> = told["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap().to_string()).collect();
                assert!(names.contains(&"Reverb".to_string()), "{names:?}");
                // What can be undone now (the add is a step of the history) is told before the answer.
                assert_eq!(said(&mut client).await, json!({"type": "history", "undo": "add Reverb", "redo": null}));
                assert_eq!(said(&mut client).await, json!({"type": "ack", "reqId": 1, "rev": 1}));
                // A plugin that is not in the catalog is refused, with the words of the controller.
                tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "addEffect", "parent": oscillator, "plugin": "./evil.clap"}})).await;
                let refused = said(&mut client).await;
                assert_eq!((refused["type"].clone(), refused["error"]["code"].clone()), (json!("rejected"), json!("invalid")), "{refused}");
                assert!(refused["error"]["message"].as_str().unwrap().contains("not a plugin that is installed"), "{refused}");
                browser.quit().await;
            },
            |h, _| {
                assert_eq!(h.s.ctl.model.instruments[0].effects.len(), 2, "the filter, and the reverb the browser added");
                assert!(h.tree().iter().any(|line| line.contains("fx Reverb")), "the tree is built again: {:#?}", h.tree());
            },
        );
    }

    #[test]
    fn an_instrument_a_browser_puts_in_is_in_the_tuis_tree_at_once_and_the_browser_is_given_the_new_structure() {
        let mut h = Harness::new(TRACER);
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        assert_eq!(h.s.ctl.model.instruments.len(), 1);
        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let oscillator = node(&snapshot, "Oscillator")["id"].clone();
                // A lane of its own: told as a snapshot, at the revision after the intent's, and the answer follows it.
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addInstrument", "plugin": "builtin:osc"}})).await;
                let told = said(&mut client).await;
                assert_eq!((told["type"].clone(), told["snapshot"]["rev"].clone()), (json!("snapshot"), json!(2)), "{told}");
                let lanes = told["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().filter(|n| n["kind"] == "instrument").count();
                assert_eq!(lanes, 2, "{told}");
                // Its meters (a peak and a clip count) are in the layout of the telemetry that the snapshot has.
                let meters = |snapshot: &Value| snapshot["snapshot"]["telemetry"]["meters"].as_array().unwrap().len();
                assert_eq!(meters(&told), meters(&snapshot) + 2, "{told}");
                assert_eq!(said(&mut client).await, json!({"type": "history", "undo": "add Oscillator as an instrument", "redo": null}));
                assert_eq!(said(&mut client).await, json!({"type": "ack", "reqId": 1, "rev": 1}));
                // The lane there was gets another instrument: a step of its own, in the words of the button that puts one in.
                tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "replaceInstrument", "node": oscillator, "plugin": "builtin:osc"}})).await;
                let told = said(&mut client).await;
                assert_eq!((told["type"].clone(), told["snapshot"]["rev"].clone()), (json!("snapshot"), json!(4)), "{told}");
                assert_eq!(said(&mut client).await, json!({"type": "history", "undo": "change the instrument to Oscillator", "redo": null}));
                assert_eq!(said(&mut client).await, json!({"type": "ack", "reqId": 2, "rev": 3}));
                // An effect is not an instrument, and the controller's words say so.
                tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "replaceInstrument", "node": oscillator, "plugin": "builtin:reverb"}})).await;
                let refused = said(&mut client).await;
                assert_eq!((refused["type"].clone(), refused["error"]["code"].clone()), (json!("rejected"), json!("invalid")), "{refused}");
                assert_eq!(refused["error"]["message"], "Reverb is an effect, not an instrument", "{refused}");
                browser.quit().await;
            },
            |h, _| {
                assert_eq!(h.s.ctl.model.instruments.len(), 2, "the lane the browser added");
                assert_eq!(h.tree().iter().filter(|line| line.contains("♪ Oscillator")).count(), 2, "{:#?}", h.tree());
            },
        );
    }

    #[test]
    fn a_server_that_has_stopped_does_not_make_the_loop_spin() {
        let mut h = Harness::new(TRACER);
        let mut web = start(&mut h);
        // Nothing can send a request any more, and the loop is told so by the channel.
        web.stop_server();
        let (tx, rx) = unbounded();
        let quitter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            tx.send(terminal_quit()).unwrap();
        });
        event_loop(&mut h.terminal, &mut h.s, &rx, Some(&mut web)).unwrap();
        quitter.join().unwrap();
        let frames = h.terminal.get_frame().count();
        assert!(frames < 40, "{frames} frames in half a second: the loop spins on a channel that has nothing more to give");
    }

    #[test]
    fn the_web_interface_that_is_up_is_in_the_tab_bar_from_the_first_frame() {
        let mut h = Harness::new(TRACER);
        let mut web = start(&mut h);
        let (tx, rx) = unbounded();
        tx.send(terminal_quit()).unwrap();
        event_loop(&mut h.terminal, &mut h.s, &rx, Some(&mut web)).unwrap();
        let bar = tab_bar(&mut h);
        assert!(bar.contains(&format!("web {}", web.status().address)), "{bar}");
        assert!(!bar.contains("web 127.0.0.1:0"), "the address it listens on, not the one it was asked for: {bar}");
    }
}

// -- A browser changes the structure under the TUI's cursor and popups --
//
// The tree is addressed by position, and a browser can add or take out a node above what the TUI is on:
// the cursor has to stay on the node it was on (or `d` takes out what it was never on), and a popup that
// holds a place in the tree (a modulator's, a lane's, a group's) is not for what is there now.

mod browser_changes_structure {
    use super::*;
    use crate::app::graph::{Endpoint, Port};
    use crate::app::intent::{Intent, ModulatorKind};
    use crate::app::model::ModSourceSlot;

    const TWO_LANES: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
"#;

    const THREE_LFOS: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator]]
rate = 3.0
"#;

    /// What a browser does: the intent is applied by the controller, and the loop of the TUI reacts to it.
    fn browser(h: &mut Harness, intent: Intent) {
        let applied = h.s.ctl.apply(intent).unwrap();
        h.s.react_to(&applied);
    }

    fn cursor_node(h: &Harness) -> NodeId {
        h.s.tree_entries[h.s.chain_state.selected].node
    }

    fn rates(h: &Harness) -> Vec<f32> {
        h.s.ctl.model.instruments[0].modulators.iter().map(|m| if let ModSourceSlot::Lfo { rate, .. } = m.source { rate } else { -1.0 }).collect()
    }

    #[test]
    fn the_cursor_stays_on_its_node_when_a_browser_adds_a_node_above_it() {
        let mut h = Harness::new(TWO_LANES);
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        let lane0 = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::AddModulator { parent: lane0, kind: ModulatorKind::Lfo });
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
        // And what `d` takes out is the reverb, which the person was on, and not what has come to its place.
        h.press(KeyCode::Char('d'));
        let lane = &h.s.ctl.model.instruments[1];
        assert!(lane.instrument.is_some() && lane.effects.is_empty(), "the reverb went: {:#?}", h.tree());
    }

    #[test]
    fn an_effect_a_browser_adds_is_in_the_tree_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(TWO_LANES);
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        let lane0 = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::AddEffect { parent: lane0, plugin: "builtin:filter".to_string() });
        assert!(h.tree().iter().any(|line| line.contains("fx Filter")), "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
        assert!(h.s.ctl.dirty);
    }

    #[test]
    fn a_lane_a_browser_adds_is_at_the_end_of_the_tree_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(TWO_LANES);
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        let before = h.tree().len();
        browser(&mut h, Intent::AddInstrument { plugin: "builtin:osc".to_string() });
        assert_eq!(h.s.ctl.model.instruments.len(), 3);
        assert_eq!(h.tree().len(), before + 1, "{:#?}", h.tree());
        assert!(h.tree().last().is_some_and(|line| line.contains("Oscillator")), "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
        assert!(h.s.ctl.dirty);
        // A lane that came from a browser is a lane like any: the first `d` empties it, a second takes it out.
        h.goto(TreeAddress::Instrument(2));
        h.press(KeyCode::Char('d')).press(KeyCode::Char('d'));
        assert_eq!(h.s.ctl.model.instruments.len(), 2);
    }

    #[test]
    fn an_instrument_a_browser_puts_in_a_lane_takes_the_cable_to_the_old_one_out_of_the_tree_and_the_cursor_stays() {
        let mut h = Harness::new(scene_split());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        h.goto(TreeAddress::Instrument(0));
        let lane = cursor_node(&h);
        assert!(h.tree().iter().any(|line| line.contains("LFO 4.0Hz sine → ")), "{:#?}", h.tree());
        browser(&mut h, Intent::ReplaceInstrument { node: lane, plugin: "builtin:osc".to_string() });
        assert!(h.tree().iter().any(|line| line.contains("♪ Oscillator")), "{:#?}", h.tree());
        assert!(h.tree().iter().any(|line| line.contains("LFO 4.0Hz sine") && !line.contains('→')), "the vibrato's cable went with the old instrument: {:#?}", h.tree());
        assert!(h.tree().iter().any(|line| line.contains("LFO 7.0Hz sine → ")), "the other lane's stays: {:#?}", h.tree());
        assert_eq!(cursor_node(&h), lane, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Instrument(0));
    }

    #[test]
    fn a_row_on_the_cable_that_goes_with_a_replaced_instrument_is_not_left_selected() {
        let mut h = Harness::new(scene_split());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        // On the vibrato's own parameters, on the row of its cable (the last).
        h.goto(TreeAddress::Modulator { inst: 0, index: 0 });
        h.press(KeyCode::Enter);
        let rows = h.s.param_state.len;
        h.press_n(KeyCode::Down, rows - 1);
        assert_eq!(h.s.param_state.selected, rows - 1, "the last row, which is the cable's");
        let lane = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::ReplaceInstrument { node: lane, plugin: "builtin:osc".to_string() });
        assert!(h.s.ctl.model.instruments[0].modulators[0].targets.is_empty());
        // The pane has fewer rows, and the selection is on the last of them.
        assert!(h.s.param_state.len < rows, "{} rows of {rows}", h.s.param_state.len);
        assert_eq!(h.s.param_state.selected, h.s.param_state.len - 1);
        // The frame is drawn from what is there, and the keys still do what they do.
        assert!(!h.dump().is_empty());
        h.press(KeyCode::Down).press(KeyCode::Up).press(KeyCode::Left).press(KeyCode::Right);
        assert!(!h.dump().is_empty());
    }

    #[test]
    fn the_cursor_stays_on_its_node_when_a_browser_takes_out_an_effect_above_it() {
        let mut h = Harness::new(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
"#,
        );
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        let filter = h.s.ctl.model.instruments[0].effects[0].node;
        browser(&mut h, Intent::RemoveNode { node: filter });
        assert!(h.tree().iter().all(|line| !line.contains("fx Filter")), "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
        assert!(h.s.ctl.dirty);
    }

    /// A lane with a filter and then a reverb, and a group with a filter and then a reverb on its bus and a lane in it.
    const TWO_CHAINS: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
group = 0
"#;

    /// Where the line that has `text` is in the tree.
    fn line_of(h: &Harness, text: &str) -> usize {
        h.tree().iter().position(|line| line.contains(text)).unwrap_or_else(|| panic!("no {text:?} in {:#?}", h.tree()))
    }

    #[test]
    fn the_cursor_follows_its_effect_when_a_browser_moves_it_and_the_tree_shows_the_new_order() {
        let mut h = Harness::new(TWO_CHAINS);
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        let filter = cursor_node(&h);
        assert!(line_of(&h, "fx Filter  [Built-in]") < line_of(&h, "fx Reverb  [Built-in]"), "{:#?}", h.tree());
        browser(&mut h, Intent::MoveEffect { node: filter, index: 1 });
        // (The lane's two effects are the first two of the tree's effects; the bus has its own, marked.)
        let lane_filter = h.tree().iter().position(|line| line.contains("fx Filter  [Built-in]") && !line.contains("(bus)")).unwrap();
        let lane_reverb = h.tree().iter().position(|line| line.contains("fx Reverb  [Built-in]") && !line.contains("(bus)")).unwrap();
        assert!(lane_reverb < lane_filter, "the reverb is first now: {:#?}", h.tree());
        assert_eq!(cursor_node(&h), filter, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 0, index: 1 });
        assert!(h.s.ctl.dirty);
        // What `d` takes out is the filter, which the person was on, and not what has come to its place.
        h.press(KeyCode::Char('d'));
        let chain: Vec<_> = h.s.ctl.model.instruments[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(chain, ["Reverb"]);
    }

    #[test]
    fn what_a_browser_pins_to_a_node_leaves_the_tree_as_it_is_and_the_session_unsaved() {
        let mut h = Harness::new(TWO_CHAINS);
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        let filter = cursor_node(&h);
        let before: Vec<String> = h.tree().iter().map(|line| line.to_string()).collect();
        let cutoff = h.s.ctl.model.instruments[0].effects[0].params.iter().find(|p| p.name == "cutoff").unwrap().index;
        browser(&mut h, Intent::SetPinned { node: filter, params: vec![cutoff] });
        assert_eq!(h.s.ctl.model.instruments[0].effects[0].pinned, [cutoff]);
        let after: Vec<String> = h.tree().iter().map(|line| line.to_string()).collect();
        assert_eq!(after, before, "the TUI does not draw which knobs a node has");
        assert_eq!(cursor_node(&h), filter);
        assert!(h.s.ctl.dirty, "it is in the file when the session is saved");
    }

    #[test]
    fn the_cursor_is_left_where_it_was_by_a_move_of_an_effect_it_is_not_on() {
        let mut h = Harness::new(TWO_CHAINS);
        h.goto(TreeAddress::Instrument(0));
        let lane = cursor_node(&h);
        let filter = h.s.ctl.model.instruments[0].effects[0].node;
        browser(&mut h, Intent::MoveEffect { node: filter, index: 1 });
        assert_eq!(cursor_node(&h), lane);
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Instrument(0));
    }

    #[test]
    fn a_bus_effect_a_browser_moves_is_in_its_new_place_in_the_tree_and_the_cursor_follows_it() {
        let mut h = Harness::new(TWO_CHAINS);
        h.goto(TreeAddress::GroupEffect { group: 0, index: 1 });
        let reverb = cursor_node(&h);
        browser(&mut h, Intent::MoveEffect { node: reverb, index: 0 });
        assert!(line_of(&h, "fx Reverb  [Built-in]  (bus)") < line_of(&h, "fx Filter  [Built-in]  (bus)"), "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb);
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::GroupEffect { group: 0, index: 0 });
        assert!(h.s.ctl.dirty);
    }

    #[test]
    fn a_value_typed_in_the_tui_goes_to_the_effect_it_was_typed_for_when_a_browser_moves_it() {
        let mut h = Harness::new(TWO_CHAINS);
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        h.press(KeyCode::Enter); // into the filter's parameters, on the first: the cutoff
        let filter = cursor_node(&h);
        h.press(KeyCode::Enter); // the value popup
        assert!(h.s.editing.is_some());
        for _ in 0..8 {
            h.press(KeyCode::Backspace);
        }
        h.type_chars("0.25");
        browser(&mut h, Intent::MoveEffect { node: filter, index: 1 });
        assert!(h.s.editing.is_some(), "what was being typed is for a node that is still there");
        h.press(KeyCode::Enter);
        let effects = &h.s.ctl.model.instruments[0].effects;
        assert_eq!((effects[1].name.as_str(), effects[1].params[0].value), ("Filter", 0.25), "the filter, which is the second now, has it");
        assert_ne!(effects[0].params[0].value, 0.25, "and the reverb, which has come to the filter's place, does not");
    }

    #[test]
    fn a_selector_of_presets_is_closed_when_a_browser_moves_an_effect_whose_place_it_holds() {
        let mut h = Harness::new(TWO_CHAINS);
        h.goto(TreeAddress::Effect { inst: 0, index: 1 }); // the reverb, which has presets
        h.press(KeyCode::Char('p'));
        assert!(h.s.preset_selector.is_some(), "the reverb has presets");
        let filter = h.s.ctl.model.instruments[0].effects[0].node;
        browser(&mut h, Intent::MoveEffect { node: filter, index: 1 });
        assert!(h.s.preset_selector.is_none(), "it holds the place of an effect that has moved");
    }

    #[test]
    fn popups_are_closed_when_a_browser_takes_out_the_effect_the_cursor_was_on() {
        let mut h = Harness::new(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
"#,
        );
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        h.press(KeyCode::Enter); // its parameters
        h.press(KeyCode::Enter); // the value popup for the first
        assert!(h.s.editing.is_some());
        let reverb = cursor_node(&h);
        browser(&mut h, Intent::RemoveNode { node: reverb });
        assert!(h.s.editing.is_none(), "what was being typed was for a node that is gone");
        assert!(h.s.ctl.model.instruments[0].effects.is_empty());
    }

    #[test]
    fn the_cursor_stays_on_its_node_when_a_browser_takes_out_a_node_above_it() {
        let mut h = Harness::new(THREE_LFOS);
        h.goto(TreeAddress::Modulator { inst: 0, index: 1 });
        let second = cursor_node(&h);
        let first = h.s.ctl.model.instruments[0].modulators[0].node;
        browser(&mut h, Intent::RemoveNode { node: first });
        assert_eq!(cursor_node(&h), second, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Modulator { inst: 0, index: 0 });
    }

    #[test]
    fn a_value_typed_in_the_tui_goes_to_the_node_it_was_typed_for_when_a_browser_adds_a_node_above() {
        let mut h = Harness::new(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument]]
plugin = "builtin:osc"
"#,
        );
        h.goto(TreeAddress::Instrument(1));
        h.press(KeyCode::Enter); // into the parameters of the second oscillator
        h.press(KeyCode::Down).press(KeyCode::Down); // the third row
        let before = h.s.ctl.model.instruments[1].instrument.as_ref().unwrap().params[2].value;
        h.press(KeyCode::Enter); // the value popup
        assert!(h.s.editing.is_some());
        for _ in 0..8 {
            h.press(KeyCode::Backspace);
        }
        h.type_chars("0.25");
        let lane0 = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::AddModulator { parent: lane0, kind: ModulatorKind::Lfo });
        h.press(KeyCode::Enter);
        let after = h.s.ctl.model.instruments[1].instrument.as_ref().unwrap().params[2].value;
        assert_eq!(after, 0.25, "it was {before}: the value reached the parameter it was typed for");
        assert_eq!(rates(&h), [1.0], "and the first lane has the one LFO the browser gave it, at the rate every new one has: not what was typed");
    }

    #[test]
    fn a_selector_of_targets_is_closed_when_a_browser_takes_out_a_modulator_it_counts_by() {
        let mut h = Harness::new(THREE_LFOS);
        h.goto(TreeAddress::Modulator { inst: 0, index: 1 }); // the 2 Hz LFO
        h.press(KeyCode::Char('t'));
        assert!(h.s.target_selector.is_some());
        let first = h.s.ctl.model.instruments[0].modulators[0].node;
        browser(&mut h, Intent::RemoveNode { node: first });
        assert!(h.s.target_selector.is_none(), "it holds the place of a modulator that has moved");
        h.press(KeyCode::Enter);
        assert_eq!(rates(&h), [2.0, 3.0]);
        assert!(h.s.ctl.model.instruments[0].modulators.iter().all(|m| m.targets.is_empty()), "no cable went to the one that was not chosen");
    }

    #[test]
    fn popups_are_closed_when_a_browser_takes_out_the_node_the_cursor_was_on() {
        let mut h = Harness::new(THREE_LFOS);
        h.goto(TreeAddress::Modulator { inst: 0, index: 1 });
        h.press(KeyCode::Enter); // its parameters: the type, the waveform, then the rate
        h.press(KeyCode::Down).press(KeyCode::Down);
        h.press(KeyCode::Enter); // the value popup for the rate
        assert!(h.s.editing.is_some());
        let second = cursor_node(&h);
        browser(&mut h, Intent::RemoveNode { node: second });
        assert!(h.s.editing.is_none(), "what was being typed was for a node that is gone");
        assert_eq!(rates(&h), [1.0, 3.0]);
    }

    #[test]
    fn a_popup_is_left_alone_by_a_change_that_is_not_one_of_structure() {
        let mut h = Harness::new(THREE_LFOS);
        h.goto(TreeAddress::Modulator { inst: 0, index: 1 });
        h.press(KeyCode::Char('t'));
        assert!(h.s.target_selector.is_some());
        let third = h.s.ctl.model.instruments[0].modulators[2].node;
        browser(&mut h, Intent::SetModulatorField { node: third, field: crate::app::intent::ModulatorField::Rate, value: 9.0 });
        assert!(h.s.target_selector.is_some(), "the third LFO's rate changed: nothing moved");
    }

    #[test]
    fn a_save_that_a_browser_asks_for_takes_the_mark_off_the_session_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = Harness::new(TWO_LANES);
        h.s.ctl.session_path = Some(dir.path().join("song.toml"));
        let lane = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::SetGain { node: lane, gain: crate::app::intent::Gain::Volume, value: 0.5 });
        assert!(tab_bar(&mut h).contains("(1) Session *"), "{}", tab_bar(&mut h));
        browser(&mut h, Intent::Save);
        let bar = tab_bar(&mut h);
        assert!(bar.contains("(1) Session") && !bar.contains("Session *"), "{bar}");
        assert!(dir.path().join("song.toml").exists(), "and the file is there");
        // A save that fails says nothing to the TUI and keeps the mark.
        h.s.ctl.session_path = Some(dir.path().join("song.toml").join("not-a-directory.toml"));
        browser(&mut h, Intent::SetGain { node: lane, gain: crate::app::intent::Gain::Volume, value: 0.75 });
        assert!(h.s.ctl.apply(Intent::Save).is_err());
        assert!(tab_bar(&mut h).contains("Session *"));
    }

    #[test]
    fn a_popup_is_left_alone_by_a_cable_that_moves_no_place() {
        let mut h = Harness::new(THREE_LFOS);
        h.goto(TreeAddress::Modulator { inst: 0, index: 1 });
        h.press(KeyCode::Char('t'));
        assert!(h.s.target_selector.is_some());
        let lane = &h.s.ctl.model.instruments[0];
        let volume = lane.instrument.as_ref().unwrap().params.iter().find(|p| p.name == "volume").unwrap().index;
        let to = Endpoint { node: lane.node, port: Port::Param { index: volume } };
        let applied = h.s.ctl.apply(Intent::AddModEdge { modulator: lane.modulators[0].node, to }).unwrap();
        assert!(applied.restructured, "a cable is a change of structure");
        let places = h.s.tree_places();
        h.s.react_to(&applied);
        assert_eq!(h.s.tree_places(), places, "but the tree has the same nodes in the same places");
        assert!(h.tree()[1].contains('→'), "and shows the cable: {:#?}", h.tree());
        assert!(h.s.target_selector.is_some(), "nothing it counts by has moved");
    }

    /// A group with a lane in it, then a lane that is in none, which has a reverb (it has presets) and an LFO.
    const GROUP_AND_LANE: &str = r#"
[[group]]
name = "Pad"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 1.0

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.modulator]]
rate = 2.0
"#;

    type IsOpen = fn(&State) -> bool;

    /// Every popup that holds a place in the tree of `GROUP_AND_LANE`: what it is, where the cursor is when it is opened, the keys that open it
    /// and how to tell that it is open.
    fn popups_that_hold_a_place() -> [(&'static str, TreeAddress, &'static [KeyCode], IsOpen); 7] {
        [
            ("the range of a lane", TreeAddress::Instrument(1), &[KeyCode::Char('R')], |s| s.range_edit.is_some()),
            ("the name of a group", TreeAddress::Group(0), &[KeyCode::Char('R')], |s| s.rename_group.is_some()),
            ("the selector of a plugin", TreeAddress::Instrument(1), &[KeyCode::Char('a')], |s| s.selector.is_some()),
            ("the selector of targets", TreeAddress::Modulator { inst: 1, index: 0 }, &[KeyCode::Char('t')], |s| s.target_selector.is_some()),
            ("the choice of a modulator", TreeAddress::Effect { inst: 1, index: 0 }, &[KeyCode::Enter, KeyCode::Char('m')], |s| s.modulate.is_some()),
            ("the group of a lane", TreeAddress::Instrument(1), &[KeyCode::Char('g')], |s| s.group_assign.is_some()),
            ("the presets of a plugin", TreeAddress::Effect { inst: 1, index: 0 }, &[KeyCode::Char('p')], |s| s.preset_selector.is_some()),
        ]
    }

    #[test]
    fn every_popup_that_holds_a_place_is_closed_when_a_browser_moves_the_places() {
        for (what, at, keys, is_open) in popups_that_hold_a_place() {
            let mut h = Harness::new(GROUP_AND_LANE);
            h.goto(at);
            for key in keys {
                h.press(*key);
            }
            assert!(is_open(&h.s), "{what} is open to begin with");
            let in_the_group = h.s.ctl.model.instruments[0].node;
            browser(&mut h, Intent::AddModulator { parent: in_the_group, kind: ModulatorKind::Lfo });
            assert!(!is_open(&h.s), "{what} is for a place that has moved");
        }
    }

    #[test]
    fn every_popup_that_holds_a_place_is_closed_when_a_browser_adds_a_group_and_when_it_takes_one_out() {
        type Mover = fn(&Harness) -> Intent;
        let movers: [(&str, Mover); 3] = [
            ("adds a group", |_| Intent::AddGroup { members: vec![] }),
            ("adds a group with the lane in it", |h| Intent::AddGroup { members: vec![h.s.ctl.model.instruments[1].node] }),
            ("takes the group out", |h| Intent::RemoveNode { node: h.s.ctl.model.groups[0].node }),
        ];
        for (move_, mover) in movers {
            for (what, at, keys, is_open) in popups_that_hold_a_place() {
                let mut h = Harness::new(GROUP_AND_LANE);
                h.goto(at);
                for key in keys {
                    h.press(*key);
                }
                assert!(is_open(&h.s), "{what} is open to begin with");
                let intent = mover(&h);
                browser(&mut h, intent);
                assert!(!is_open(&h.s), "{what} is for a place that has moved when a browser {move_}");
            }
        }
    }

    #[test]
    fn the_cursor_stays_on_a_lanes_pattern_and_does_not_go_up_to_its_instrument() {
        // The pattern and the instrument of a lane are two entries of one node.
        let mut h = Harness::new(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument]]
plugin = "builtin:osc"
[instrument.pattern]
bpm = 120.0
length_beats = 4.0
base_note = "C4"
enabled = true
[[instrument.pattern.events]]
frame = 0
status = "on"
note = "C4"
velocity = 100
[[instrument.pattern.events]]
frame = 24000
status = "off"
note = "C4"
velocity = 0
"#,
        );
        h.goto(TreeAddress::Pattern(1));
        let lane0 = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::AddModulator { parent: lane0, kind: ModulatorKind::Lfo });
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Pattern(1), "{:#?}", h.tree());
    }

    // -- what a browser does to groups, as the TUI shows it --

    /// A group with a lane in it, another that is empty, and a lane in none.
    const TWO_GROUPS_AND_LANE: &str = r#"
[[group]]
name = "Pad"

[[group]]
name = "Lead"

[[instrument]]
plugin = "builtin:osc"
group = 0

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
"#;

    /// The lines of the tree that are a group's header.
    fn headers(h: &Harness) -> Vec<&str> {
        h.tree().into_iter().filter(|line| line.starts_with('▦')).collect()
    }

    /// The lines of the tree that are a lane's instrument and in no group (a member of a group is indented under its header).
    fn ungrouped_lanes(h: &Harness) -> usize {
        h.tree().iter().filter(|line| line.starts_with('♪')).count()
    }

    #[test]
    fn a_group_a_browser_adds_is_in_the_tree_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(TWO_LANES);
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        assert!(headers(&h).is_empty(), "{:#?}", h.tree());
        browser(&mut h, Intent::AddGroup { members: vec![] });
        assert_eq!(h.s.ctl.model.groups.len(), 1);
        assert_eq!(headers(&h).len(), 1, "{:#?}", h.tree());
        assert!(headers(&h)[0].contains("Group 1"), "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
        assert!(h.s.ctl.dirty);
        // A group that came from a browser is a group like any: `R` names it in the TUI and `d` takes it out.
        h.goto(TreeAddress::Group(0));
        h.press(KeyCode::Char('R')).type_chars("Pad").press(KeyCode::Enter);
        // (The field starts with the name the group was given, and a group that has none starts with nothing.)
        assert_eq!(h.s.ctl.model.groups[0].name.as_deref(), Some("Pad"));
        h.press(KeyCode::Char('d'));
        assert!(h.s.ctl.model.groups.is_empty(), "{:#?}", h.tree());
        assert!(headers(&h).is_empty(), "{:#?}", h.tree());
    }

    #[test]
    fn a_group_a_browser_makes_with_a_lane_in_it_has_the_lane_under_its_header_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(TWO_LANES);
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        assert_eq!(ungrouped_lanes(&h), 2, "{:#?}", h.tree());
        let lane = h.s.ctl.model.instruments[1].node;
        browser(&mut h, Intent::AddGroup { members: vec![lane] });
        assert_eq!(h.s.ctl.model.instruments[1].group, Some(0));
        let tree = h.tree();
        // The header, and under it the lane with its reverb; the lane that is in no group is after.
        let header = tree.iter().position(|line| line.starts_with('▦')).expect("a header");
        assert!(tree[header + 1].contains("♪ Oscillator") && tree[header + 1].starts_with("  "), "{tree:#?}");
        assert!(tree[header + 2].contains("fx Reverb"), "{tree:#?}");
        assert_eq!(ungrouped_lanes(&h), 1, "{tree:#?}");
        assert_eq!(cursor_node(&h), reverb, "{tree:#?}");
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
    }

    #[test]
    fn a_lane_a_browser_takes_out_of_its_group_is_under_no_header_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(GROUP_AND_LANE);
        h.goto(TreeAddress::Modulator { inst: 0, index: 0 });
        let lfo = cursor_node(&h);
        assert_eq!(ungrouped_lanes(&h), 1, "{:#?}", h.tree());
        let lane = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::SetLaneGroup { node: lane, group: None });
        assert_eq!(h.s.ctl.model.instruments[0].group, None);
        // The group is still there, empty, and both lanes are in none.
        assert_eq!(headers(&h).len(), 1, "{:#?}", h.tree());
        assert_eq!(ungrouped_lanes(&h), 2, "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), lfo, "{:#?}", h.tree());
        assert!(h.s.ctl.dirty);
    }

    #[test]
    fn a_name_a_browser_gives_a_group_is_in_the_tree_and_in_the_title_of_its_pane_and_a_name_of_nothing_gives_the_default_back() {
        let mut h = Harness::new(GROUP_AND_LANE);
        h.goto(TreeAddress::Group(0));
        let pad = cursor_node(&h);
        assert!(h.dump().contains("┌ Pad "), "{}", h.dump());
        browser(&mut h, Intent::RenameGroup { node: pad, name: "  Strings  ".to_string() });
        assert!(headers(&h)[0].contains("Strings") && !headers(&h)[0].contains("Pad"), "{:#?}", h.tree());
        let frame = h.dump();
        assert!(frame.contains("┌ Strings ") && !frame.contains("┌ Pad "), "{frame}");
        assert!(h.s.ctl.dirty);
        browser(&mut h, Intent::RenameGroup { node: pad, name: " ".to_string() });
        assert!(headers(&h)[0].contains("Group 1"), "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), pad);
    }

    #[test]
    fn a_name_typed_in_the_tui_is_for_its_group_when_a_browser_names_another() {
        let mut h = Harness::new(TWO_GROUPS_AND_LANE);
        // The TUI is naming the second group, and a browser names the first.
        h.goto(TreeAddress::Group(1));
        h.press(KeyCode::Char('R')).type_chars("!");
        assert_eq!(h.s.rename_group.as_ref().map(|rg| rg.input.value.as_str()), Some("Lead!"));
        let pad = h.s.ctl.model.groups[0].node;
        browser(&mut h, Intent::RenameGroup { node: pad, name: "Keys".to_string() });
        // A name changes no place: what is being typed stays, and goes to the group it was typed for.
        assert_eq!(h.s.rename_group.as_ref().map(|rg| rg.input.value.as_str()), Some("Lead!"));
        h.press(KeyCode::Enter);
        let names: Vec<_> = h.s.ctl.model.groups.iter().map(|g| g.name.clone()).collect();
        assert_eq!(names, [Some("Keys".to_string()), Some("Lead!".to_string())]);
    }

    #[test]
    fn a_group_a_browser_takes_out_leaves_its_lanes_in_the_tree_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(GROUP_AND_LANE);
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        let pad = h.s.ctl.model.groups[0].node;
        browser(&mut h, Intent::RemoveNode { node: pad });
        assert!(h.s.ctl.model.groups.is_empty());
        assert_eq!(h.s.ctl.model.instruments.len(), 2, "the lanes stay");
        assert!(h.s.ctl.model.instruments.iter().all(|lane| lane.group.is_none()));
        assert!(headers(&h).is_empty(), "{:#?}", h.tree());
        assert_eq!(ungrouped_lanes(&h), 2, "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
    }

    #[test]
    fn the_cursor_is_on_a_node_of_the_tree_when_a_browser_takes_out_the_group_it_was_on() {
        let mut h = Harness::new(GROUP_AND_LANE);
        h.goto(TreeAddress::Group(0));
        let pad = cursor_node(&h);
        browser(&mut h, Intent::RemoveNode { node: pad });
        assert!(h.s.chain_state.selected < h.s.tree_entries.len(), "{:#?}", h.tree());
        assert_ne!(cursor_node(&h), pad);
        // The frame is drawn from what is there, and the keys still do what they do.
        assert!(!h.dump().is_empty());
        h.press(KeyCode::Down).press(KeyCode::Up).press(KeyCode::Enter).press(KeyCode::Esc);
        assert!(h.s.chain_state.selected < h.s.tree_entries.len());
    }

    #[test]
    fn a_lane_a_browser_takes_out_is_gone_from_the_tree_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(GROUP_AND_LANE);
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        let lane = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::RemoveNode { node: lane });
        assert_eq!(h.s.ctl.model.instruments.len(), 1);
        // The lane that was in the group is gone with its modulator, and the group is empty.
        assert_eq!(headers(&h).len(), 1, "{:#?}", h.tree());
        assert!(!h.tree().iter().any(|line| line.contains("LFO 1.0Hz")), "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 0, index: 0 });
    }
}

mod undo_and_redo {
    //! Ctrl+Z and Ctrl+Y: the controller's history (`Controller::undo` / `redo`), which the browser's buttons ask for too.

    use super::*;
    use crate::app::intent::Intent;

    fn ctrl_z(h: &mut Harness) {
        h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    }

    fn ctrl_y(h: &mut Harness) {
        h.press_with(KeyCode::Char('y'), KeyModifiers::CONTROL);
    }

    fn cutoff_of_the_bus_filter(h: &Harness) -> f32 {
        h.s.ctl.model.groups[0].effects[0].params[0].value
    }

    #[test]
    fn the_help_tab_lists_the_keys() {
        let mut h = Harness::new(scene_bus_pair());
        h.press(KeyCode::Char('4'));
        let frame = h.dump();
        assert!(frame.contains("Ctrl+Z     Undo the last thing done"), "{frame}");
        assert!(frame.contains("Ctrl+Y     Redo (Ctrl+Shift+Z too)"), "{frame}");
    }

    #[test]
    fn ctrl_z_puts_back_what_a_key_set_and_ctrl_y_does_it_again_in_the_model_and_in_the_engine() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.press(KeyCode::Enter).press(KeyCode::Left); // the filter's cutoff, 5% down from fully open
        assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.950"]);
        assert_eq!(cutoff_of_the_bus_filter(&h), 0.95);

        h.s.ctl.dirty = false;
        ctrl_z(&mut h);
        assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=1.000"]);
        assert_eq!(cutoff_of_the_bus_filter(&h), 1.0);
        assert!(h.s.ctl.dirty, "going back is a change of the session too");

        ctrl_y(&mut h);
        assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.950"]);
        assert_eq!(cutoff_of_the_bus_filter(&h), 0.95);
    }

    #[test]
    fn a_drag_of_keys_is_one_step_for_ctrl_z_as_it_is_for_the_browsers_undo() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.press(KeyCode::Enter).press_n(KeyCode::Left, 4); // 20% down, in four presses a moment apart
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(cutoff_of_the_bus_filter(&h), 1.0, "all four are put back at once");
        assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=1.000"]);
    }

    #[test]
    fn ctrl_shift_z_redoes_as_ctrl_y_does_and_a_capital_z_is_the_same_key() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.press(KeyCode::Enter).press(KeyCode::Left);
        ctrl_z(&mut h);
        h.commands();
        h.press_with(KeyCode::Char('z'), ctrl_shift());
        assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.950"]);
        ctrl_z(&mut h);
        h.commands();
        h.press_with(KeyCode::Char('Z'), ctrl_shift());
        assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.950"]);
    }

    #[test]
    fn a_z_and_a_y_without_control_are_no_undo_and_no_redo() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.press(KeyCode::Enter).press(KeyCode::Left);
        ctrl_z(&mut h);
        h.commands();
        // A key of the piano on its own tab, and a letter on the others: what was undone is not done again, and the other way round.
        for tab in ['1', '2'] {
            h.press(KeyCode::Char(tab));
            h.press(KeyCode::Char('y')).press(KeyCode::Char('z')).press_with(KeyCode::Char('Z'), KeyModifiers::SHIFT);
        }
        assert!(h.commands().is_empty());
        assert_eq!(cutoff_of_the_bus_filter(&h), 1.0);
        ctrl_y(&mut h);
        h.commands();
        h.press(KeyCode::Char('1')).press(KeyCode::Char('z')).press(KeyCode::Char('y'));
        assert!(h.commands().is_empty());
        assert_eq!(cutoff_of_the_bus_filter(&h), 0.95);
    }

    #[test]
    fn ctrl_z_and_ctrl_y_with_nothing_to_undo_or_redo_do_nothing() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.s.ctl.dirty = false;
        ctrl_z(&mut h);
        ctrl_y(&mut h);
        assert!(h.commands().is_empty());
        assert_eq!(cutoff_of_the_bus_filter(&h), 1.0);
        assert!(!h.s.ctl.dirty);
    }

    #[test]
    fn ctrl_z_puts_back_the_keys_of_a_lane_and_the_tree_says_so() {
        let mut h = Harness::new(scene_split());
        let before = h.tree()[0].to_string();
        h.press(KeyCode::Char('R')).press_n(KeyCode::Backspace, 5).type_chars("C2-C6").press(KeyCode::Enter);
        assert_eq!(h.tree()[0], "♪ Oscillator  [Built-in]  C2-C6");
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(h.tree()[0], before, "the label of the lane is what it was");
        assert_eq!(h.commands(), ["SetInstrumentRange inst=0 range=Some((12, 59))"], "C0-B3, as the scene has it");
        ctrl_y(&mut h);
        assert_eq!(h.tree()[0], "♪ Oscillator  [Built-in]  C2-C6");
    }

    #[test]
    fn ctrl_z_puts_back_what_a_browser_did_and_the_tree_follows_the_effects_back_to_their_places() {
        let mut h = Harness::new(scene_two_effects());
        let order = |h: &Harness| h.tree().iter().filter(|line| line.contains("fx ")).map(|line| line.to_string()).collect::<Vec<_>>();
        let before = order(&h);
        let filter = h.s.ctl.model.instruments[0].effects[0].node;
        // What a browser does: the intent is applied by the controller, and the loop of the TUI reacts to it.
        let applied = h.s.ctl.apply(Intent::MoveEffect { node: filter, index: 1 }).unwrap();
        h.s.react_to(&applied);
        assert_ne!(order(&h), before, "the effects are in the other order");
        ctrl_z(&mut h);
        assert_eq!(order(&h), before);
        ctrl_y(&mut h);
        assert_ne!(order(&h), before);
    }

    #[test]
    fn an_effect_added_from_the_selector_is_a_step_that_ctrl_z_takes_out_and_ctrl_y_puts_back() {
        let mut h = Harness::new(scene_split());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        h.press(KeyCode::Char('a')).type_chars("reverb").press(KeyCode::Enter);
        assert_eq!(h.commands(), ["InsertEffect inst=0 index=0 plugin=Reverb mix=0.500"]);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("add Reverb"));
        ctrl_z(&mut h);
        assert_eq!(h.commands(), ["RemoveEffect inst=0 index=0 keep"]);
        assert!(h.tree().iter().all(|line| !line.contains("fx Reverb")), "{:#?}", h.tree());
        assert!(h.s.ctl.model.instruments[0].effects.is_empty());
        ctrl_y(&mut h);
        assert_eq!(h.commands(), ["InsertEffect inst=0 index=0 plugin=Reverb mix=0.500"]);
        assert!(h.tree().iter().any(|line| line.contains("fx Reverb")), "{:#?}", h.tree());
    }

    #[test]
    fn an_effect_on_a_bus_added_from_the_selector_is_a_step_too() {
        let mut h = Harness::new(scene_group());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        h.goto(TreeAddress::Group(0));
        let buses = h.s.ctl.model.groups[0].effects.len();
        h.press(KeyCode::Char('a')).type_chars("filter").press(KeyCode::Enter);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("add Filter"));
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(h.commands(), [format!("RemoveGroupEffect group=0 index={buses} keep")]);
        assert_eq!(h.s.ctl.model.groups[0].effects.len(), buses);
        ctrl_y(&mut h);
        assert_eq!(h.commands(), [format!("InsertGroupEffect group=0 index={buses} plugin=Filter mix=1.000")]);
    }

    #[test]
    fn an_effect_that_cannot_be_added_leaves_what_was_done_before_to_be_undone() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.press(KeyCode::Enter).press(KeyCode::Left);
        catalog_with_a_plugin_that_has_gone(&mut h);
        h.press(KeyCode::Esc).goto(TreeAddress::Group(0));
        h.press(KeyCode::Char('a')).type_chars("vanish").press(KeyCode::Enter);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("set cutoff"), "no step for what was refused, and nothing forgotten");
    }

    #[test]
    fn n_adds_a_lane_that_ctrl_z_takes_out_and_ctrl_y_puts_back() {
        let mut h = Harness::new(scene_split());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        let lanes = h.s.ctl.model.instruments.len();
        h.press(KeyCode::Char('n')).press(KeyCode::Esc); // (the selector is dismissed, and the lane stays with its oscillator)
        assert_eq!(h.s.ctl.model.instruments.len(), lanes + 1);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("add Oscillator as an instrument"));
        ctrl_z(&mut h);
        assert_eq!(h.s.ctl.model.instruments.len(), lanes);
        assert!(h.tree().iter().filter(|line| line.contains("♪ Oscillator")).count() <= lanes, "{:#?}", h.tree());
        ctrl_y(&mut h);
        assert_eq!(h.s.ctl.model.instruments.len(), lanes + 1);
        assert!(h.s.ctl.model.instruments[lanes].instrument.is_some(), "with its oscillator");
    }

    #[test]
    fn n_with_nothing_in_the_catalog_that_names_the_oscillator_adds_no_lane() {
        let mut h = Harness::new(scene_split());
        let lanes = h.s.ctl.model.instruments.len();
        h.press(KeyCode::Char('n'));
        assert_eq!(h.s.ctl.model.instruments.len(), lanes, "a lane with nothing in it would make no sound: the catalog has to name what goes in");
        assert!(h.s.selector.is_none());
        assert_eq!(h.s.ctl.history_state().undo, None);
    }

    #[test]
    fn the_modulate_popup_with_a_new_lfo_is_one_step_that_ctrl_z_takes_out_whole_and_ctrl_y_puts_back() {
        let mut h = on_the_bass_filters_cutoff();
        let before = h.s.ctl.model.graph();
        h.press(KeyCode::Char('m')).press(KeyCode::Enter);
        let after = h.s.ctl.model.graph();
        assert_ne!(before, after);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("modulate cutoff"), "the LFO and the cable to it are one thing");
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(h.s.ctl.model.graph(), before);
        assert_eq!(h.s.ctl.model.instruments[2].modulators.len(), 1, "the LFO the scene has");
        assert_eq!(h.commands().len(), 1, "the LFO goes, and the cable with it (the audio thread drops a modulator with its cables)");
        ctrl_y(&mut h);
        assert_eq!(h.s.ctl.model.graph(), after);
        assert_eq!(h.commands().len(), 2, "the LFO is made again, and the cable to it");
    }

    #[test]
    fn the_modulate_popup_with_a_modulator_that_is_there_is_a_step_of_a_cable() {
        let mut h = on_the_bass_filters_cutoff();
        h.press(KeyCode::Down); // the filter's resonance
        h.press(KeyCode::Char('m')).press(KeyCode::Down).press(KeyCode::Down).press(KeyCode::Enter);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("draw a cable"));
        ctrl_z(&mut h);
        assert_eq!(h.s.ctl.model.instruments[2].modulators[0].targets.len(), 1, "the cable of the LFO that was there");
    }

    #[test]
    fn learning_a_parameter_is_one_step_and_ctrl_z_takes_the_armed_modulator_out_with_its_cable() {
        let mut h = on_the_bass_filters_cutoff();
        let before = h.s.ctl.model.graph();
        h.press(KeyCode::Char('l'));
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("learn cutoff"));
        let after = h.s.ctl.model.graph();
        ctrl_z(&mut h);
        assert_eq!(h.s.ctl.model.graph(), before);
        ctrl_y(&mut h);
        assert_eq!(h.s.ctl.model.graph(), after);
        let armed = &h.s.ctl.model.instruments[2].modulators[1];
        assert_eq!(armed.targets[0].depth, 1.0, "at full depth, as it was learned");
    }

    #[test]
    fn the_target_selector_makes_a_step_of_the_cable_it_draws() {
        let mut h = Harness::new(scene_group());
        h.goto(TreeAddress::Modulator { inst: 2, index: 0 });
        let before = h.s.ctl.model.instruments[2].modulators[0].targets.len();
        h.press(KeyCode::Char('t')).press(KeyCode::Enter);
        assert_eq!(h.s.ctl.model.instruments[2].modulators[0].targets.len(), before + 1);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("draw a cable"));
        ctrl_z(&mut h);
        assert_eq!(h.s.ctl.model.instruments[2].modulators[0].targets.len(), before);
    }

    #[test]
    fn m_on_an_instrument_adds_an_lfo_that_ctrl_z_takes_out() {
        let mut h = Harness::new(scene_split());
        h.goto(TreeAddress::Instrument(1));
        let before = h.s.ctl.model.instruments[1].modulators.len();
        h.press(KeyCode::Char('m'));
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("add an LFO"));
        ctrl_z(&mut h);
        assert_eq!(h.s.ctl.model.instruments[1].modulators.len(), before);
    }

    #[test]
    fn shift_down_on_an_effect_is_a_step_that_ctrl_z_puts_back_and_ctrl_y_does_again() {
        let mut h = Harness::new(scene_two_effects());
        let names = |h: &Harness| h.s.ctl.model.instruments[0].effects.iter().map(|fx| fx.name.clone()).collect::<Vec<_>>();
        let before = names(&h);
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
        let moved = names(&h);
        assert_ne!(moved, before, "the effects are in the other order");
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("move an effect"));
        ctrl_z(&mut h);
        assert_eq!(names(&h), before);
        ctrl_y(&mut h);
        assert_eq!(names(&h), moved);
    }

    #[test]
    fn shift_up_on_a_bus_effect_is_a_step_too() {
        let mut h = Harness::new(scene_bus_pair());
        // A third effect on the bus, so that one place up is not the first place.
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        let group = h.s.ctl.model.groups[0].node;
        h.s.ctl.apply(Intent::AddEffect { parent: group, plugin: "builtin:reverb".to_string() }).unwrap();
        h.s.rebuild_tree();
        let order = |h: &Harness| h.s.ctl.model.groups[0].effects.iter().map(|fx| fx.node).collect::<Vec<_>>();
        let before = order(&h);
        h.goto(TreeAddress::GroupEffect { group: 0, index: 2 });
        h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
        let moved = order(&h);
        assert_eq!(moved, [before[0], before[2], before[1]], "one place up");
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("move an effect"));
        ctrl_z(&mut h);
        assert_eq!(order(&h), before);
        ctrl_y(&mut h);
        assert_eq!(order(&h), moved);
    }

    #[test]
    fn shift_up_on_the_first_effect_and_shift_down_on_the_last_move_nothing_and_leave_no_step() {
        let mut h = Harness::new(scene_two_effects());
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
        h.goto(TreeAddress::Effect { inst: 0, index: 1 });
        h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(h.s.ctl.history_state().undo, None);
        assert!(h.commands().is_empty());
    }

    #[test]
    fn d_takes_a_modulator_out_as_a_step_that_ctrl_z_puts_back_with_its_cable_and_ctrl_y_takes_out_again() {
        let cases = [
            (scene_two_effects().to_string(), TreeAddress::Modulator { inst: 0, index: 0 }, "a lane's modulator", "InsertModulator inst=0 index=0 "),
            (scene_bus_pair().to_string(), TreeAddress::GroupModulator { group: 0, index: 0 }, "a group's modulator", "InsertGroupModulator group=0 index=0 "),
        ];
        for (scene, address, what, insert) in cases {
            let mut h = Harness::new(scene);
            h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            let shape = |h: &Harness| {
                let racks = h.s.ctl.model.instruments.iter().map(|lane| &lane.modulators).chain(h.s.ctl.model.groups.iter().map(|group| &group.modulators));
                racks.map(|rack| rack.iter().map(|m| (m.node, m.targets.iter().map(|t| (t.edge, t.depth)).collect::<Vec<_>>())).collect::<Vec<_>>()).collect::<Vec<_>>()
            };
            let before = shape(&h);
            h.commands();
            h.goto(address).press(KeyCode::Char('d'));
            assert_ne!(shape(&h), before, "{what} is gone");
            assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("remove an LFO"), "{what}");
            h.commands();
            ctrl_z(&mut h);
            let commands = h.commands();
            assert!(commands[0].starts_with(insert), "{what}: {commands:?}");
            assert_eq!(shape(&h), before, "{what} is back where it was, with its cable and the ids they had");
            ctrl_y(&mut h);
            assert_ne!(shape(&h), before, "{what} is taken out again");
            ctrl_z(&mut h);
            assert_eq!(shape(&h), before);
        }
    }

    #[test]
    fn d_takes_an_effect_out_as_a_step_that_ctrl_z_puts_back_in_its_place_with_its_cables_and_ctrl_y_takes_out_again() {
        let cases = [
            (scene_two_effects().to_string(), TreeAddress::Effect { inst: 0, index: 0 }, "an effect of a lane", "InsertEffect inst=0 index=0 plugin=Filter"),
            (scene_bus_pair().to_string(), TreeAddress::GroupEffect { group: 0, index: 0 }, "an effect of a bus", "InsertGroupEffect group=0 index=0 plugin=Filter"),
        ];
        for (scene, address, what, insert) in cases {
            let mut h = Harness::new(scene);
            h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            // Every chain and every cable, with the ids and slots they have.
            let shape = |h: &Harness| -> String {
                let lanes = h.s.ctl.model.instruments.iter().map(|lane| format!("{:?} {:?}", lane.effects, lane.modulators));
                let groups = h.s.ctl.model.groups.iter().map(|group| format!("{:?} {:?}", group.effects, group.modulators));
                lanes.chain(groups).collect::<Vec<_>>().join("\n")
            };
            let before = shape(&h);
            h.commands();
            h.goto(address).press(KeyCode::Char('d'));
            assert_ne!(shape(&h), before, "{what} is gone");
            assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("remove Filter"), "{what}");
            h.commands();
            ctrl_z(&mut h);
            let commands = h.commands();
            assert!(commands[0].starts_with(insert), "{what}: {commands:?}");
            assert_eq!(shape(&h), before, "{what} is back where it was, with its cables and the ids and slots they had");
            ctrl_y(&mut h);
            assert_ne!(shape(&h), before, "{what} is taken out again");
            ctrl_z(&mut h);
            assert_eq!(shape(&h), before);
        }
    }

    #[test]
    fn d_takes_a_node_out_and_forgets_the_history_as_the_browsers_remove_does() {
        use crate::app::history::HistoryState;
        // What nothing puts back yet, of each kind that the key takes out, in a scene that has one. (A modulator, an effect, a lane and a group are put back: tests of their own.)
        let cases = [(scene_group().to_string(), TreeAddress::Instrument(0), "an instrument, which is emptied")];
        for (scene, address, what) in cases {
            let mut h = Harness::new(scene);
            h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            assert!(h.s.ctl.history_state().undo.is_some(), "{what}: there is something to undo");
            h.goto(address).press(KeyCode::Char('d'));
            assert_eq!(h.s.ctl.history_state(), HistoryState { undo: None, redo: None }, "{what}");
        }
    }

    #[test]
    fn d_takes_a_lane_out_as_a_step_that_ctrl_z_puts_back_in_its_place_in_its_group_with_its_cables_and_ctrl_y_takes_out_again() {
        let mut h = Harness::new(scene_group());
        // Every lane and group, with the ids and slots they have: what a lane put back has to leave as it was.
        let shape = |h: &Harness| -> String {
            let lanes = h.s.ctl.model.instruments.iter().map(|lane| format!("{lane:?}"));
            let groups = h.s.ctl.model.groups.iter().map(|group| format!("{group:?}"));
            lanes.chain(groups).collect::<Vec<_>>().join("\n")
        };
        // The first key empties the lane, which nothing puts back; the second takes the lane out.
        h.goto(TreeAddress::Instrument(0)).press(KeyCode::Char('d'));
        h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let (lanes, before) = (h.s.ctl.model.instruments.len(), shape(&h));
        h.commands();
        h.goto(TreeAddress::Instrument(0)).press(KeyCode::Char('d'));
        assert_eq!(h.s.ctl.model.instruments.len(), lanes - 1);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("remove an empty lane"));
        // (The commands are read: the audio thread, which the test has not, would have handed the lane back.)
        let removed = h.commands();
        assert_eq!(removed.last().map(String::as_str), Some("RemoveInstrument inst=0 keep"), "{removed:?}");
        ctrl_z(&mut h);
        let commands = h.commands();
        assert!(commands.iter().any(|command| command.starts_with("InsertInstrument index=0 plugins=")), "{commands:?}");
        assert_eq!(shape(&h), before, "the lane is back where it was, in its group, with its cables and the ids and slots they had");
        ctrl_y(&mut h);
        assert_eq!(h.s.ctl.model.instruments.len(), lanes - 1, "taken out again");
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(shape(&h), before);
    }

    #[test]
    fn d_takes_a_group_out_as_a_step_that_ctrl_z_puts_back_with_its_members_and_cables_and_ctrl_y_takes_out_again() {
        let mut h = Harness::new(scene_group());
        // Every lane and group, with the ids and slots they have: what a group put back has to leave as it was.
        let shape = |h: &Harness| -> String {
            let lanes = h.s.ctl.model.instruments.iter().map(|lane| format!("{lane:?}"));
            let groups = h.s.ctl.model.groups.iter().map(|group| format!("{group:?}"));
            lanes.chain(groups).collect::<Vec<_>>().join("\n")
        };
        h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let before = shape(&h);
        h.commands();
        h.goto(TreeAddress::Group(0)).press(KeyCode::Char('d'));
        assert!(h.s.ctl.model.groups.is_empty(), "the group is gone");
        assert!(h.s.ctl.model.instruments.iter().all(|lane| lane.group.is_none()), "and its members are in none");
        let name = h.s.ctl.history_state().undo;
        assert!(name.as_deref().is_some_and(|name| name.starts_with("remove ")), "{name:?}");
        // (The commands are read: the audio thread, which the test has not, would have handed the group back.)
        let removed = h.commands();
        assert!(removed.iter().any(|command| command == "RemoveGroup group=0 keep"), "{removed:?}");
        ctrl_z(&mut h);
        let commands = h.commands();
        assert!(commands.iter().any(|command| command.starts_with("InsertGroup index=0 plugins=")), "{commands:?}");
        assert_eq!(shape(&h), before, "the group is back where it was, with its members, its cables and the ids and slots they had");
        ctrl_y(&mut h);
        assert!(h.s.ctl.model.groups.is_empty(), "taken out again");
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(shape(&h), before);
    }

    #[test]
    fn choosing_a_preset_is_a_step_that_ctrl_z_puts_back_with_the_preset_and_the_values_the_reverb_had_and_ctrl_y_loads_again() {
        let mut h = Harness::new(PRESET_SCENE);
        // The reverb, with the preset it has and a value set over it, as they are: what a preset that is put back has to leave as it was.
        let reverb = h.s.ctl.model.instruments[0].effects[0].node;
        h.s.ctl.apply(Intent::SetParam { node: reverb, param: 2, value: 0.9 }).unwrap();
        let shape = |h: &Harness| format!("{:?}", h.s.ctl.model.instruments[0].effects[0]);
        let before = shape(&h);
        h.commands();
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        h.press(KeyCode::Char('p')).type_chars("cathed").press(KeyCode::Enter);
        assert_eq!(h.commands(), ["LoadPreset inst=0 slot=1 id=cathedral"]);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("load preset Cathedral"));
        let loaded = shape(&h);
        assert_ne!(loaded, before);
        ctrl_z(&mut h);
        let commands = h.commands();
        // The preset it had is loaded again, and then the values: the ten of the reverb, the one that was set among them.
        assert_eq!(commands.first().map(String::as_str), Some("LoadPreset inst=0 slot=1 id=plate"), "{commands:?}");
        assert!(commands.contains(&"SetParameter inst=0 slot=1 param=2 value=0.900".to_string()), "{commands:?}");
        assert_eq!(shape(&h), before, "the preset and the values the reverb had, and what they are compared with");
        ctrl_y(&mut h);
        assert_eq!(shape(&h), loaded, "loaded again");
        ctrl_z(&mut h);
        assert_eq!(shape(&h), before);
    }

    #[test]
    fn i_replaces_the_instrument_as_a_step_that_ctrl_z_puts_back_with_its_cable_and_ctrl_y_replaces_again() {
        let mut h = Harness::new(scene_split());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        // The first lane, with its instrument (its values among them) and the cable of its LFO to it, as they are: what a replacement that is put back has to leave as it was.
        let shape = |h: &Harness| format!("{:?}", h.s.ctl.model.instruments[0]);
        h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        h.s.ctl.model.instruments[0].instrument.as_mut().unwrap().params[2].value = 0.25; // volume, edited
        let before = shape(&h);
        h.commands();
        h.goto(TreeAddress::Instrument(0));
        h.press(KeyCode::Char('i')).press(KeyCode::Enter);
        assert_ne!(shape(&h), before, "another instrument is in the lane, and the cable is gone");
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("change the instrument to Oscillator"));
        // (The commands are read: the audio thread, which the test has not, would have handed the old instrument back, so that it is loaded again from the model.)
        h.commands();
        ctrl_z(&mut h);
        let commands = h.commands();
        assert_eq!(commands[0], "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false", "{commands:?}");
        assert!(commands[1].starts_with("InsertModTarget inst=0 mod=0 target=0"), "the cable comes back after the instrument: {commands:?}");
        assert_eq!(shape(&h), before, "the instrument is back, with the value that was set and the cable that drove it");
        ctrl_y(&mut h);
        assert_ne!(shape(&h), before, "replaced again");
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(shape(&h), before);
    }

    #[test]
    fn a_lane_that_a_browser_takes_out_with_its_instrument_is_put_back_by_ctrl_z() {
        let mut h = Harness::new(scene_group());
        h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        // The ungrouped bass, the last lane, with its instrument: taken out by an intent, as a browser asks for it.
        let bass = h.s.ctl.model.instruments[2].node;
        let before = format!("{:?}", h.s.ctl.model.instruments[2]);
        h.commands();
        h.s.ctl.apply(Intent::RemoveNode { node: bass }).unwrap();
        h.commands();
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("remove the lane of Oscillator"));
        ctrl_z(&mut h);
        assert_eq!(format!("{:?}", h.s.ctl.model.instruments[2]), before);
        assert!(h.commands().iter().any(|command| command == "InsertInstrument index=2 plugins=2"));
    }

    #[test]
    fn ctrl_z_is_not_heard_while_a_value_is_being_typed_and_does_not_undo_what_was_not_set() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.press(KeyCode::Enter).press(KeyCode::Left);
        h.commands();
        // The value popup is open: a key goes to it, and what was set is not put back behind it.
        h.press(KeyCode::Enter);
        assert!(h.s.editing.is_some());
        ctrl_z(&mut h);
        assert!(h.commands().is_empty());
        assert_eq!(cutoff_of_the_bus_filter(&h), 0.95);
    }
}
