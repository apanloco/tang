//! The names of the things in a session: what the model, the engine and the front-ends call a node and a cable by.
//!
//! They are here and not in the model so that the engine can hold them (P2 of `docs/patch-graph.md` §11.6) without reaching into a module that imports the engine. The model
//! hands them out (`app::model::NodeIds`) and re-exports them, so that nothing that names `app::model::NodeId` changes.

/// Names a node of the session (an instrument, an effect, a group, a bus effect
/// or a modulator) for as long as it exists. The engine and the tree address
/// nodes by position, which shifts when others come and go; a `NodeId` does not.
/// Allocated by the model, never reused during a run, not saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct NodeId(pub u32);

impl NodeId {
    /// The master mix, where every ungrouped lane and every group ends up. It is a
    /// node of the graph a client gets but not of the model: nothing is allocated
    /// with this id, and `SessionModel::locate` does not find it.
    pub const MASTER: NodeId = NodeId(u32::MAX);
}

/// Names a modulation edge, one target of one modulator, for as long as it
/// exists: the cable a client draws from the modulator to what it drives. Edges
/// come and go as targets are bound and removed, and a target is addressed by its
/// position in its modulator, which shifts; an `EdgeId` does not. Allocated by the
/// model, never reused during a run, not saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct EdgeId(pub u32);
