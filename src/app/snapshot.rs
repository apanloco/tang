//! The whole state a client needs to start from: `Controller::snapshot`; see
//! `docs/web-ui.md` §7.
//!
//! A client that connects (or that sees a gap in the revisions of the events) asks
//! for a snapshot and then applies the events that follow. It is the graph of the
//! session (`app::graph`), what the telemetry slots mean (`app::layout`), and the
//! few things that belong to the session as a whole rather than to a node.

#![cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime sends snapshots"))]

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::app::controller::Controller;
use crate::app::graph::{EdgeKind, Graph, ModulatorSource, Node, NodeKind, Plugin, Waveform};
use crate::app::intent::{Applied, Event, Gain, ModulatorField};
use crate::app::layout::TelemetryLayout;
use crate::app::model::NodeId;
use crate::plugin::chain::LfoWaveform;
use crate::piano_filter::PianoMode;
use crate::scale::ScaleSetting;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct Snapshot {
    /// The revision of the session this is a picture of: the events after it have
    /// a higher one.
    pub rev: u64,
    pub meta: Meta,
    pub graph: Graph,
    pub telemetry: TelemetryLayout,
}

/// What belongs to the session as a whole.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    /// The session has changes that are not saved.
    pub dirty: bool,
    /// The name of the file `Ctrl+S` writes, if it has one. Not the path: a client
    /// on another machine has no use for it.
    pub file: Option<String>,
    /// The tempo patterns are recorded and played at.
    pub bpm: f32,
    /// The sample rate the engine runs at, in Hz: what a client needs to put a frequency on what it draws of the output
    /// (the scope's spectrum). It does not change while the program runs.
    pub sample_rate: f32,
    pub piano: PianoMeta,
}

/// The Piano tab's scale and mode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct PianoMeta {
    /// As it is saved in the session file: `C major`, `F# dorian`.
    pub scale: String,
    /// The scale's root, a pitch class: 0 C to 11 B. (A client draws the scale from this and `notes`, and knows no scale.)
    pub root: u8,
    /// The short name of the kind of scale: `major`, `dorian`, `harm minor`.
    pub kind: String,
    /// The pitch classes of the scale, lowest first.
    pub notes: Vec<u8>,
    /// Off-scale notes are dropped (Locked mode), as opposed to just coloured.
    pub locked: bool,
}

impl PianoMeta {
    pub fn new(scale: ScaleSetting, locked: bool) -> Self {
        PianoMeta { scale: scale.short(), root: scale.root, kind: scale.kind().to_string(), notes: scale.pitch_classes(), locked }
    }
}

/// An event that does not fit the snapshot it was applied to. The copy is not the
/// session's any more: whoever holds it starts again from a fresh snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoesNotFit(String);

impl fmt::Display for DoesNotFit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DoesNotFit {}

impl Snapshot {
    /// The snapshot after the intent that `applied` describes: what a client does
    /// with the events that follow the snapshot it started from. The rules are those
    /// of `Controller::apply`, read from the other side, and the web client's
    /// `applyEvent` (`web/src/client/reducers.ts`) follows them too.
    ///
    /// An intent is the next revision, or one this snapshot already has (nothing to
    /// do: a snapshot asked for later may include what was told before it). Anything
    /// else, or an event about what the snapshot lacks, does not fit, and the
    /// snapshot is as it was before the event that did not.
    pub fn apply(&mut self, applied: &Applied) -> Result<(), DoesNotFit> {
        if applied.rev <= self.rev {
            return Ok(());
        }
        if applied.rev != self.rev + 1 {
            return Err(DoesNotFit(format!("the session is at revision {} and the next event is {}", self.rev, applied.rev)));
        }
        if applied.restructured {
            // A change of structure is told as a snapshot (`Controller::apply`), which is the only
            // thing that shows it: the copy has to be replaced, not changed.
            return Err(DoesNotFit("the structure changed: start again from a snapshot".to_string()));
        }
        for event in &applied.events {
            self.change(event)?;
        }
        self.rev = applied.rev;
        // An intent, from whichever front-end, is a change the session has not saved, unless it is a save.
        self.meta.dirty = !applied.events.iter().any(|event| matches!(event, Event::Saved { .. }));
        Ok(())
    }

    fn change(&mut self, event: &Event) -> Result<(), DoesNotFit> {
        match event {
            Event::ParamSet { node, param, value } => {
                let plugin = self.plugin_mut(*node)?;
                let row = plugin
                    .params
                    .iter_mut()
                    .find(|p| p.index == *param)
                    .ok_or_else(|| DoesNotFit(format!("node {} has no parameter {param}", node.0)))?;
                row.value = *value;
            }
            Event::GainSet { node, gain, value } => {
                let target = match (gain, &mut self.node_mut(*node)?.kind) {
                    (Gain::Volume, NodeKind::Instrument { volume, .. }) => volume,
                    (Gain::Mix, NodeKind::Effect { mix, .. }) => mix,
                    (Gain::GroupVolume, NodeKind::Group { volume, .. }) => volume,
                    (Gain::GroupPan, NodeKind::Group { pan, .. }) => pan,
                    _ => return Err(DoesNotFit(format!("{gain:?} does not apply to node {}", node.0))),
                };
                *target = *value;
            }
            Event::RangeSet { node, range } => {
                let NodeKind::Instrument { range: keys, .. } = &mut self.node_mut(*node)?.kind else {
                    return Err(DoesNotFit(format!("node {} is not a lane", node.0)));
                };
                *keys = *range;
            }
            Event::NodeUiSet { node, ui } => {
                self.node_mut(*node)?.ui = *ui;
            }
            Event::PinnedSet { node, params } => {
                let plugin = self.plugin_mut(*node)?;
                if let Some(missing) = params.iter().find(|index| plugin.params.iter().all(|p| p.index != **index)) {
                    return Err(DoesNotFit(format!("node {} has no parameter {missing}", node.0)));
                }
                plugin.pinned = params.clone();
            }
            Event::GroupRenamed { node, name } => {
                let group = self.node_mut(*node)?;
                if !matches!(group.kind, NodeKind::Group { .. }) {
                    return Err(DoesNotFit(format!("node {} is not a group", node.0)));
                }
                group.name.clone_from(name);
            }
            Event::PresetLoaded { node, preset, values } => {
                let plugin = self.plugin_mut(*node)?;
                // All of it or none: look before changing anything.
                if values.iter().any(|v| plugin.params.iter().all(|p| p.index != v.param)) {
                    return Err(DoesNotFit(format!("the preset names a parameter node {} does not have", node.0)));
                }
                plugin.current_preset = Some(preset.clone());
                for v in values {
                    for row in plugin.params.iter_mut().filter(|p| p.index == v.param) {
                        // The controller keeps a preset's values as the baseline too, and
                        // the snapshot shows the baseline as the parameter's default.
                        row.value = v.value;
                        row.default = v.value;
                    }
                }
            }
            Event::ModulatorFieldSet { node, field, value } => {
                let NodeKind::Modulator { source } = &mut self.node_mut(*node)?.kind else {
                    return Err(DoesNotFit(format!("node {} is not a modulator", node.0)));
                };
                match (source, field) {
                    (ModulatorSource::Lfo { rate, .. }, ModulatorField::Rate) => *rate = *value,
                    (ModulatorSource::Lfo { waveform, .. }, ModulatorField::Waveform) => {
                        let index = *value as usize;
                        let known = (*value >= 0.0 && value.fract() == 0.0).then(|| LfoWaveform::ALL.get(index)).flatten();
                        *waveform = Waveform::from(*known.ok_or_else(|| DoesNotFit(format!("{value} is not a waveform")))?);
                    }
                    (ModulatorSource::Envelope { attack, .. }, ModulatorField::Attack) => *attack = *value,
                    (ModulatorSource::Envelope { decay, .. }, ModulatorField::Decay) => *decay = *value,
                    (ModulatorSource::Envelope { sustain, .. }, ModulatorField::Sustain) => *sustain = *value,
                    (ModulatorSource::Envelope { release, .. }, ModulatorField::Release) => *release = *value,
                    _ => return Err(DoesNotFit(format!("{field:?} does not apply to node {}", node.0))),
                }
            }
            Event::EdgeDepthSet { edge, depth } => {
                let amount = self
                    .graph
                    .edges
                    .iter_mut()
                    .find_map(|e| match &mut e.kind {
                        EdgeKind::Modulation { id, amount } if id == edge => Some(amount),
                        _ => None,
                    })
                    .ok_or_else(|| DoesNotFit(format!("there is no cable {}", edge.0)))?;
                *amount = *depth;
            }
            Event::ScaleSet { scale, root, kind, notes } => {
                let piano = &mut self.meta.piano;
                piano.scale.clone_from(scale);
                piano.root = *root;
                piano.kind.clone_from(kind);
                piano.notes.clone_from(notes);
            }
            Event::LockedSet { locked } => self.meta.piano.locked = *locked,
            Event::Saved { file } => self.meta.file.clone_from(file),
        }
        Ok(())
    }

    fn node_mut(&mut self, id: NodeId) -> Result<&mut Node, DoesNotFit> {
        self.graph.nodes.iter_mut().find(|n| n.id == id).ok_or_else(|| DoesNotFit(format!("there is no node {}", id.0)))
    }

    /// The plugin of an instrument or effect node.
    fn plugin_mut(&mut self, id: NodeId) -> Result<&mut Plugin, DoesNotFit> {
        match &mut self.node_mut(id)?.kind {
            NodeKind::Instrument { plugin: Some(plugin), .. } | NodeKind::Effect { plugin, .. } => Ok(plugin),
            _ => Err(DoesNotFit(format!("node {} has no plugin", id.0))),
        }
    }
}

impl Controller {
    /// The session as it is now.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            rev: self.rev,
            meta: Meta {
                dirty: self.dirty,
                file: self.session_file_name(),
                bpm: self.global_bpm,
                sample_rate: self.sample_rate,
                piano: PianoMeta::new(self.piano_filter.scale(), matches!(self.piano_filter.mode(), PianoMode::Locked)),
            },
            graph: self.model.graph(),
            telemetry: self.model.telemetry_layout(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::app::graph::{EdgeKind, ModulatorSource, Node, NodeKind, NoteRange, Plugin};
    use crate::app::intent::{Applied, Event, Gain, Intent, ModulatorField, ParamValue};
    use crate::app::model::{EdgeId, NodeId};
    use crate::app::testutil::{GROUPS, load, stub_engine};
    use crate::scale::ScaleSetting;
    use crate::session::NodeUi;

    fn controller(session: &str, path: Option<&str>) -> Controller {
        let model = load(session).model;
        let (handles, _engine) = stub_engine();
        Controller::new(model, handles, path.map(PathBuf::from))
    }

    #[test]
    fn a_fresh_controllers_snapshot_is_its_graph_and_its_layout_and_nothing_changed() {
        let ctl = controller(GROUPS, None);
        let snapshot = ctl.snapshot();
        assert_eq!(snapshot.rev, 0);
        assert_eq!(snapshot.graph, ctl.model.graph());
        assert_eq!(snapshot.telemetry, ctl.model.telemetry_layout());
        assert_eq!(
            snapshot.meta,
            Meta {
                dirty: false,
                file: None,
                bpm: 120.0,
                sample_rate: 48_000.0,
                piano: PianoMeta { scale: "C major".to_string(), root: 0, kind: "major".to_string(), notes: vec![0, 2, 4, 5, 7, 9, 11], locked: false },
            }
        );
    }

    #[test]
    fn an_applied_intent_is_in_the_next_snapshot() {
        let mut ctl = controller(GROUPS, None);
        let lane = ctl.model.instruments[0].node;
        let before = ctl.snapshot();
        let applied = ctl.apply(Intent::SetParam { node: lane, param: 1, value: 3.0 }).unwrap();
        let after = ctl.snapshot();
        assert_eq!((after.rev, applied.rev), (1, 1));
        assert!(after.meta.dirty);
        assert_ne!(after.graph, before.graph);
        assert_eq!(after.graph, ctl.model.graph());
        assert_eq!(after.telemetry, before.telemetry, "a value is not a change of layout");
    }

    #[test]
    fn the_session_is_known_by_its_file_name() {
        let ctl = controller(GROUPS, Some("/home/someone/sessions/jam.toml"));
        assert_eq!(ctl.snapshot().meta.file.as_deref(), Some("jam.toml"));
        let ctl = controller(GROUPS, Some("jam.toml"));
        assert_eq!(ctl.snapshot().meta.file.as_deref(), Some("jam.toml"));
    }

    #[test]
    fn the_tempo_and_the_piano_are_in_the_snapshot() {
        let mut ctl = controller(GROUPS, None);
        ctl.global_bpm = 93.5;
        ctl.piano_filter.set_scale(ScaleSetting::parse("F# dorian").unwrap());
        ctl.piano_filter.set_mode(PianoMode::Locked);
        let meta = ctl.snapshot().meta;
        assert_eq!(meta.bpm, 93.5);
        assert_eq!(
            meta.piano,
            PianoMeta { scale: "F# dorian".to_string(), root: 6, kind: "dorian".to_string(), notes: vec![1, 3, 4, 6, 8, 9, 11], locked: true }
        );
    }

    #[test]
    fn the_sample_rate_is_in_the_snapshot() {
        let mut ctl = controller(GROUPS, None);
        assert_eq!(ctl.snapshot().meta.sample_rate, 48_000.0, "the stub engine's");
        ctl.sample_rate = 44_100.0;
        assert_eq!(ctl.snapshot().meta.sample_rate, 44_100.0, "the engine's, as it is now");
    }

    /// The snapshot of the tracer scene (`web/src/client/fixtures/tracer.toml`) as JSON text, with the layout's
    /// fingerprint blanked: it comes from the hasher, which the standard library
    /// does not promise to keep the same from one release to the next.
    fn tracer_json() -> String {
        let ctl = controller(include_str!("../../web/src/client/fixtures/tracer.toml"), Some("/home/someone/tracer.toml"));
        // Through the text the wire carries, so that the numbers read as they do there
        // (an f32 as its shortest text, not widened to an f64).
        let wire = serde_json::to_string(&ctl.snapshot()).unwrap();
        let mut json: serde_json::Value = serde_json::from_str(&wire).unwrap();
        json["telemetry"]["rev"] = json!("(fingerprint)");
        serde_json::to_string_pretty(&json).unwrap()
    }

    #[test]
    fn the_tracer_sessions_snapshot_is_what_it_was() {
        // The contract with the browser: if this changes, the TypeScript and the
        // web client have to change with it.
        insta::assert_snapshot!("snapshot_tracer", tracer_json());
    }

    /// The tracer's snapshot as the web client's mock engine serves it (and its tests read
    /// it): the file `web/src/client/fixtures/tracer.snapshot.json`, which this test keeps
    /// current. `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_the_tracer_is_current() {
        let ctl = controller(include_str!("../../web/src/client/fixtures/tracer.toml"), Some("tracer.toml"));
        let wire = serde_json::to_string(&ctl.snapshot()).unwrap();
        let mut json: serde_json::Value = serde_json::from_str(&wire).unwrap();
        // The layout's fingerprint comes from the hasher, which the standard library does not
        // promise to keep the same from one release to the next; the fixture needs some number.
        json["telemetry"]["rev"] = json!(1);
        let fresh = serde_json::to_string_pretty(&json).unwrap() + "\n";

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/tracer.snapshot.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what the tracer's snapshot is now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    #[test]
    fn a_snapshot_is_camel_case_json() {
        let ctl = controller("[[instrument]]\nplugin = \"builtin:osc\"\n", Some("a.toml"));
        let snapshot = ctl.snapshot();
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(json["rev"], json!(0));
        assert_eq!(
            json["meta"],
            json!({
                "dirty": false,
                "file": "a.toml",
                "bpm": 120.0,
                "sampleRate": 48000.0,
                "piano": {"scale": "C major", "root": 0, "kind": "major", "notes": [0, 2, 4, 5, 7, 9, 11], "locked": false},
            })
        );
        assert_eq!(json["graph"]["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(json["telemetry"]["meters"].as_array().unwrap().len(), 4);
        assert_eq!(serde_json::from_value::<Snapshot>(json).unwrap(), snapshot);
    }
    /// A group with a bus filter and an LFO that drives it, a member with a reverb (which
    /// has presets) and an LFO and an envelope, and a lane with no instrument plugin.
    const APPLY: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.modulator]]
type = "lfo"
rate = 2.0
[[group.modulator.target]]
bus = 0
param = "cutoff"
depth = 0.2

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.modulator]]
type = "lfo"
rate = 1.0
[[instrument.modulator.target]]
slot = 1
param = "size"
depth = 0.3
[[instrument.modulator]]
type = "envelope"

[[instrument]]
"#;

    fn find(snapshot: &Snapshot, wanted: impl Fn(&Node) -> bool) -> NodeId {
        snapshot.graph.nodes.iter().find(|n| wanted(n)).map(|n| n.id).expect("a node like that")
    }

    fn plugin_of(snapshot: &Snapshot, node: NodeId) -> &Plugin {
        match &snapshot.graph.nodes.iter().find(|n| n.id == node).unwrap().kind {
            NodeKind::Instrument { plugin: Some(plugin), .. } | NodeKind::Effect { plugin, .. } => plugin,
            other => panic!("{other:?} has no plugin"),
        }
    }

    fn param_named(snapshot: &Snapshot, node: NodeId, name: &str) -> u32 {
        plugin_of(snapshot, node).params.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("no {name}")).index
    }

    /// The nodes of `APPLY` that the tests act on.
    struct Nodes {
        osc: NodeId,
        reverb: NodeId,
        bus: NodeId,
        group: NodeId,
        empty: NodeId,
        lane_lfo: NodeId,
        envelope: NodeId,
        group_lfo: NodeId,
    }

    /// The modulation cables, in the snapshot's order: the group's, then the lane's.
    fn cables(snapshot: &Snapshot) -> Vec<EdgeId> {
        snapshot
            .graph
            .edges
            .iter()
            .filter_map(|e| match e.kind {
                EdgeKind::Modulation { id, .. } => Some(id),
                _ => None,
            })
            .collect()
    }

    fn nodes(snapshot: &Snapshot) -> Nodes {
        let modulator = |parent_is_group: bool, envelope: bool| {
            find(snapshot, |n| {
                let parent = n.parent.and_then(|p| snapshot.graph.nodes.iter().find(|q| q.id == p));
                let in_group = parent.is_some_and(|p| matches!(p.kind, NodeKind::Group { .. }));
                matches!(&n.kind, NodeKind::Modulator { source } if matches!(source, ModulatorSource::Envelope { .. }) == envelope)
                    && in_group == parent_is_group
            })
        };
        let by_plugin = |id: &'static str| move |n: &Node| matches!(&n.kind, NodeKind::Instrument { plugin: Some(p), .. } | NodeKind::Effect { plugin: p, .. } if p.id == id);
        Nodes {
            osc: find(snapshot, by_plugin("builtin:osc")),
            reverb: find(snapshot, by_plugin("builtin:reverb")),
            bus: find(snapshot, by_plugin("builtin:filter")),
            group: find(snapshot, |n| matches!(n.kind, NodeKind::Group { .. })),
            empty: find(snapshot, |n| matches!(n.kind, NodeKind::Instrument { plugin: None, .. })),
            lane_lfo: modulator(false, false),
            envelope: modulator(false, true),
            group_lfo: modulator(true, false),
        }
    }

    fn applied(rev: u64, event: Event) -> Applied {
        Applied::of(rev, vec![event])
    }

    #[test]
    fn the_events_of_an_intent_are_everything_it_changed() {
        // What a client does: start from a snapshot and apply the events. It has to end
        // up where the controller is, whatever the intent was.
        let mut ctl = controller(APPLY, None);
        let mut told = ctl.snapshot();
        let n = nodes(&told);
        let detune = param_named(&told, n.osc, "detune");
        let waveform = param_named(&told, n.osc, "waveform");
        let size = param_named(&told, n.reverb, "size");
        let cutoff = param_named(&told, n.bus, "cutoff");
        let intents = [
            Intent::SetParam { node: n.osc, param: detune, value: 3.0 },
            Intent::SetParam { node: n.reverb, param: size, value: 0.25 },
            Intent::SetParam { node: n.bus, param: cutoff, value: 0.125 },
            Intent::SetParam { node: n.osc, param: waveform, value: 1.75 },
            Intent::SetParam { node: n.osc, param: detune, value: 1.0e9 },
            Intent::SetGain { node: n.osc, gain: Gain::Volume, value: 2.5 },
            Intent::SetGain { node: n.reverb, gain: Gain::Mix, value: 0.25 },
            Intent::SetGain { node: n.bus, gain: Gain::Mix, value: 0.75 },
            Intent::SetGain { node: n.group, gain: Gain::GroupVolume, value: 1.5 },
            Intent::SetGain { node: n.group, gain: Gain::GroupPan, value: -0.5 },
            Intent::LoadPreset { node: n.reverb, preset: "cathedral".to_string() },
            Intent::SetParam { node: n.reverb, param: size, value: 0.5 },
            Intent::SetModulatorField { node: n.lane_lfo, field: ModulatorField::Rate, value: 4.0 },
            Intent::SetModulatorField { node: n.lane_lfo, field: ModulatorField::Waveform, value: 2.4 },
            Intent::SetModulatorField { node: n.group_lfo, field: ModulatorField::Rate, value: 1.0e9 },
            Intent::SetModulatorField { node: n.envelope, field: ModulatorField::Attack, value: 0.5 },
            Intent::SetModulatorField { node: n.envelope, field: ModulatorField::Decay, value: 1.5 },
            Intent::SetModulatorField { node: n.envelope, field: ModulatorField::Sustain, value: 0.25 },
            Intent::SetModulatorField { node: n.envelope, field: ModulatorField::Release, value: 2.0 },
            Intent::SetEdgeDepth { edge: cables(&told)[0], depth: -0.5 },
            Intent::SetEdgeDepth { edge: cables(&told)[1], depth: 9.0 },
            Intent::SetRange { node: n.osc, range: Some(NoteRange { low: 36, high: 84 }) },
            Intent::SetRange { node: n.empty, range: Some(NoteRange { low: 0, high: 59 }) },
            Intent::SetRange { node: n.osc, range: None },
            Intent::SetScale { scale: "F# dorian".to_string() },
            Intent::SetLocked { locked: true },
            Intent::SetScale { scale: "Bb Natural Minor".to_string() },
            Intent::SetLocked { locked: false },
            Intent::SetNodeUi { node: n.osc, ui: Some(NodeUi { x: 80.0, y: 120.0 }) },
            Intent::SetNodeUi { node: NodeId::MASTER, ui: Some(NodeUi { x: 1200.0, y: -40.5 }) },
            Intent::SetNodeUi { node: n.osc, ui: Some(NodeUi { x: 80.0, y: 140.0 }) },
            Intent::SetNodeUi { node: n.reverb, ui: Some(NodeUi { x: 1.0e9, y: 0.0 }) },
            Intent::SetNodeUi { node: n.osc, ui: None },
            Intent::SetPinned { node: n.osc, params: vec![detune, waveform] },
            Intent::SetPinned { node: n.reverb, params: vec![size] },
            Intent::SetPinned { node: n.bus, params: vec![cutoff] },
            Intent::SetPinned { node: n.osc, params: vec![waveform, detune] },
            Intent::SetPinned { node: n.osc, params: vec![] },
            Intent::RenameGroup { node: n.group, name: "Strings".to_string() },
            Intent::RenameGroup { node: n.group, name: "  ".to_string() },
        ];
        for intent in intents {
            let before = told.clone();
            let applied = ctl.apply(intent.clone()).unwrap();
            told.apply(&applied).unwrap_or_else(|e| panic!("{intent:?}: {e}"));
            assert_ne!(told, before, "{intent:?} changed something");
            assert_eq!(told, ctl.snapshot(), "after {intent:?}");
        }
        assert_eq!(told.rev, 40);
    }

    #[test]
    fn a_client_is_told_of_a_random_waveform_and_ends_up_where_the_controller_is() {
        let mut ctl = controller(APPLY, None);
        let mut told = ctl.snapshot();
        let lfo = nodes(&told).lane_lfo;
        let waveform_of = |snapshot: &Snapshot| match &snapshot.graph.nodes.iter().find(|node| node.id == lfo).unwrap().kind {
            NodeKind::Modulator { source: ModulatorSource::Lfo { waveform, .. } } => *waveform,
            other => panic!("{other:?} is not an LFO"),
        };
        for (value, waveform) in [(4.0, Waveform::Random), (5.0, Waveform::Smooth)] {
            let applied = ctl.apply(Intent::SetModulatorField { node: lfo, field: ModulatorField::Waveform, value }).unwrap();
            told.apply(&applied).unwrap();
            assert_eq!(waveform_of(&told), waveform);
            assert_eq!(told, ctl.snapshot());
        }
    }

    #[test]
    fn what_undo_and_redo_do_is_events_a_client_applies_as_it_does_any_others() {
        // A client that starts from a snapshot and applies what Undo and Redo bring ends up where the controller is: back at the
        // start after as many undos as there were changes, and where it was after as many redos.
        let mut ctl = controller(APPLY, None);
        let start = ctl.snapshot();
        let mut told = start.clone();
        let n = nodes(&told);
        let detune = param_named(&told, n.osc, "detune");
        let cutoff = param_named(&told, n.bus, "cutoff");
        let intents = [
            Intent::SetParam { node: n.osc, param: detune, value: 3.0 },
            Intent::SetParam { node: n.bus, param: cutoff, value: 0.125 },
            Intent::SetGain { node: n.group, gain: Gain::GroupPan, value: -0.5 },
            Intent::SetModulatorField { node: n.lane_lfo, field: ModulatorField::Waveform, value: 2.0 },
            Intent::SetModulatorField { node: n.envelope, field: ModulatorField::Sustain, value: 0.25 },
            Intent::SetEdgeDepth { edge: cables(&told)[0], depth: -0.5 },
            Intent::SetRange { node: n.osc, range: Some(NoteRange { low: 36, high: 84 }) },
            Intent::SetScale { scale: "F# dorian".to_string() },
            Intent::SetLocked { locked: true },
            Intent::SetNodeUi { node: n.osc, ui: Some(NodeUi { x: 80.0, y: 120.0 }) },
            Intent::SetPinned { node: n.osc, params: vec![detune] },
            Intent::RenameGroup { node: n.group, name: "Strings".to_string() },
        ];
        let apply = |ctl: &mut Controller, told: &mut Snapshot, intent: Intent| {
            let applied = ctl.apply(intent.clone()).unwrap_or_else(|e| panic!("{intent:?}: {e}"));
            told.apply(&applied).unwrap_or_else(|e| panic!("{intent:?}: {e}"));
            assert_eq!(*told, ctl.snapshot(), "after {intent:?}");
        };
        for intent in intents.clone() {
            apply(&mut ctl, &mut told, intent);
        }
        let done = told.clone();
        for _ in &intents {
            apply(&mut ctl, &mut told, Intent::Undo);
        }
        assert_eq!(told.graph, start.graph, "everything is as it was");
        assert_eq!(told.meta.piano, start.meta.piano);
        assert_ne!(told.graph, done.graph);
        for _ in &intents {
            apply(&mut ctl, &mut told, Intent::Redo);
        }
        assert_eq!(told.graph, done.graph, "and everything is as it was done");
        assert_eq!(told.meta.piano, done.meta.piano);
    }

    #[test]
    fn an_applied_event_leaves_the_session_dirty() {
        let mut ctl = controller(APPLY, None);
        let mut told = ctl.snapshot();
        assert!(!told.meta.dirty);
        let osc = nodes(&told).osc;
        let applied = ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 0.5 }).unwrap();
        told.apply(&applied).unwrap();
        assert!(told.meta.dirty, "an intent was applied");
        assert_eq!(told.meta, ctl.snapshot().meta);
    }

    #[test]
    fn a_save_leaves_the_session_with_nothing_unsaved_and_the_snapshot_that_is_told_so_is_the_one_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.toml");
        let mut ctl = controller(APPLY, Some(path.to_str().unwrap()));
        let mut told = ctl.snapshot();
        let osc = nodes(&told).osc;
        let applied = ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 0.5 }).unwrap();
        told.apply(&applied).unwrap();
        assert!(told.meta.dirty);
        let saved = ctl.apply(Intent::Save).unwrap();
        told.apply(&saved).unwrap();
        assert!(!told.meta.dirty, "it is saved now");
        assert_eq!(told.meta.file.as_deref(), Some("song.toml"));
        assert_eq!(told, ctl.snapshot());
        // And an edit after it is unsaved again.
        let applied = ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 1.5 }).unwrap();
        told.apply(&applied).unwrap();
        assert!(told.meta.dirty);
        assert_eq!(told, ctl.snapshot());
    }

    #[test]
    fn a_saved_event_names_the_file_the_session_is_saved_to() {
        // (Save As will make it another one: the event is the whole of what the client is told.)
        let ctl = controller(APPLY, Some("/somewhere/song.toml"));
        let mut told = ctl.snapshot();
        assert_eq!(told.meta.file.as_deref(), Some("song.toml"));
        told.apply(&applied(1, Event::Saved { file: Some("other.toml".to_string()) })).unwrap();
        assert_eq!((told.meta.file.as_deref(), told.meta.dirty, told.rev), (Some("other.toml"), false, 1));
        told.apply(&applied(2, Event::Saved { file: None })).unwrap();
        assert_eq!(told.meta.file, None, "a session with no file, saved: it says so");
    }

    #[test]
    fn a_preset_makes_the_values_it_sets_the_parameters_baseline() {
        // The controller keeps a preset's values as the baseline too (what is not saved
        // as the user's edit), and the snapshot shows the baseline as the default.
        let mut ctl = controller(APPLY, None);
        let mut told = ctl.snapshot();
        let reverb = nodes(&told).reverb;
        let applied = ctl.apply(Intent::LoadPreset { node: reverb, preset: "cathedral".to_string() }).unwrap();
        told.apply(&applied).unwrap();
        let plugin = plugin_of(&told, reverb);
        assert_eq!(plugin.current_preset.as_deref(), Some("Cathedral"));
        assert!(plugin.params.iter().any(|p| p.default != 0.0 && p.default == p.value));
        assert!(plugin.params.iter().all(|p| p.default == p.value), "every parameter of a reverb is in the preset");
    }

    #[test]
    fn an_event_the_snapshot_has_already_is_nothing_to_do() {
        let mut ctl = controller(APPLY, None);
        let mut told = ctl.snapshot();
        let osc = nodes(&told).osc;
        let detune = param_named(&told, osc, "detune");
        let event = applied(0, Event::ParamSet { node: osc, param: detune, value: 5.0 });
        let before = told.clone();
        told.apply(&event).unwrap();
        assert_eq!(told, before, "revision 0 is the one the snapshot is of");

        let real = ctl.apply(Intent::SetParam { node: osc, param: detune, value: 5.0 }).unwrap();
        told.apply(&real).unwrap();
        let once = told.clone();
        told.apply(&real).unwrap();
        assert_eq!(told, once, "the second time is a repeat");
    }

    #[test]
    fn an_intent_that_changed_the_structure_does_not_fit_what_it_changed() {
        let mut snapshot = controller("[[instrument]]\nplugin = \"builtin:osc\"\n", None).snapshot();
        let before = snapshot.clone();
        let error = snapshot.apply(&Applied::restructured(1)).unwrap_err();
        assert!(error.to_string().contains("snapshot"), "{error}");
        assert_eq!(snapshot, before, "nothing is applied");
        // (An intent the snapshot has already is nothing to do, whatever it was.)
        assert!(snapshot.apply(&Applied::restructured(0)).is_ok());
    }

    #[test]
    fn an_event_that_skips_a_revision_does_not_fit() {
        let ctl = controller(APPLY, None);
        let mut told = ctl.snapshot();
        let osc = nodes(&told).osc;
        let before = told.clone();
        let error = told.apply(&applied(2, Event::GainSet { node: osc, gain: Gain::Volume, value: 0.5 })).unwrap_err();
        assert_eq!(error.to_string(), "the session is at revision 0 and the next event is 2");
        assert_eq!(told, before);
    }

    #[test]
    fn an_event_that_does_not_fit_what_it_names_is_refused_and_changes_nothing() {
        let ctl = controller(APPLY, None);
        let mut told = ctl.snapshot();
        let n = nodes(&told);
        let before = told.clone();
        let ghost = NodeId(9_999);
        let refused = [
            (Event::GainSet { node: ghost, gain: Gain::Volume, value: 0.5 }, "there is no node 9999"),
            (Event::ParamSet { node: ghost, param: 0, value: 0.5 }, "there is no node 9999"),
            (Event::ParamSet { node: n.empty, param: 0, value: 0.5 }, "has no plugin"),
            (Event::ParamSet { node: n.group, param: 0, value: 0.5 }, "has no plugin"),
            (Event::ParamSet { node: n.osc, param: 4_000, value: 0.5 }, "has no parameter 4000"),
            (Event::GainSet { node: n.osc, gain: Gain::Mix, value: 0.5 }, "does not apply"),
            (Event::GainSet { node: n.reverb, gain: Gain::Volume, value: 0.5 }, "does not apply"),
            (Event::GainSet { node: n.osc, gain: Gain::GroupPan, value: 0.5 }, "does not apply"),
            (Event::GainSet { node: n.group, gain: Gain::Mix, value: 0.5 }, "does not apply"),
            (Event::PresetLoaded { node: n.empty, preset: "x".to_string(), values: vec![] }, "has no plugin"),
            (Event::ModulatorFieldSet { node: ghost, field: ModulatorField::Rate, value: 1.0 }, "there is no node 9999"),
            (Event::ModulatorFieldSet { node: n.osc, field: ModulatorField::Rate, value: 1.0 }, "is not a modulator"),
            (Event::ModulatorFieldSet { node: n.group, field: ModulatorField::Rate, value: 1.0 }, "is not a modulator"),
            (Event::ModulatorFieldSet { node: n.lane_lfo, field: ModulatorField::Attack, value: 1.0 }, "does not apply"),
            (Event::ModulatorFieldSet { node: n.envelope, field: ModulatorField::Rate, value: 1.0 }, "does not apply"),
            (Event::ModulatorFieldSet { node: n.envelope, field: ModulatorField::Waveform, value: 1.0 }, "does not apply"),
            (Event::ModulatorFieldSet { node: n.lane_lfo, field: ModulatorField::Waveform, value: 6.0 }, "6 is not a waveform"),
            (Event::ModulatorFieldSet { node: n.lane_lfo, field: ModulatorField::Waveform, value: 7.0 }, "7 is not a waveform"),
            (Event::ModulatorFieldSet { node: n.lane_lfo, field: ModulatorField::Waveform, value: 1.5 }, "1.5 is not a waveform"),
            (Event::ModulatorFieldSet { node: n.lane_lfo, field: ModulatorField::Waveform, value: -1.0 }, "-1 is not a waveform"),
            (Event::EdgeDepthSet { edge: EdgeId(9_999), depth: 0.5 }, "there is no cable 9999"),
            (Event::NodeUiSet { node: ghost, ui: Some(NodeUi { x: 1.0, y: 2.0 }) }, "there is no node 9999"),
            (Event::PinnedSet { node: ghost, params: vec![] }, "there is no node 9999"),
            (Event::PinnedSet { node: n.empty, params: vec![] }, "has no plugin"),
            (Event::PinnedSet { node: n.group, params: vec![] }, "has no plugin"),
            (Event::PinnedSet { node: n.osc, params: vec![4_000] }, "has no parameter 4000"),
            (Event::GroupRenamed { node: ghost, name: "Pad".to_string() }, "there is no node 9999"),
            (Event::GroupRenamed { node: n.osc, name: "Pad".to_string() }, "is not a group"),
            (Event::GroupRenamed { node: NodeId::MASTER, name: "Pad".to_string() }, "is not a group"),
        ];
        for (event, why) in refused {
            let error = told.apply(&applied(1, event.clone())).unwrap_err().to_string();
            assert!(error.contains(why), "{event:?}: {error}");
            assert_eq!(told, before, "{event:?}");
        }
    }

    #[test]
    fn a_preset_that_names_a_parameter_the_plugin_lacks_is_refused_whole() {
        let ctl = controller(APPLY, None);
        let mut told = ctl.snapshot();
        let reverb = nodes(&told).reverb;
        let size = param_named(&told, reverb, "size");
        let before = told.clone();
        let preset = Event::PresetLoaded {
            node: reverb,
            preset: "Odd".to_string(),
            values: vec![ParamValue { param: size, value: 0.9 }, ParamValue { param: 4_000, value: 0.1 }],
        };
        let error = told.apply(&applied(1, preset)).unwrap_err().to_string();
        assert!(error.contains("a parameter"), "{error}");
        assert_eq!(told, before, "not the half of it that fits");
    }
}
