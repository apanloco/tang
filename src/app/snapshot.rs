//! The whole state a client needs to start from: `Controller::snapshot`; see
//! `docs/web-ui.md` §7.
//!
//! A client that connects (or that sees a gap in the revisions of the events) asks
//! for a snapshot and then applies the events that follow. It is the graph of the
//! session (`app::graph`), what the telemetry slots mean (`app::layout`), and the
//! few things that belong to the session as a whole rather than to a node.

#![cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime sends snapshots"))]

use serde::{Deserialize, Serialize};

use crate::app::controller::Controller;
use crate::app::graph::Graph;
use crate::app::layout::TelemetryLayout;
use crate::piano_filter::PianoMode;

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
pub struct Meta {
    /// The session has changes that are not saved.
    pub dirty: bool,
    /// The name of the file `Ctrl+S` writes, if it has one. Not the path: a client
    /// on another machine has no use for it.
    pub file: Option<String>,
    /// The tempo patterns are recorded and played at.
    pub bpm: f32,
    pub piano: PianoMeta,
}

/// The Piano tab's scale and mode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct PianoMeta {
    /// As it is saved in the session file: `C major`, `F# dorian`.
    pub scale: String,
    /// Off-scale notes are dropped (Locked mode), as opposed to just coloured.
    pub locked: bool,
}

impl Controller {
    /// The session as it is now.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            rev: self.rev,
            meta: Meta {
                dirty: self.dirty,
                file: self.session_path.as_ref().and_then(|path| path.file_name()).map(|name| name.to_string_lossy().into_owned()),
                bpm: self.global_bpm,
                piano: PianoMeta {
                    scale: self.piano_filter.scale().short(),
                    locked: matches!(self.piano_filter.mode(), PianoMode::Locked),
                },
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
    use crate::app::intent::Intent;
    use crate::app::testutil::{GROUPS, load, stub_engine};
    use crate::scale::ScaleSetting;

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
            Meta { dirty: false, file: None, bpm: 120.0, piano: PianoMeta { scale: "C major".to_string(), locked: false } }
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
        assert_eq!(meta.piano, PianoMeta { scale: "F# dorian".to_string(), locked: true });
    }

    /// The snapshot of `examples/tracer.toml` as JSON text, with the layout's
    /// fingerprint blanked: it comes from the hasher, which the standard library
    /// does not promise to keep the same from one release to the next.
    fn tracer_json() -> String {
        let ctl = controller(include_str!("../../examples/tracer.toml"), Some("/home/someone/tracer.toml"));
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

    #[test]
    fn a_snapshot_is_camel_case_json() {
        let ctl = controller("[[instrument]]\nplugin = \"builtin:osc\"\n", Some("a.toml"));
        let snapshot = ctl.snapshot();
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(json["rev"], json!(0));
        assert_eq!(
            json["meta"],
            json!({"dirty": false, "file": "a.toml", "bpm": 120.0, "piano": {"scale": "C major", "locked": false}})
        );
        assert_eq!(json["graph"]["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(json["telemetry"]["meters"].as_array().unwrap().len(), 4);
        assert_eq!(serde_json::from_value::<Snapshot>(json).unwrap(), snapshot);
    }
}
