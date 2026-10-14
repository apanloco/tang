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

use crate::app::intent::{Intent, ModulatorField};
use crate::plugin;
use crate::plugin::chain::{CrossModField, ModTargetKind, ModulatorLocation, group_target_member_mut, member_moves};
use crate::session::NodeUi;
use crate::telemetry::{MeterSlots, Slot, SlotAllocator};
use std::collections::BTreeMap;

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

/// Hands out `NodeId`s and `EdgeId`s, each one once. The two are series of their
/// own.
#[derive(Debug, Default)]
pub struct NodeIds {
    next: u32,
    next_edge: u32,
}

impl NodeIds {
    pub fn alloc(&mut self) -> NodeId {
        let id = NodeId(self.next);
        self.next += 1;
        id
    }

    pub fn alloc_edge(&mut self) -> EdgeId {
        let id = EdgeId(self.next_edge);
        self.next_edge += 1;
        id
    }
}

/// Where a node sits in the model, in the positional terms the audio thread's
/// commands use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodePath {
    /// A lane, with the instrument plugin in it.
    Instrument(usize),
    Effect { inst: usize, index: usize },
    Group(usize),
    GroupEffect { group: usize, index: usize },
    Modulator { at: ModulatorLocation, index: usize },
}

/// What a cable drives, named so that it can be found again once other cables and modulators have gone
/// and the places in the racks have moved: a plugin's parameter (by the node of the plugin), a
/// modulator's rate or a stage of its envelope (by the node of the modulator), or the depth of a cable
/// (by its edge).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Driven {
    Param { plugin: NodeId, index: u32 },
    Setting { modulator: NodeId, field: ModulatorField },
    Depth(EdgeId),
}

/// The modulator, in the same rack, and the field of it that a cable of `kind` drives, if it drives one.
fn sibling_field(kind: &ModTargetKind) -> Option<(usize, CrossModField)> {
    match *kind {
        ModTargetKind::ModulatorRate { mod_index } => Some((mod_index, CrossModField::Rate)),
        ModTargetKind::ModulatorAttack { mod_index } => Some((mod_index, CrossModField::Attack)),
        ModTargetKind::ModulatorDecay { mod_index } => Some((mod_index, CrossModField::Decay)),
        ModTargetKind::ModulatorSustain { mod_index } => Some((mod_index, CrossModField::Sustain)),
        ModTargetKind::ModulatorRelease { mod_index } => Some((mod_index, CrossModField::Release)),
        ModTargetKind::ModulatorDepth { mod_index, target_index } => Some((mod_index, CrossModField::Depth(target_index))),
        ModTargetKind::PluginParam { .. }
        | ModTargetKind::GroupMember { .. }
        | ModTargetKind::GroupBus { .. }
        | ModTargetKind::GroupMemberMod { .. } => None,
    }
}

/// What a front-end edits: the instrument lanes and the submix groups.
#[derive(Debug, Default)]
pub struct SessionModel {
    pub instruments: Vec<InstrumentNode>,
    pub groups: Vec<GroupNode>,
    pub ids: NodeIds,
    /// Hands out the telemetry slots the nodes publish into.
    pub slots: SlotAllocator,
    /// Where a front-end draws the nodes it has been told a place for, by node: a side table, since the engine and the
    /// TUI have no use for it and a node does not need one. The master is under `NodeId::MASTER`. An id is never
    /// reused, so what is left here of a node that has gone is never read: `graph()` and the save look up the nodes
    /// there are.
    pub ui: BTreeMap<NodeId, NodeUi>,
}

impl SessionModel {
    /// Where the master mix is drawn, if it has been placed.
    pub fn master_ui(&self) -> Option<NodeUi> {
        self.ui.get(&NodeId::MASTER).copied()
    }

    /// A `NodeId` no other node has had.
    pub fn alloc_node(&mut self) -> NodeId {
        self.ids.alloc()
    }

    /// An `EdgeId` no other modulation edge has had.
    pub fn alloc_edge(&mut self) -> EdgeId {
        self.ids.alloc_edge()
    }

    /// Every node and where it sits: the groups with their bus effects and
    /// modulators, then the lanes with their effects and modulators. (A lane's
    /// instrument plugin shares the lane's id.)
    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, NodePath)> + '_ {
        let groups = self.groups.iter().enumerate().flat_map(|(group, g)| {
            std::iter::once((g.node, NodePath::Group(group)))
                .chain(g.effects.iter().enumerate().map(move |(index, fx)| {
                    (fx.node, NodePath::GroupEffect { group, index })
                }))
                .chain(g.modulators.iter().enumerate().map(move |(index, m)| {
                    (m.node, NodePath::Modulator { at: ModulatorLocation::Group(group), index })
                }))
        });
        let lanes = self.instruments.iter().enumerate().flat_map(|(inst, lane)| {
            std::iter::once((lane.node, NodePath::Instrument(inst)))
                .chain(lane.effects.iter().enumerate().map(move |(index, fx)| {
                    (fx.node, NodePath::Effect { inst, index })
                }))
                .chain(lane.modulators.iter().enumerate().map(move |(index, m)| {
                    (m.node, NodePath::Modulator { at: ModulatorLocation::Lane(inst), index })
                }))
        });
        groups.chain(lanes)
    }

    /// Where the node `id` sits, if it is still in the session.
    pub fn locate(&self, id: NodeId) -> Option<NodePath> {
        self.nodes().find(|(node, _)| *node == id).map(|(_, path)| path)
    }

    /// Where the modulation cable `id` is: the modulator's rack, the modulator's
    /// position in it and the target's in the modulator. None if there is no such
    /// cable (any more).
    pub fn locate_edge(&self, id: EdgeId) -> Option<(ModulatorLocation, usize, usize)> {
        let groups = self.groups.iter().enumerate().flat_map(|(group, g)| {
            g.modulators.iter().enumerate().flat_map(move |(index, m)| {
                m.targets.iter().enumerate().map(move |(target, t)| (t.edge, ModulatorLocation::Group(group), index, target))
            })
        });
        let lanes = self.instruments.iter().enumerate().flat_map(|(inst, lane)| {
            lane.modulators.iter().enumerate().flat_map(move |(index, m)| {
                m.targets.iter().enumerate().map(move |(target, t)| (t.edge, ModulatorLocation::Lane(inst), index, target))
            })
        });
        groups.chain(lanes).find(|(edge, ..)| *edge == id).map(|(_, at, index, target)| (at, index, target))
    }

    /// Check that every node has an id of its own, that ids were allocated here,
    /// that a lane's instrument plugin carries the lane's id, and that the
    /// telemetry slots are in order: every lane and group has a meter (two slots),
    /// every modulator an output slot and every target on a plugin parameter an
    /// effective-value slot (the others none), all different, allocated here, and
    /// none the engine's own.
    #[cfg(test)]
    pub fn check_ids(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for (id, path) in self.nodes() {
            if id.0 >= self.ids.next {
                return Err(format!("{path:?} has {id:?}, which was never allocated"));
            }
            if !seen.insert(id) {
                return Err(format!("{path:?} has {id:?}, which another node has too"));
            }
        }
        for (inst, lane) in self.instruments.iter().enumerate() {
            if let Some(slot) = &lane.instrument {
                if slot.node != lane.node {
                    return Err(format!("lane {inst} is {:?}, its instrument {:?}", lane.node, slot.node));
                }
            }
        }

        let racks = self
            .groups
            .iter()
            .enumerate()
            .map(|(g, group)| (format!("group {g}"), &group.modulators))
            .chain(self.instruments.iter().enumerate().map(|(i, lane)| (format!("lane {i}"), &lane.modulators)));
        let mut edges = std::collections::HashSet::new();
        let mut taken = std::collections::HashSet::new();
        let mut claim = |slot: Option<Slot>, whose: String| -> Result<(), String> {
            let Some(slot) = slot else { return Err(format!("{whose} has no telemetry slot")) };
            if slot == crate::telemetry::MASTER.peak || slot == crate::telemetry::MASTER.clips {
                return Err(format!("{whose} has {slot:?}, which is the master meter's"));
            }
            if slot.index() >= self.slots.high_water() {
                return Err(format!("{whose} has {slot:?}, which was never allocated"));
            }
            if !taken.insert(slot) {
                return Err(format!("{whose} has {slot:?}, which another node has too"));
            }
            Ok(())
        };
        for (g, group) in self.groups.iter().enumerate() {
            let meter = group.meter;
            claim(meter.map(|m| m.peak), format!("the meter of group {g}"))?;
            claim(meter.map(|m| m.clips), format!("the meter of group {g}"))?;
        }
        for (i, lane) in self.instruments.iter().enumerate() {
            let meter = lane.meter;
            claim(meter.map(|m| m.peak), format!("the meter of lane {i}"))?;
            claim(meter.map(|m| m.clips), format!("the meter of lane {i}"))?;
        }
        for (rack, modulators) in racks {
            for (m, modulator) in modulators.iter().enumerate() {
                claim(modulator.output, format!("modulator {m} of {rack}"))?;
                for (t, target) in modulator.targets.iter().enumerate() {
                    let whose = format!("target {t} of modulator {m} of {rack}");
                    if target.edge.0 >= self.ids.next_edge {
                        return Err(format!("{whose} has {:?}, which was never allocated", target.edge));
                    }
                    if !edges.insert(target.edge) {
                        return Err(format!("{whose} has {:?}, which another edge has too", target.edge));
                    }
                    if target.kind.has_effective_value() {
                        claim(target.effective, whose)?;
                    } else if target.effective.is_some() {
                        return Err(format!("{whose} drives a modulator's field and has an effective-value slot"));
                    }
                }
            }
        }
        Ok(())
    }

    /// The modulator rack at `at`: a lane's or a group's, if it exists.
    pub fn modulators_mut(&mut self, at: ModulatorLocation) -> Option<&mut Vec<ModulatorSlot>> {
        match at {
            ModulatorLocation::Lane(inst) => self.instruments.get_mut(inst).map(|n| &mut n.modulators),
            ModulatorLocation::Group(group) => self.groups.get_mut(group).map(|g| &mut g.modulators),
        }
    }

    /// The modulator rack at `at`: a lane's or a group's, if it exists.
    pub fn modulators(&self, at: ModulatorLocation) -> Option<&Vec<ModulatorSlot>> {
        match at {
            ModulatorLocation::Lane(inst) => self.instruments.get(inst).map(|n| &n.modulators),
            ModulatorLocation::Group(group) => self.groups.get(group).map(|g| &g.modulators),
        }
    }

    /// Take the cable `edge` out of the session, and with it every cable that drives the
    /// depth of a cable that goes: those, and the ones that drive theirs in turn. (A cable
    /// that is left driving the depth of one that is gone would, being addressed by its place,
    /// drive the next one's.) The cables of the same modulator, and of the modulators that
    /// drive their depths, are told the places their neighbours have after, as the audio
    /// thread does it (`RemoveModTarget`).
    ///
    /// The removals, in the order they were made: the rack, the modulator in it, and the
    /// cable's place in the modulator *as it was when its turn came*, so that the audio thread
    /// can be sent the same commands in the same order. A cable goes after the cables that
    /// drive its depth. `None`, and nothing changed, if there is no such cable.
    pub fn remove_edge(&mut self, edge: EdgeId) -> Option<Vec<(ModulatorLocation, usize, usize)>> {
        let going = self.cascade_of(edge)?;
        let mut removals = Vec::new();
        for gone in going.into_iter().rev() {
            let (at, mod_index, target) = self.locate_edge(gone)?;
            self.remove_target_at(at, mod_index, target);
            removals.push((at, mod_index, target));
        }
        Some(removals)
    }

    /// The cables that go when cable `edge` is taken out: it, and then, in turn, those that drive the
    /// depth of one that goes (which may be one that is there already: they can drive each other's).
    /// `None` if there is no such cable.
    pub fn cascade_of(&self, edge: EdgeId) -> Option<Vec<EdgeId>> {
        self.locate_edge(edge)?;
        let mut going = vec![edge];
        let mut next = 0;
        while let Some(&gone) = going.get(next) {
            next += 1;
            for driver in self.depth_cables_of(gone) {
                if !going.contains(&driver) {
                    going.push(driver);
                }
            }
        }
        Some(going)
    }

    /// What the cable of kind `kind`, from the modulator at `source` in the rack at `at`, drives. Nothing
    /// if it names a plugin, a modulator or a cable that is not there, or a modulator that is its own (a
    /// cable like that does nothing).
    pub fn driven_by(&self, at: ModulatorLocation, source: usize, kind: &ModTargetKind) -> Option<Driven> {
        use ModTargetKind as K;
        // A field of the modulator at `index` of `rack`, as a cable of `field` drives it.
        let field_of = |rack: &[ModulatorSlot], index: usize, field: CrossModField| -> Option<Driven> {
            let modulator = rack.get(index)?;
            let setting = |field| Driven::Setting { modulator: modulator.node, field };
            Some(match field {
                CrossModField::Rate => setting(ModulatorField::Rate),
                CrossModField::Attack => setting(ModulatorField::Attack),
                CrossModField::Decay => setting(ModulatorField::Decay),
                CrossModField::Sustain => setting(ModulatorField::Sustain),
                CrossModField::Release => setting(ModulatorField::Release),
                CrossModField::Depth(target) => Driven::Depth(modulator.targets.get(target)?.edge),
            })
        };
        match (at, kind) {
            (ModulatorLocation::Lane(inst), &K::PluginParam { slot, param_index }) => {
                Some(Driven::Param { plugin: self.instruments.get(inst)?.plugin_node(slot)?, index: param_index })
            }
            (ModulatorLocation::Group(group), &K::GroupMember { member, slot, param_index }) => {
                Some(Driven::Param { plugin: self.group_member(group, member)?.plugin_node(slot)?, index: param_index })
            }
            (ModulatorLocation::Group(group), &K::GroupBus { effect_index, param_index }) => {
                Some(Driven::Param { plugin: self.groups.get(group)?.effects.get(effect_index)?.node, index: param_index })
            }
            (ModulatorLocation::Group(group), &K::GroupMemberMod { member, mod_index, field }) => {
                field_of(&self.group_member(group, member)?.modulators, mod_index, field)
            }
            (_, kind) => {
                let (index, field) = sibling_field(kind)?;
                if index == source {
                    return None;
                }
                field_of(self.modulators(at)?, index, field)
            }
        }
    }

    /// What the cable `edge` drives.
    pub fn driven_by_edge(&self, edge: EdgeId) -> Option<Driven> {
        let (at, source, target) = self.locate_edge(edge)?;
        let kind = &self.modulators(at)?.get(source)?.targets.get(target)?.kind;
        self.driven_by(at, source, kind)
    }

    /// Whether any cable in the session drives `driven`.
    pub fn is_driven(&self, driven: Driven) -> bool {
        let lanes = self.instruments.iter().enumerate().map(|(i, lane)| (ModulatorLocation::Lane(i), &lane.modulators));
        let groups = self.groups.iter().enumerate().map(|(i, group)| (ModulatorLocation::Group(i), &group.modulators));
        lanes.chain(groups).any(|(at, rack)| {
            rack.iter().enumerate().any(|(source, m)| m.targets.iter().any(|t| self.driven_by(at, source, &t.kind) == Some(driven)))
        })
    }

    /// The lane that is member `member` of group `group`: the `member`th of the lanes in it.
    fn group_member(&self, group: usize, member: usize) -> Option<&InstrumentNode> {
        self.instruments.iter().filter(|lane| lane.group == Some(group)).nth(member)
    }

    /// Put the lanes at `a` and `b` in each other's places, each with everything it has (its
    /// plugins and effects, modulators, volume, range, group and pattern: the tree moves whole
    /// nodes), as the audio thread's `SwapInstruments` does. A group's cables address their members
    /// by the place of the lane among the group's lanes, so they are told the places the lanes have
    /// now (`member_moves`). False, and nothing changed, if either is not there or they are one.
    pub fn swap_lanes(&mut self, a: usize, b: usize) -> bool {
        let count = self.instruments.len();
        if a == b || a >= count || b >= count {
            return false;
        }
        let groups: Vec<Option<usize>> = self.instruments.iter().map(|lane| lane.group).collect();
        let moves = member_moves(&groups, a, b);
        self.instruments.swap(a, b);
        for (index, group) in self.groups.iter_mut().enumerate() {
            let of_group: Vec<(usize, usize)> = moves.iter().filter(|(g, ..)| *g == index).map(|&(_, old, new)| (old, new)).collect();
            if of_group.is_empty() {
                continue;
            }
            for target in group.modulators.iter_mut().flat_map(|m| &mut m.targets) {
                if let Some(member) = group_target_member_mut(&mut target.kind) {
                    if let Some(&(_, new)) = of_group.iter().find(|(old, _)| old == member) {
                        *member = new;
                    }
                }
            }
        }
        true
    }

    /// The cables of other modulators that would be left pointing at nothing if modulator `index`
    /// of the rack at `at` were taken out: those of its siblings (and, for a lane's, of the group
    /// the lane is a member of) that drive its rate, its stages or the depth of its cables. (Its
    /// own cables go with it. The cables that drive the depth of any of these go with *them*:
    /// `remove_edge`.)
    pub fn cables_into(&self, at: ModulatorLocation, index: usize) -> Vec<EdgeId> {
        let rack = match at {
            ModulatorLocation::Lane(inst) => self.instruments.get(inst).map(|lane| &lane.modulators),
            ModulatorLocation::Group(group) => self.groups.get(group).map(|g| &g.modulators),
        };
        let Some(rack) = rack else { return Vec::new() };
        let siblings = rack.iter().enumerate().filter(|(i, _)| *i != index).flat_map(|(_, m)| &m.targets);
        let mut found: Vec<EdgeId> = siblings.filter(|t| t.kind.sibling_modulator() == Some(index)).map(|t| t.edge).collect();
        if let ModulatorLocation::Lane(inst) = at {
            if let Some((group, member)) = lane_member_ordinal(&self.instruments, inst) {
                let group_cables = self.groups.get(group).into_iter().flat_map(|g| &g.modulators).flat_map(|m| &m.targets);
                found.extend(
                    group_cables
                        .filter(|t| matches!(t.kind, ModTargetKind::GroupMemberMod { member: of, mod_index, .. } if of == member && mod_index == index))
                        .map(|t| t.edge),
                );
            }
        }
        found
    }

    /// The cables that drive a parameter of the plugin in node `plugin`, an instrument, an effect or a bus
    /// effect: a lane's own, and a group's to its members' plugins and to its bus effects. In the order of the
    /// racks (the lanes', then the groups'), each modulator's by place.
    pub fn cables_into_plugin(&self, plugin: NodeId) -> Vec<EdgeId> {
        let lanes = self.instruments.iter().enumerate().map(|(i, lane)| (ModulatorLocation::Lane(i), &lane.modulators));
        let groups = self.groups.iter().enumerate().map(|(i, group)| (ModulatorLocation::Group(i), &group.modulators));
        let mut found = Vec::new();
        for (at, rack) in lanes.chain(groups) {
            for (source, modulator) in rack.iter().enumerate() {
                for target in &modulator.targets {
                    if matches!(self.driven_by(at, source, &target.kind), Some(Driven::Param { plugin: driven, .. }) if driven == plugin) {
                        found.push(target.edge);
                    }
                }
            }
        }
        found
    }

    /// The cables that drive the depth of cable `edge`: in the rack of its modulator, and, if
    /// the modulator is a lane's and the lane is a member of a group, in the group's.
    fn depth_cables_of(&self, edge: EdgeId) -> Vec<EdgeId> {
        let Some((at, mod_index, target)) = self.locate_edge(edge) else { return Vec::new() };
        let rack = match at {
            ModulatorLocation::Lane(inst) => self.instruments.get(inst).map(|lane| &lane.modulators),
            ModulatorLocation::Group(group) => self.groups.get(group).map(|g| &g.modulators),
        };
        let drivers = |rack: Option<&Vec<ModulatorSlot>>, drives: ModTargetKind| -> Vec<EdgeId> {
            rack.into_iter().flatten().flat_map(|m| &m.targets).filter(|t| t.kind == drives).map(|t| t.edge).collect()
        };
        let mut found = drivers(rack, ModTargetKind::ModulatorDepth { mod_index, target_index: target });
        if let ModulatorLocation::Lane(inst) = at {
            if let Some((group, member)) = lane_member_ordinal(&self.instruments, inst) {
                let field = CrossModField::Depth(target);
                found.extend(drivers(self.groups.get(group).map(|g| &g.modulators), ModTargetKind::GroupMemberMod { member, mod_index, field }));
            }
        }
        found
    }

    /// Take cable `target` out of modulator `mod_index` in the rack at `at`, and tell the cables
    /// that drive the depths of its later ones (`fixup_depth_cables_after_remove`).
    fn remove_target_at(&mut self, at: ModulatorLocation, mod_index: usize, target: usize) {
        if let Some(rack) = self.modulators_mut(at) {
            rack[mod_index].targets.remove(target);
            fixup_depth_cables_after_remove(rack, mod_index, target);
        }
        if let ModulatorLocation::Lane(inst) = at {
            if let Some((group, member)) = lane_member_ordinal(&self.instruments, inst) {
                if let Some(g) = self.groups.get_mut(group) {
                    fixup_member_depth_cables_after_remove(&mut g.modulators, member, mod_index, target);
                }
            }
        }
    }

    /// The plugin parameters a modulator in the rack at `at` can drive, in the order a
    /// person is offered them. A lane's modulator drives its lane's chain: the instrument,
    /// then each effect. A group's drives its members' chains, member by member in lane
    /// order, and then its own bus effects. Nothing else is a parameter a modulator can
    /// drive (the fields of other modulators are another kind of target), and a rack that
    /// is not there drives nothing.
    pub fn param_targets(&self, at: ModulatorLocation) -> Vec<ParamTarget> {
        let mut targets = Vec::new();
        match at {
            ModulatorLocation::Lane(inst) => {
                if let Some(lane) = self.instruments.get(inst) {
                    push_chain(&mut targets, lane, |slot, param_index| ModTargetKind::PluginParam { slot, param_index });
                }
            }
            ModulatorLocation::Group(group) => {
                let Some(bus) = self.groups.get(group) else { return targets };
                for (member, lane) in self.instruments.iter().filter(|lane| lane.group == Some(group)).enumerate() {
                    push_chain(&mut targets, lane, |slot, param_index| ModTargetKind::GroupMember { member, slot, param_index });
                }
                for (effect_index, effect) in bus.effects.iter().enumerate() {
                    push_params(&mut targets, effect, 0, |param_index| ModTargetKind::GroupBus { effect_index, param_index });
                }
            }
        }
        targets
    }

    /// The fields of other modulators that the modulator at `mod_index` of the rack at `at` can drive: a rate, the stages of an
    /// envelope and the depths of cables (a cable that pulls a parameter, or a field, pulls it by a depth another modulator can
    /// drive), in the order a person is offered them. A lane's modulator drives those of the others in its own rack, a group's
    /// those of its members' modulators (member by member in lane order, each modulator's settings and then its cables) and then
    /// those of the others in its own rack. Never its own (a modulator does not drive itself), and a rack that is not there
    /// drives nothing. (An index that is not in the rack leaves none out.)
    pub fn field_targets(&self, at: ModulatorLocation, mod_index: usize) -> Vec<FieldTarget> {
        let mut targets = Vec::new();
        match at {
            ModulatorLocation::Lane(inst) => {
                if let Some(lane) = self.instruments.get(inst) {
                    push_siblings(&mut targets, &lane.modulators, mod_index);
                }
            }
            ModulatorLocation::Group(group) => {
                let Some(bus) = self.groups.get(group) else { return targets };
                for (member, lane) in self.instruments.iter().filter(|lane| lane.group == Some(group)).enumerate() {
                    for (index, modulator) in lane.modulators.iter().enumerate() {
                        let owner = format!("M{member} Mod{index}");
                        push_fields(&mut targets, modulator, owner, |field| ModTargetKind::GroupMemberMod { member, mod_index: index, field });
                    }
                }
                push_siblings(&mut targets, &bus.modulators, mod_index);
            }
        }
        targets
    }

    /// Take the effect at `index` out of lane `inst`'s chain, the way the audio
    /// thread's `RemoveEffect` does: the modulator targets on it go with it, and
    /// the ones on later plugins of the chain move down a slot so that they keep
    /// pointing at the same plugins. Those are the lane's own modulators and, if
    /// the lane is a group member, the group's. `None` (and no change) if the lane
    /// has no such effect.
    pub fn remove_effect(&mut self, inst: usize, index: usize) -> Option<PluginSlot> {
        let member_of = lane_member_ordinal(&self.instruments, inst);
        let lane = self.instruments.get_mut(inst)?;
        if index >= lane.effects.len() {
            return None;
        }
        let removed = lane.effects.remove(index);
        fixup_slots_after_remove(&mut lane.modulators, index);
        if let Some((group, member)) = member_of {
            if let Some(g) = self.groups.get_mut(group) {
                fixup_member_slots_after_remove(&mut g.modulators, member, index);
            }
        }
        Some(removed)
    }

    /// The cables of the group that lane `inst` is a member of that go to the lane: to its plugins, and to
    /// its modulators' settings and the depths of their cables. (The lane's own modulators' cables, which go
    /// with it, are not these.) None if the lane is in no group.
    pub fn group_cables_into_lane(&self, inst: usize) -> Vec<EdgeId> {
        let Some((group, member)) = lane_member_ordinal(&self.instruments, inst) else { return Vec::new() };
        let cables = self.groups.get(group).into_iter().flat_map(|g| &g.modulators).flat_map(|m| &m.targets);
        cables
            .filter(|t| matches!(t.kind, ModTargetKind::GroupMember { member: of, .. } | ModTargetKind::GroupMemberMod { member: of, .. } if of == member))
            .map(|t| t.edge)
            .collect()
    }

    /// Take lane `inst` out, the way the audio thread's `RemoveInstrument` does, with everything it has: its
    /// plugins, modulators and pattern. If it was a member of a group, the group's cables to it go with it, and
    /// the ones to the members after it are told their new ordinals. `None` (and no change) if there is no
    /// such lane.
    pub fn remove_lane(&mut self, inst: usize) -> Option<InstrumentNode> {
        let member_of = lane_member_ordinal(&self.instruments, inst);
        if inst >= self.instruments.len() {
            return None;
        }
        let lane = self.instruments.remove(inst);
        if let Some((group, ordinal)) = member_of {
            if let Some(g) = self.groups.get_mut(group) {
                fixup_group_member_after_remove(&mut g.modulators, ordinal);
            }
        }
        Some(lane)
    }

    /// Take group `group` out, with its bus effects and its modulators, the way the audio thread's `RemoveGroup`
    /// does: its member lanes are in no group, and the lanes of the groups after it have the index that is one
    /// less. `None` (and no change) if there is no such group.
    pub fn remove_group(&mut self, group: usize) -> Option<GroupNode> {
        if group >= self.groups.len() {
            return None;
        }
        let removed = self.groups.remove(group);
        for lane in &mut self.instruments {
            match lane.group {
                Some(of) if of == group => lane.group = None,
                Some(of) if of > group => lane.group = Some(of - 1),
                _ => {}
            }
        }
        Some(removed)
    }

    /// Take the bus effect at `index` out of group `group`, the way the audio thread's `RemoveGroupEffect`
    /// does: the group's modulator targets on it go with it, and the ones on later bus effects move down an
    /// index so that they keep pointing at the same plugins. `None` (and no change) if the group has no such
    /// effect.
    pub fn remove_bus_effect(&mut self, group: usize, index: usize) -> Option<PluginSlot> {
        let bus = self.groups.get_mut(group)?;
        if index >= bus.effects.len() {
            return None;
        }
        let removed = bus.effects.remove(index);
        fixup_group_bus_after_remove(&mut bus.modulators, index);
        Some(removed)
    }

    /// Move the effect at `from` to `to` in lane `inst`'s chain, the way the audio
    /// thread's `ReorderEffect` does: the modulator targets of the lane and of its
    /// group follow their plugins to the new slots. False (and no change) if either
    /// index is outside the chain or they are the same.
    pub fn move_effect(&mut self, inst: usize, from: usize, to: usize) -> bool {
        let member_of = lane_member_ordinal(&self.instruments, inst);
        let Some(lane) = self.instruments.get_mut(inst) else { return false };
        if from >= lane.effects.len() || to >= lane.effects.len() || from == to {
            return false;
        }
        let effect = lane.effects.remove(from);
        lane.effects.insert(to, effect);
        remap_slots_after_reorder(&mut lane.modulators, from, to);
        if let Some((group, member)) = member_of {
            if let Some(g) = self.groups.get_mut(group) {
                remap_member_slots_after_reorder(&mut g.modulators, member, from, to);
            }
        }
        true
    }
}

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

#[derive(Debug)]
pub struct PluginSlot {
    /// The node the plugin sits in: its own for an effect or a bus effect, the
    /// lane's for an instrument.
    pub node: NodeId,
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

#[derive(Debug, Default)]
pub enum ParamKind {
    #[default]
    Float,
    Enum(Vec<String>),
    Separator,
}

#[derive(Debug, Default)]
pub struct ParamSlot {
    pub name: String,
    pub index: u32,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub value: f32,
    pub kind: ParamKind,
    /// What the value is measured in, for showing it (`ParameterInfo::unit`).
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the graph a client gets (`app::graph`), which only the web runtime sends"))]
    pub unit: Option<String>,
    /// The sections the plugin files the parameter under, outermost first
    /// (`ParameterInfo::group`).
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the graph a client gets (`app::graph`), which only the web runtime sends"))]
    pub group: Vec<String>,
    /// How many steps there are between `min` and `max`; 0 for a continuous
    /// parameter (`ParameterInfo::step_count`).
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the graph a client gets (`app::graph`), which only the web runtime sends"))]
    pub step_count: u32,
}

// ---------------------------------------------------------------------------
// Lanes and groups
// ---------------------------------------------------------------------------

/// Main-thread mirror of pattern state for an instrument.
#[derive(Debug)]
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

#[derive(Debug)]
pub struct InstrumentNode {
    pub node: NodeId,
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
    /// Where the audio thread publishes the lane's output level (telemetry);
    /// `None` if the slots had run out.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the telemetry layout a client gets (`app::layout`), which only the web runtime sends"))]
    pub meter: Option<MeterSlots>,
}

impl InstrumentNode {
    /// The lane has no note remap any more, nor the pitch bend range that went with it: the audio thread drops the remapper
    /// when the instrument it was for goes (a swap, a clear), and the model says what the audio thread has.
    pub fn forget_remap(&mut self) {
        self.remap.clear();
        self.pitch_bend_range = 2.0;
    }

    /// The node of the plugin in `slot` of the lane's chain (0 is the instrument, 1.. the effects), if there is one.
    pub fn plugin_node(&self, slot: usize) -> Option<NodeId> {
        match slot {
            0 => self.instrument.as_ref().map(|plugin| plugin.node),
            effect => self.effects.get(effect - 1).map(|plugin| plugin.node),
        }
    }
}

/// A submix group: its members are the instruments whose `group` points here;
/// it sums them and runs its own effect chain + volume.
#[derive(Debug)]
pub struct GroupNode {
    pub node: NodeId,
    pub name: Option<String>,
    pub volume: f32,
    /// Stereo balance pan for the group bus (-1 left … 0 center … +1 right).
    pub pan: f32,
    pub effects: Vec<PluginSlot>,
    /// Group-scoped modulators. Each target addresses a member instrument
    /// (`GroupMember`), a bus effect (`GroupBus`), or a sibling group modulator.
    pub modulators: Vec<ModulatorSlot>,
    /// Where the audio thread publishes the bus's output level (telemetry);
    /// `None` if the slots had run out.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the telemetry layout a client gets (`app::layout`), which only the web runtime sends"))]
    pub meter: Option<MeterSlots>,
}

impl GroupNode {
    /// What the group is called: its name, or "Group N" counting from one by its
    /// position `index` among the session's groups.
    pub fn display_name(&self, index: usize) -> String {
        self.name.clone().unwrap_or_else(|| format!("Group {}", index + 1))
    }
}

#[derive(Debug)]
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
    /// What a modulator added without further ado starts as: a 1 Hz sine.
    pub fn new_lfo() -> Self {
        ModSourceSlot::Lfo { waveform: crate::plugin::chain::LfoWaveform::Sine, rate: 1.0 }
    }

    /// An envelope as the modulate popup makes it: 10 ms attack, 0.3 s decay,
    /// sustain 0.7, 0.5 s release.
    pub fn new_envelope() -> Self {
        ModSourceSlot::Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 }
    }

    /// A control that is waiting for the next one that comes in to bind to.
    pub fn new_midi_learn() -> Self {
        ModSourceSlot::MidiLearn { source: None, learning: true }
    }

    /// What the modulator's node is called for a client: its kind, or what a control
    /// is bound to. Not its settings, which a client is told when they change and
    /// which a name would have to follow (`label` has them, for the TUI).
    pub fn node_name(&self) -> String {
        match self {
            ModSourceSlot::Lfo { .. } => "LFO".to_string(),
            other => other.label(),
        }
    }

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

#[derive(Debug)]
pub struct ModulatorSlot {
    pub node: NodeId,
    pub source: ModSourceSlot,
    pub targets: Vec<ModTargetSlot>,
    /// Where the audio thread publishes the modulator's output (telemetry);
    /// `None` if the slots had run out.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the telemetry layout a client gets (`app::layout`), which only the web runtime sends"))]
    pub output: Option<Slot>,
}

#[derive(Debug)]
pub struct ModTargetSlot {
    /// The edge this target is, for as long as it exists.
    pub edge: EdgeId,
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
    /// Where the audio thread publishes the effective value of the parameter the
    /// target drives (telemetry); `None` for a target that drives a modulator's
    /// own field, or if the slots had run out.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the telemetry layout a client gets (`app::layout`), which only the web runtime sends"))]
    pub effective: Option<Slot>,
}

/// A plugin parameter a modulator can drive (`SessionModel::param_targets`), with what it
/// takes to bind it (`Controller::add_target`) and to say what it is.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamTarget {
    /// The node of the plugin the parameter is on (a lane's own, for its instrument).
    pub node: NodeId,
    /// The plugin's own index for the parameter.
    pub param: u32,
    /// How the modulator's rack addresses it.
    pub kind: ModTargetKind,
    /// The plugin's slot in its chain: 0 for an instrument (and for a bus effect), 1.. for
    /// the effects of a lane. A target is labelled with its plugin by it.
    pub slot: usize,
    pub plugin_name: String,
    pub param_name: String,
    /// What the user has set the parameter to, where its modulation is centred.
    pub base_value: f32,
    pub param_min: f32,
    pub param_max: f32,
}

/// A field of a modulator that another modulator can drive (`SessionModel::field_targets`): its rate, a stage of its envelope or the
/// depth of one of its cables, with what it takes to bind it (`Controller::add_target`) and to say what it is.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldTarget {
    /// The node of the modulator whose field it is.
    pub node: NodeId,
    /// Which field it is.
    pub field: CrossModField,
    /// The cable, when the field is the depth of one (`Port::Depth` names it by this).
    pub edge: Option<EdgeId>,
    /// How the driving modulator's rack addresses it.
    pub kind: ModTargetKind,
    /// Whose field it is, as a person is told: `Mod 1` for the modulator at index 1 of the rack, `M0 Mod1` for the one at index 1
    /// of the first member of a group.
    pub owner: String,
    /// What the field is called: `rate`, `attack`, ..., or, for a cable's depth, `detune depth`.
    pub field_name: String,
    /// What the target is labelled with once it is bound: the owner and the name.
    pub label: String,
    /// What the field is set to now, where its modulation is centred.
    pub base_value: f32,
    pub param_min: f32,
    pub param_max: f32,
}

/// The fields of the modulators of `rack` but the one at `except`, which a modulator of the same rack addresses as its siblings'.
fn push_siblings(targets: &mut Vec<FieldTarget>, rack: &[ModulatorSlot], except: usize) {
    for (index, modulator) in rack.iter().enumerate().filter(|(index, _)| *index != except) {
        push_fields(targets, modulator, format!("Mod {index}"), |field| sibling_kind(index, field));
    }
}

/// The fields of `modulator`: the settings its source has, in the order of the pane (the rate; the four stages), and then the depth
/// of each of its cables. `kind` says how the rack that drives them addresses a field.
fn push_fields(targets: &mut Vec<FieldTarget>, modulator: &ModulatorSlot, owner: String, kind: impl Fn(CrossModField) -> ModTargetKind) {
    let mut push = |field: CrossModField, name: String, edge: Option<EdgeId>, base_value: f32, (param_min, param_max): (f32, f32)| {
        let (node, kind, label, owner) = (modulator.node, kind(field), format!("{owner} {name}"), owner.clone());
        targets.push(FieldTarget { node, field, edge, kind, owner, field_name: name, label, base_value, param_min, param_max });
    };
    const SETTINGS: [(CrossModField, ModulatorField, &str); 5] = [
        (CrossModField::Rate, ModulatorField::Rate, "rate"),
        (CrossModField::Attack, ModulatorField::Attack, "attack"),
        (CrossModField::Decay, ModulatorField::Decay, "decay"),
        (CrossModField::Sustain, ModulatorField::Sustain, "sustain"),
        (CrossModField::Release, ModulatorField::Release, "release"),
    ];
    for (field, setting, name) in SETTINGS {
        if let Some(value) = modulator.source.setting(setting) {
            push(field, name.to_string(), None, value, setting.range());
        }
    }
    for (index, cable) in modulator.targets.iter().enumerate() {
        push(CrossModField::Depth(index), format!("{} depth", cable.param_name), Some(cable.edge), cable.depth, (DEPTH_MIN, DEPTH_MAX));
    }
}

/// How a rack addresses the field `field` of its own modulator at `mod_index`: the other way round of `sibling_field`.
fn sibling_kind(mod_index: usize, field: CrossModField) -> ModTargetKind {
    match field {
        CrossModField::Rate => ModTargetKind::ModulatorRate { mod_index },
        CrossModField::Attack => ModTargetKind::ModulatorAttack { mod_index },
        CrossModField::Decay => ModTargetKind::ModulatorDecay { mod_index },
        CrossModField::Sustain => ModTargetKind::ModulatorSustain { mod_index },
        CrossModField::Release => ModTargetKind::ModulatorRelease { mod_index },
        CrossModField::Depth(target_index) => ModTargetKind::ModulatorDepth { mod_index, target_index },
    }
}

/// The parameters of every plugin in `lane`'s chain, the instrument first. `kind` makes the
/// kind of target of a parameter from its plugin's slot and its own index.
fn push_chain(targets: &mut Vec<ParamTarget>, lane: &InstrumentNode, kind: impl Fn(usize, u32) -> ModTargetKind) {
    for (slot, plugin) in std::iter::once(lane.instrument.as_ref()).chain(lane.effects.iter().map(Some)).enumerate() {
        if let Some(plugin) = plugin {
            push_params(targets, plugin, slot, |param_index| kind(slot, param_index));
        }
    }
}

/// The parameters of `plugin`, which is in chain slot `slot` (a separator is a row of a pane,
/// not a parameter, and is left out).
fn push_params(targets: &mut Vec<ParamTarget>, plugin: &PluginSlot, slot: usize, kind: impl Fn(u32) -> ModTargetKind) {
    for row in plugin.params.iter().filter(|row| !matches!(row.kind, ParamKind::Separator)) {
        targets.push(ParamTarget {
            node: plugin.node,
            param: row.index,
            kind: kind(row.index),
            slot,
            plugin_name: plugin.name.clone(),
            param_name: row.name.clone(),
            base_value: row.value,
            param_min: row.min,
            param_max: row.max,
        });
    }
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
/// How deep a cable pulls when it is first drawn: half way, which a person can hear and turn either way.
pub const NEW_CABLE_DEPTH: f32 = 0.5;

/// What a row of a modulator's parameter pane is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModulatorRow {
    /// The kind of source, which an LFO and an envelope switch between.
    Type,
    /// A MIDI control's source: shown, not edited.
    Info,
    /// An LFO's waveform, stepped through the four.
    Waveform,
    /// A number of the source: an LFO's rate, a stage of an envelope.
    Setting(ModulatorField),
    /// The "Targets" separator.
    Separator,
    /// The depth of the target with this index.
    Depth(usize),
}

impl ModSourceSlot {
    /// The rows of the pane before the targets' (which follow the separator).
    fn rows(&self) -> &'static [ModulatorRow] {
        use ModulatorField as F;
        use ModulatorRow as R;
        match self {
            ModSourceSlot::Lfo { .. } => &[R::Type, R::Waveform, R::Setting(F::Rate), R::Separator],
            ModSourceSlot::Envelope { .. } => {
                &[R::Type, R::Setting(F::Attack), R::Setting(F::Decay), R::Setting(F::Sustain), R::Setting(F::Release), R::Separator]
            }
            ModSourceSlot::MidiLearn { .. } => &[R::Info, R::Separator],
        }
    }

    /// The value of a setting, if the source has it (a waveform is its index).
    pub fn setting(&self, field: ModulatorField) -> Option<f32> {
        match (self, field) {
            (ModSourceSlot::Lfo { rate, .. }, ModulatorField::Rate) => Some(*rate),
            (ModSourceSlot::Lfo { waveform, .. }, ModulatorField::Waveform) => Some(waveform.to_index() as f32),
            (ModSourceSlot::Envelope { attack, .. }, ModulatorField::Attack) => Some(*attack),
            (ModSourceSlot::Envelope { decay, .. }, ModulatorField::Decay) => Some(*decay),
            (ModSourceSlot::Envelope { sustain, .. }, ModulatorField::Sustain) => Some(*sustain),
            (ModSourceSlot::Envelope { release, .. }, ModulatorField::Release) => Some(*release),
            _ => None,
        }
    }
}

/// Number of param-pane rows for a modulator: the rows of its source, the "Targets"
/// separator among them, and one row a target. Scope-independent (lane or group).
pub fn modulator_param_len(m: &ModulatorSlot) -> usize {
    m.source.rows().len() + m.targets.len()
}

/// What row `row` of the pane of `m` is; `None` past the last. Every other question
/// about a row (its range, whether it is an enum, what it is bound to by MIDI learn,
/// which intent it makes) is answered from this.
pub fn modulator_row(m: &ModulatorSlot, row: usize) -> Option<ModulatorRow> {
    let fixed = m.source.rows();
    match fixed.get(row) {
        Some(&found) => Some(found),
        None => {
            let target = row - fixed.len();
            (target < m.targets.len()).then_some(ModulatorRow::Depth(target))
        }
    }
}

/// (min, max) for a modulator pseudo-param at row `pa`, or None for enum /
/// separator rows. Scope-independent.
pub fn modulator_param_range(m: &ModulatorSlot, pa: usize) -> Option<(f32, f32)> {
    match modulator_row(m, pa)? {
        ModulatorRow::Setting(field) => Some(field.range()),
        ModulatorRow::Depth(_) => Some((DEPTH_MIN, DEPTH_MAX)),
        ModulatorRow::Type | ModulatorRow::Info | ModulatorRow::Waveform | ModulatorRow::Separator => None,
    }
}

/// True if a modulator pseudo-param at row `pa` is an enum (Type / Waveform).
pub fn modulator_param_is_enum(m: &ModulatorSlot, pa: usize) -> bool {
    matches!(modulator_row(m, pa), Some(ModulatorRow::Type | ModulatorRow::Waveform))
}

impl ModulatorSlot {
    /// The intent that makes row `row` what `f` makes of what it is now: a setting of the
    /// source (an LFO's rate, a stage of an envelope) or the depth of a target. `None` for
    /// a row that is not a number (the type, the waveform, a separator, a control's source).
    pub fn row_intent(&self, row: usize, f: impl FnOnce(f32) -> f32) -> Option<Intent> {
        match modulator_row(self, row)? {
            ModulatorRow::Setting(field) => Some(Intent::SetModulatorField { node: self.node, field, value: f(self.source.setting(field)?) }),
            ModulatorRow::Depth(target) => {
                let target = self.targets.get(target)?;
                Some(Intent::SetEdgeDepth { edge: target.edge, depth: f(target.depth) })
            }
            ModulatorRow::Type | ModulatorRow::Info | ModulatorRow::Waveform | ModulatorRow::Separator => None,
        }
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
    match modulator_row(m, pa)? {
        ModulatorRow::Setting(field) => {
            let (kind, name) = match field {
                ModulatorField::Rate => (K::ModulatorRate { mod_index }, "rate"),
                ModulatorField::Attack => (K::ModulatorAttack { mod_index }, "attack"),
                ModulatorField::Decay => (K::ModulatorDecay { mod_index }, "decay"),
                ModulatorField::Sustain => (K::ModulatorSustain { mod_index }, "sustain"),
                ModulatorField::Release => (K::ModulatorRelease { mod_index }, "release"),
                // (It has a row of its own, `ModulatorRow::Waveform`, which is not a number for a control to move.)
                ModulatorField::Waveform => return None,
            };
            let (min, max) = field.range();
            Some((kind, min, max, m.source.setting(field)?, format!("Mod {mod_index} {name}")))
        }
        ModulatorRow::Depth(target) => {
            let depth = m.targets.get(target)?.depth;
            Some((K::ModulatorDepth { mod_index, target_index: target }, DEPTH_MIN, DEPTH_MAX, depth, format!("Mod {mod_index} depth {target}")))
        }
        ModulatorRow::Type | ModulatorRow::Info | ModulatorRow::Waveform | ModulatorRow::Separator => None,
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

/// Session-model mirror of the audio thread's `fixup_depth_cables_after_remove`: after the cable
/// at `removed` is taken out of the modulator at `mod_index`, the cables of the modulators in
/// the rack that drive the depth of a later one of its cables are told its new place.
fn fixup_depth_cables_after_remove(modulators: &mut [ModulatorSlot], mod_index: usize, removed: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::ModulatorDepth { mod_index: of, target_index } = &mut t.kind {
                if *of == mod_index && *target_index > removed {
                    *target_index -= 1;
                }
            }
        }
    }
}

/// The same for a group's cables to the depth of a cable of member `member`'s modulator
/// `mod_index` (`fixup_member_depth_cables_after_remove` of the audio thread).
fn fixup_member_depth_cables_after_remove(modulators: &mut [ModulatorSlot], member: usize, mod_index: usize, removed: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMemberMod { member: of, mod_index: at, field: CrossModField::Depth(target) } = &mut t.kind {
                if *of == member && *at == mod_index && *target > removed {
                    *target -= 1;
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

/// Where index `i` lands when the element at `from` moves to `to` and the
/// elements in between slide over by one.
fn remap_index(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if from < to && i > from && i <= to {
        i - 1
    } else if from > to && i >= to && i < from {
        i + 1
    } else {
        i
    }
}

/// Session-model mirror of the audio thread's `fixup_slots_after_remove`: after
/// the effect at index `removed` is deleted from a lane's chain, drop the
/// plugin-param targets that pointed at it and shift higher slots down one, so
/// every remaining target still points at the same plugin. Slot 0 (the
/// instrument) never moves.
pub fn fixup_slots_after_remove(modulators: &mut [ModulatorSlot], removed: usize) {
    use crate::plugin::chain::ModTargetKind;
    let removed_slot = removed + 1;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            !matches!(&t.kind, ModTargetKind::PluginParam { slot, .. } if *slot == removed_slot)
        });
        for t in &mut m.targets {
            if let ModTargetKind::PluginParam { slot, .. } = &mut t.kind {
                if *slot > removed_slot {
                    *slot -= 1;
                    t.slot = *slot;
                }
            }
        }
    }
}

/// Session-model mirror of `remap_slots_after_reorder`: after the effect at
/// `from` moves to `to`, remap plugin-param target slots through the same
/// permutation so they follow their plugins.
pub fn remap_slots_after_reorder(modulators: &mut [ModulatorSlot], from: usize, to: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::PluginParam { slot, .. } = &mut t.kind {
                if *slot >= 1 {
                    *slot = remap_index(*slot - 1, from, to) + 1;
                    t.slot = *slot;
                }
            }
        }
    }
}

/// Session-model mirror of `fixup_member_slots_after_remove`: after the effect at
/// index `removed` is deleted from member `member`'s lane, drop the group targets
/// that pointed at it and shift that member's higher slots down one, so every
/// remaining target still points at the same plugin. Other members' targets and
/// slot 0 (the instrument) never move.
pub fn fixup_member_slots_after_remove(modulators: &mut [ModulatorSlot], member: usize, removed: usize) {
    use crate::plugin::chain::ModTargetKind;
    let removed_slot = removed + 1;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            !matches!(&t.kind, ModTargetKind::GroupMember { member: tm, slot, .. }
                if *tm == member && *slot == removed_slot)
        });
        for t in &mut m.targets {
            if let ModTargetKind::GroupMember { member: tm, slot, .. } = &mut t.kind {
                if *tm == member && *slot > removed_slot {
                    *slot -= 1;
                    t.slot = *slot;
                }
            }
        }
    }
}

/// Session-model mirror of `remap_member_slots_after_reorder`: after the effect at
/// `from` moves to `to` in member `member`'s lane, remap that member's group
/// target slots through the same permutation so they follow their plugins.
pub fn remap_member_slots_after_reorder(modulators: &mut [ModulatorSlot], member: usize, from: usize, to: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMember { member: tm, slot, .. } = &mut t.kind {
                if *tm == member && *slot >= 1 {
                    *slot = remap_index(*slot - 1, from, to) + 1;
                    t.slot = *slot;
                }
            }
        }
    }
}

/// Session-model mirror of `remap_group_bus_after_reorder`: after bus effect
/// `from` moves to `to`, remap `GroupBus` effect indices through the same
/// permutation so targets follow their effects.
pub fn remap_group_bus_after_reorder(modulators: &mut [ModulatorSlot], from: usize, to: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupBus { effect_index, .. } = &mut t.kind {
                *effect_index = remap_index(*effect_index, from, to);
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
    use crate::app::testutil::load;
    use crate::plugin::chain::{CrossModField, LfoWaveform, MidiBindSource, ModSource, ModTargetKind as K};

    #[test]
    fn every_node_is_found_by_its_id() {
        use crate::app::testutil::GROUPS;
        let model = load(GROUPS).model;
        model.check_ids().unwrap();

        let nodes: Vec<(NodeId, NodePath)> = model.nodes().collect();
        let expected = model.groups.len()
            + model.groups.iter().map(|g| g.effects.len() + g.modulators.len()).sum::<usize>()
            + model.instruments.len()
            + model.instruments.iter().map(|l| l.effects.len() + l.modulators.len()).sum::<usize>();
        assert_eq!(nodes.len(), expected, "one node per group, lane, effect, bus effect and modulator");
        for (id, path) in &nodes {
            assert_eq!(model.locate(*id), Some(*path), "{id:?}");
        }
        let has = |f: fn(&NodePath) -> bool| nodes.iter().any(|(_, p)| f(p));
        assert!(has(|p| matches!(p, NodePath::Instrument(_))));
        assert!(has(|p| matches!(p, NodePath::Effect { .. })));
        assert!(has(|p| matches!(p, NodePath::Group(_))));
        assert!(has(|p| matches!(p, NodePath::GroupEffect { .. })));
        assert!(has(|p| matches!(p, NodePath::Modulator { at: ModulatorLocation::Lane(_), .. })));
        assert!(has(|p| matches!(p, NodePath::Modulator { at: ModulatorLocation::Group(_), .. })));

        assert_eq!(model.locate(NodeId(9_999)), None, "an id nothing has");
    }

    #[test]
    fn a_lanes_instrument_sits_in_the_lanes_node() {
        let model = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n").model;
        let lane = &model.instruments[0];
        assert_eq!(lane.instrument.as_ref().unwrap().node, lane.node);
        assert_ne!(lane.effects[0].node, lane.node, "an effect is a node of its own");
    }

    #[test]
    fn ids_are_handed_out_once() {
        let mut ids = NodeIds::default();
        let a = ids.alloc();
        let b = ids.alloc();
        assert_ne!(a, b);
        let mut model = SessionModel::default();
        let first = model.alloc_node();
        assert_ne!(model.alloc_node(), first);
    }

    #[test]
    fn a_group_is_called_by_its_name_or_else_by_its_number_counting_from_one() {
        let mut model = load("[[group]]\nname = \"Pad\"\n[[group]]\n[[group]]\nname = \"\"\n").model;
        let names: Vec<String> = model.groups.iter().enumerate().map(|(g, group)| group.display_name(g)).collect();
        assert_eq!(names, ["Pad", "Group 2", "Group 3"]);
        model.groups[0].name = None;
        assert_eq!(model.groups[0].display_name(0), "Group 1");
        assert_eq!(model.groups[0].display_name(6), "Group 7", "it is the index it is asked about");
    }

    #[test]
    fn edge_ids_are_a_series_of_their_own() {
        let mut model = SessionModel::default();
        let (n0, e0, n1, e1) = (model.alloc_node(), model.alloc_edge(), model.alloc_node(), model.alloc_edge());
        assert_eq!((n0, n1), (NodeId(0), NodeId(1)), "edges do not use up node ids");
        assert_eq!((e0, e1), (EdgeId(0), EdgeId(1)));
    }

    #[test]
    fn check_ids_notices_edge_ids_that_are_shared_or_strangers() {
        use crate::app::testutil::GROUPS;
        let fresh = || load(GROUPS).model;

        let mut model = fresh();
        let first = model.groups[0].modulators[0].targets[0].edge;
        model.groups[0].modulators[0].targets[1].edge = first;
        let error = model.check_ids().unwrap_err();
        assert!(error.contains("another edge has too"), "two targets with one edge: {error}");

        // A lane's target and a group's.
        let mut model = load(
            r#"
[[group]]
[[group.modulator]]
[[group.modulator.target]]
member = 0
param = "detune"
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
[[instrument.modulator.target]]
param = "detune"
"#,
        )
        .model;
        assert!(model.check_ids().is_ok());
        model.instruments[0].modulators[0].targets[0].edge = model.groups[0].modulators[0].targets[0].edge;
        let error = model.check_ids().unwrap_err();
        assert!(error.contains("another edge has too"), "a lane's target and a group's: {error}");

        let mut model = fresh();
        model.groups[0].modulators[0].targets[0].edge = EdgeId(9_999);
        let error = model.check_ids().unwrap_err();
        assert!(error.contains("never allocated"), "an edge nothing handed out: {error}");
    }

    #[test]
    fn check_ids_notices_duplicates_strangers_and_a_mismatched_instrument() {
        let fresh = || load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n").model;

        let mut model = fresh();
        assert!(model.check_ids().is_ok());

        model.instruments[0].effects[0].node = model.instruments[0].node;
        assert!(model.check_ids().unwrap_err().contains("another node has"), "a duplicate");

        let mut model = fresh();
        model.instruments[0].effects[0].node = NodeId(500);
        assert!(model.check_ids().unwrap_err().contains("never allocated"), "a stranger");

        let mut model = fresh();
        let other = model.alloc_node();
        model.instruments[0].instrument.as_mut().unwrap().node = other;
        assert!(model.check_ids().unwrap_err().contains("its instrument"), "an instrument in another node");
    }

    #[test]
    fn check_ids_notices_telemetry_slots_that_are_missing_shared_reserved_or_strangers() {
        use crate::app::testutil::GROUPS;
        use crate::telemetry::MASTER;
        let fresh = || load(GROUPS).model;
        assert!(fresh().check_ids().is_ok());

        let mut model = fresh();
        model.groups[0].modulators[0].output = None;
        assert!(model.check_ids().unwrap_err().contains("has no telemetry slot"), "a modulator without one");

        let mut model = fresh();
        model.instruments[1].modulators[0].output = None;
        let error = model.check_ids().unwrap_err();
        assert!(error.contains("has no telemetry slot") && error.contains("lane 1"), "a lane's modulator without one: {error}");

        let mut model = fresh();
        model.groups[0].modulators[0].targets[0].effective = None;
        assert!(model.check_ids().unwrap_err().contains("has no telemetry slot"), "a parameter target without one");

        let mut model = fresh();
        model.instruments[0].meter = None;
        assert!(model.check_ids().unwrap_err().contains("the meter of lane 0"), "a lane without a meter");

        let mut model = fresh();
        model.groups[0].meter = None;
        assert!(model.check_ids().unwrap_err().contains("the meter of group 0"), "a group without a meter");

        let mut model = fresh();
        let meter = model.groups[0].meter.unwrap();
        model.groups[0].meter = Some(crate::telemetry::MeterSlots { peak: meter.peak, clips: meter.peak });
        assert!(model.check_ids().unwrap_err().contains("another node has too"), "a meter whose two slots are one");

        let mut model = fresh();
        model.instruments[2].meter = model.instruments[1].meter;
        assert!(model.check_ids().unwrap_err().contains("another node has too"), "two lanes with one meter");

        let mut model = fresh();
        model.instruments[0].meter = Some(MASTER);
        assert!(model.check_ids().unwrap_err().contains("master meter's"), "a lane with the master meter's");

        // Each of a meter's two slots is looked at, not only the first.
        let mut model = fresh();
        let own = model.instruments[0].meter.unwrap();
        model.instruments[0].meter = Some(crate::telemetry::MeterSlots { peak: own.peak, clips: MASTER.clips });
        assert!(model.check_ids().unwrap_err().contains("master meter's"), "a lane's clip counter that is the master's");
        let mut model = fresh();
        let own = model.groups[0].meter.unwrap();
        model.groups[0].meter = Some(crate::telemetry::MeterSlots { peak: own.peak, clips: MASTER.clips });
        assert!(model.check_ids().unwrap_err().contains("master meter's"), "a group's clip counter that is the master's");

        let mut model = fresh();
        model.groups[0].modulators[1].output = model.groups[0].modulators[0].output;
        assert!(model.check_ids().unwrap_err().contains("another node has too"), "two modulators with one");

        let mut model = fresh();
        model.groups[0].modulators[0].targets[1].effective = model.groups[0].modulators[0].targets[0].effective;
        assert!(model.check_ids().unwrap_err().contains("another node has too"), "two targets with one");

        let mut model = fresh();
        model.groups[0].modulators[0].output = Some(MASTER.clips);
        assert!(model.check_ids().unwrap_err().contains("master meter's"));

        let mut model = fresh();
        let high = model.slots.alloc().unwrap(); // allocated here...
        model.instruments[1].modulators[0].output = Some(high);
        assert!(model.check_ids().is_ok(), "...so it is fine");
        let mut model = fresh();
        model.slots = SlotAllocator::default(); // as if nothing had been allocated: every slot is a stranger
        assert!(model.check_ids().unwrap_err().contains("never allocated"));

        let mut model = fresh();
        model.groups[0].modulators[0].targets[3].effective = model.slots.alloc();
        assert!(
            model.check_ids().unwrap_err().contains("drives a modulator's field"),
            "a cross-mod target with an effective-value slot"
        );
    }

    #[test]
    fn a_modulator_rack_is_found_by_where_it_lives() {
        let mut model = load(
            r#"
[[group]]
[[group.modulator]]
type = "lfo"
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
type = "lfo"
[[instrument.modulator]]
type = "envelope"
"#,
        )
        .model;
        assert_eq!(model.modulators_mut(ModulatorLocation::Lane(0)).map(|m| m.len()), Some(2));
        assert_eq!(model.modulators_mut(ModulatorLocation::Group(0)).map(|m| m.len()), Some(1));
        assert!(model.modulators_mut(ModulatorLocation::Lane(1)).is_none());
        assert!(model.modulators_mut(ModulatorLocation::Group(1)).is_none());

        // It is the rack itself, not a copy.
        model.modulators_mut(ModulatorLocation::Group(0)).unwrap().pop();
        assert_eq!((model.instruments[0].modulators.len(), model.groups[0].modulators.len()), (2, 0));
    }

    #[test]
    fn the_cables_into_a_plugin_are_the_lanes_and_the_groups_that_drive_a_parameter_of_it() {
        use crate::app::testutil::CHAINS;
        let model = load(CHAINS).model;
        let cable = |at: ModulatorLocation, modulator: usize, target: usize| model.modulators(at).unwrap()[modulator].targets[target].edge;
        let lane = |i: usize, m: usize, t: usize| cable(ModulatorLocation::Lane(i), m, t);
        let group = |m: usize, t: usize| cable(ModulatorLocation::Group(0), m, t);
        let (first, second, third) = (&model.instruments[0], &model.instruments[1], &model.instruments[2]);
        // The first lane's filter: its own modulator's cable to the cutoff, and the group's to it as the first member's.
        assert_eq!(model.cables_into_plugin(first.effects[0].node), [lane(0, 0, 0), group(0, 0)]);
        assert_eq!(model.cables_into_plugin(first.effects[1].node), [lane(0, 0, 1), group(0, 1)]);
        // An instrument, which has the lane's node: its detune from its own modulator and, for the second member, from the group.
        assert_eq!(model.cables_into_plugin(first.node), [lane(0, 0, 2)]);
        assert_eq!(model.cables_into_plugin(second.node), [group(0, 2)]);
        // The bus effects have the group's cables to them, and the third lane's filter its own.
        assert_eq!(model.cables_into_plugin(model.groups[0].effects[0].node), [group(0, 3)]);
        assert_eq!(model.cables_into_plugin(model.groups[0].effects[1].node), [group(0, 4)]);
        assert_eq!(model.cables_into_plugin(third.effects[0].node), [lane(2, 0, 0)]);
        // What drives only a modulator's settings or the depth of a cable is not a cable into a plugin, and a
        // node that is no plugin (or is not there) has none.
        assert!(model.cables_into_plugin(third.node).is_empty());
        assert!(model.cables_into_plugin(model.groups[0].node).is_empty());
        assert!(model.cables_into_plugin(first.modulators[1].node).is_empty());
        assert!(model.cables_into_plugin(NodeId(9999)).is_empty());
    }

    /// What a rack's modulators can drive, one run per plugin: its node and the kind of target, with the
    /// parameter's own index left out.
    fn plugins_driven(model: &SessionModel, at: ModulatorLocation) -> Vec<(NodeId, K)> {
        let mut runs: Vec<(NodeId, K)> = Vec::new();
        for target in model.param_targets(at) {
            let shape = match target.kind {
                K::PluginParam { slot, .. } => K::PluginParam { slot, param_index: 0 },
                K::GroupMember { member, slot, .. } => K::GroupMember { member, slot, param_index: 0 },
                K::GroupBus { effect_index, .. } => K::GroupBus { effect_index, param_index: 0 },
                other => panic!("{other:?} is not a parameter"),
            };
            if runs.last() != Some(&(target.node, shape.clone())) {
                runs.push((target.node, shape));
            }
        }
        runs
    }

    #[test]
    fn a_lane_modulator_can_drive_every_parameter_of_its_lanes_chain_as_the_plugins_have_them() {
        use crate::app::testutil::GROUPS;
        let mut model = load(GROUPS).model;
        // (A value the user has set is not the parameter's default.)
        model.instruments[1].instrument.as_mut().unwrap().params[1].value = 0.77;
        let lane = &model.instruments[1];
        let (osc, filter) = (lane.instrument.as_ref().unwrap(), &lane.effects[0]);
        let targets = model.param_targets(ModulatorLocation::Lane(1));

        let expected: Vec<_> =
            osc.params.iter().map(|row| (osc, 0, row)).chain(filter.params.iter().map(|row| (filter, 1, row))).collect();
        assert_eq!(targets.len(), expected.len());
        for (target, (plugin, slot, row)) in targets.iter().zip(expected) {
            assert_eq!((target.node, target.slot, target.param), (plugin.node, slot, row.index));
            assert_eq!(target.kind, K::PluginParam { slot, param_index: row.index });
            assert_eq!((target.plugin_name.as_str(), target.param_name.as_str()), (plugin.name.as_str(), row.name.as_str()));
            assert_eq!((target.base_value, target.param_min, target.param_max), (row.value, row.min, row.max));
        }
        assert_eq!(plugins_driven(&model, ModulatorLocation::Lane(1)), [(lane.node, K::PluginParam { slot: 0, param_index: 0 }), (filter.node, K::PluginParam { slot: 1, param_index: 0 })]);
    }

    #[test]
    fn a_group_modulator_can_drive_its_members_chains_in_lane_order_and_then_its_bus_effects() {
        use crate::app::testutil::GROUPS;
        let model = load(GROUPS).model;
        let (first, second) = (&model.instruments[0], &model.instruments[1]);
        // (Lane 2 is in no group: none of it is the group's to drive.)
        assert_eq!(
            plugins_driven(&model, ModulatorLocation::Group(0)),
            [
                (first.node, K::GroupMember { member: 0, slot: 0, param_index: 0 }),
                (second.node, K::GroupMember { member: 1, slot: 0, param_index: 0 }),
                (second.effects[0].node, K::GroupMember { member: 1, slot: 1, param_index: 0 }),
                (model.groups[0].effects[0].node, K::GroupBus { effect_index: 0, param_index: 0 }),
            ]
        );
        // Each parameter of the bus reverb, by its own index, with the plugin's name.
        let reverb = &model.groups[0].effects[0];
        let bus: Vec<_> = model.param_targets(ModulatorLocation::Group(0)).into_iter().filter(|t| matches!(t.kind, K::GroupBus { .. })).collect();
        assert_eq!(bus.iter().map(|t| t.param).collect::<Vec<_>>(), reverb.params.iter().map(|row| row.index).collect::<Vec<_>>());
        assert!(bus.iter().all(|t| t.slot == 0 && t.plugin_name == reverb.name));
    }

    #[test]
    fn a_group_modulator_counts_members_among_its_own_lanes_and_bus_effects_by_their_place() {
        let mut model = load(
            r#"
[[group]]
[[group.effect]]
plugin = "builtin:reverb"
[[group.effect]]
plugin = "builtin:filter"
[[group]]
[[instrument]]
plugin = "builtin:osc"
[[instrument]]
plugin = "builtin:osc"
group = 1
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument]]
plugin = "builtin:osc"
group = 1
"#,
        )
        .model;
        let lane = |i: usize| model.instruments[i].node;
        let bus = |i: usize| model.groups[0].effects[i].node;
        // The first group: the one lane in it (the third, but its first member) and its two bus effects.
        assert_eq!(
            plugins_driven(&model, ModulatorLocation::Group(0)),
            [
                (lane(2), K::GroupMember { member: 0, slot: 0, param_index: 0 }),
                (bus(0), K::GroupBus { effect_index: 0, param_index: 0 }),
                (bus(1), K::GroupBus { effect_index: 1, param_index: 0 }),
            ]
        );
        // The second: its two lanes, and none of the first group's.
        assert_eq!(
            plugins_driven(&model, ModulatorLocation::Group(1)),
            [(lane(1), K::GroupMember { member: 0, slot: 0, param_index: 0 }), (lane(3), K::GroupMember { member: 1, slot: 0, param_index: 0 })]
        );
        // A lane that says it is in a group that is not there makes no members of one.
        model.instruments[0].group = Some(7);
        assert!(model.param_targets(ModulatorLocation::Group(7)).is_empty());
    }

    // -- What a modulator can drive of the others' --

    /// What a list of field targets says, one row each: whose, what it is called, how the rack addresses it, its range and where it is centred.
    fn field_rows(targets: &[FieldTarget]) -> Vec<(String, String, K, f32, f32, f32)> {
        targets.iter().map(|t| (t.owner.clone(), t.field_name.clone(), t.kind.clone(), t.param_min, t.param_max, t.base_value)).collect()
    }

    #[test]
    fn a_lane_modulator_can_drive_the_settings_of_the_others_in_its_rack_and_the_depths_of_their_cables_and_not_its_own() {
        use crate::app::testutil::CROSS_MOD_RACKS;
        let model = load(CROSS_MOD_RACKS).model;
        let rack = &model.instruments[0].modulators;
        let mut expected = vec![];
        let row = |owner: &str, name: &str, kind: K, min: f32, max: f32, base: f32| (owner.to_string(), name.to_string(), kind, min, max, base);
        // The envelope (1) has four stages, which an LFO does not; the MIDI control (2) has no settings, only the depth of its cable.
        expected.push(row("Mod 1", "attack", K::ModulatorAttack { mod_index: 1 }, 0.001, 10.0, 0.01));
        expected.push(row("Mod 1", "decay", K::ModulatorDecay { mod_index: 1 }, 0.001, 10.0, 0.3));
        expected.push(row("Mod 1", "sustain", K::ModulatorSustain { mod_index: 1 }, 0.0, 1.0, 0.4));
        expected.push(row("Mod 1", "release", K::ModulatorRelease { mod_index: 1 }, 0.001, 10.0, 0.5));
        expected.push(row("Mod 1", "volume depth", K::ModulatorDepth { mod_index: 1, target_index: 0 }, -1.0, 1.0, 0.2));
        expected.push(row("Mod 1", "detune depth", K::ModulatorDepth { mod_index: 1, target_index: 1 }, -1.0, 1.0, 0.7));
        expected.push(row("Mod 2", "pan depth", K::ModulatorDepth { mod_index: 2, target_index: 0 }, -1.0, 1.0, 0.3));
        let targets = model.field_targets(ModulatorLocation::Lane(0), 0);
        assert_eq!(field_rows(&targets), expected);
        // Each is the field of the modulator it says, by its node; a depth names its cable.
        let nodes: Vec<NodeId> = targets.iter().map(|t| t.node).collect();
        assert_eq!(nodes, [rack[1].node, rack[1].node, rack[1].node, rack[1].node, rack[1].node, rack[1].node, rack[2].node]);
        let fields: Vec<(CrossModField, Option<EdgeId>)> = targets.iter().map(|t| (t.field, t.edge)).collect();
        assert_eq!(
            fields,
            [
                (CrossModField::Attack, None),
                (CrossModField::Decay, None),
                (CrossModField::Sustain, None),
                (CrossModField::Release, None),
                (CrossModField::Depth(0), Some(rack[1].targets[0].edge)),
                (CrossModField::Depth(1), Some(rack[1].targets[1].edge)),
                (CrossModField::Depth(0), Some(rack[2].targets[0].edge)),
            ]
        );
        // And what it is called when it is bound: the owner and the name.
        assert_eq!(targets[0].label, "Mod 1 attack");
        assert_eq!(targets[4].label, "Mod 1 volume depth");
        assert_eq!(targets[5].label, "Mod 1 detune depth");
    }

    #[test]
    fn what_a_lane_modulator_can_drive_depends_on_which_it_is() {
        use crate::app::testutil::CROSS_MOD_RACKS;
        let model = load(CROSS_MOD_RACKS).model;
        // The envelope (1) can drive the LFO's rate and the depth of its cable, and the MIDI control's cable.
        let rows = field_rows(&model.field_targets(ModulatorLocation::Lane(0), 1));
        let names: Vec<(&str, &str)> = rows.iter().map(|r| (r.0.as_str(), r.1.as_str())).collect();
        assert_eq!(names, [("Mod 0", "rate"), ("Mod 0", "detune depth"), ("Mod 2", "pan depth")]);
        assert_eq!(rows[0].2, K::ModulatorRate { mod_index: 0 });
        // The MIDI control (2) can drive all of the others'.
        let names: Vec<String> = field_rows(&model.field_targets(ModulatorLocation::Lane(0), 2)).iter().map(|r| format!("{} {}", r.0, r.1)).collect();
        assert_eq!(
            names,
            ["Mod 0 rate", "Mod 0 detune depth", "Mod 1 attack", "Mod 1 decay", "Mod 1 sustain", "Mod 1 release", "Mod 1 volume depth", "Mod 1 detune depth"]
        );
        // A lane with one modulator has nothing to drive of the others'.
        assert!(model.field_targets(ModulatorLocation::Lane(1), 0).is_empty());
    }

    #[test]
    fn a_group_modulator_can_drive_the_settings_of_its_members_modulators_and_then_those_of_the_others_in_its_rack() {
        use crate::app::testutil::CROSS_MOD_RACKS;
        let model = load(CROSS_MOD_RACKS).model;
        let targets = model.field_targets(ModulatorLocation::Group(0), 0);
        let names: Vec<String> = field_rows(&targets).iter().map(|r| format!("{} {}", r.0, r.1)).collect();
        assert_eq!(
            names,
            [
                "M0 Mod0 rate",
                "M0 Mod0 detune depth",
                "M0 Mod1 attack",
                "M0 Mod1 decay",
                "M0 Mod1 sustain",
                "M0 Mod1 release",
                "M0 Mod1 volume depth",
                "M0 Mod1 detune depth",
                "M0 Mod2 pan depth",
                "M1 Mod0 rate",
                "Mod 1 attack",
                "Mod 1 decay",
                "Mod 1 sustain",
                "Mod 1 release",
                "Mod 1 damping depth",
                "Mod 2 volume depth",
            ]
        );
        // A member's modulator is addressed by the member's place in the group and its own in the lane's rack; the group's
        // own are addressed as a lane's are.
        assert_eq!(targets[0].kind, K::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Rate });
        assert_eq!(targets[9].kind, K::GroupMemberMod { member: 1, mod_index: 0, field: CrossModField::Rate });
        assert_eq!(targets[10].kind, K::ModulatorAttack { mod_index: 1 });
        // A depth is of the cable at its place in the modulator.
        assert_eq!(targets[6].kind, K::GroupMemberMod { member: 0, mod_index: 1, field: CrossModField::Depth(0) });
        assert_eq!(targets[7].kind, K::GroupMemberMod { member: 0, mod_index: 1, field: CrossModField::Depth(1) });
        // The node of each is the modulator that has the field: the lane's own for a member's, the group's for the rest.
        assert_eq!(targets[0].node, model.instruments[0].modulators[0].node);
        assert_eq!(targets[9].node, model.instruments[1].modulators[0].node);
        assert_eq!(targets[10].node, model.groups[0].modulators[1].node);
        // The depth of a member's cable and of the group's own name the cable.
        assert_eq!(targets[1].edge, Some(model.instruments[0].modulators[0].targets[0].edge));
        assert_eq!(targets[14].edge, Some(model.groups[0].modulators[1].targets[0].edge));
        // The lanes that are in no group, or in another, are not the group's: the second group's own member is its first.
        let second: Vec<String> = field_rows(&model.field_targets(ModulatorLocation::Group(1), 0)).iter().map(|r| format!("{} {}", r.0, r.1)).collect();
        assert_eq!(second, ["M0 Mod0 rate"]);
        assert_eq!(model.field_targets(ModulatorLocation::Group(1), 0)[0].node, model.instruments[3].modulators[0].node);
    }

    #[test]
    fn a_rack_that_is_not_there_drives_no_settings_and_an_index_that_is_not_in_it_leaves_none_out() {
        use crate::app::testutil::CROSS_MOD_RACKS;
        let model = load(CROSS_MOD_RACKS).model;
        assert!(model.field_targets(ModulatorLocation::Lane(9), 0).is_empty());
        assert!(model.field_targets(ModulatorLocation::Group(9), 0).is_empty());
        // (A modulator that is not there is none of them: the others are all there is.)
        let all = model.field_targets(ModulatorLocation::Lane(0), 99);
        let each = [0, 1, 2].map(|i| model.field_targets(ModulatorLocation::Lane(0), i).len());
        assert!(all.len() > each[0] && all.len() > each[1] && all.len() > each[2]);
    }

    // -- Taking a cable out --

    use crate::app::testutil::{CABLES_THAT_DRIVE_CABLES, CABLES_THAT_DRIVE_EACH_OTHER};

    /// What the modulators of a rack drive: each cable's kind and depth.
    fn cables(rack: &[ModulatorSlot]) -> Vec<Vec<(K, f32)>> {
        rack.iter().map(|m| m.targets.iter().map(|t| (t.kind.clone(), t.depth)).collect()).collect()
    }

    fn depth_of(mod_index: usize, target_index: usize) -> K {
        K::ModulatorDepth { mod_index, target_index }
    }

    /// The edge of cable `target` of modulator `index` in lane `inst`.
    fn edge_of(model: &SessionModel, inst: usize, index: usize, target: usize) -> EdgeId {
        model.instruments[inst].modulators[index].targets[target].edge
    }

    #[test]
    fn a_cable_that_nothing_drives_is_taken_out_alone_and_the_depth_cables_of_the_later_ones_follow_them() {
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        let (volume, pan) = (edge_of(&model, 0, 0, 1), edge_of(&model, 0, 0, 2));
        let removals = model.remove_edge(volume).unwrap();
        assert_eq!(removals, [(ModulatorLocation::Lane(0), 0, 1)]);
        let rack = &model.instruments[0].modulators;
        assert_eq!(rack[0].targets.len(), 2);
        assert_eq!(rack[0].targets[1].edge, pan, "the cable that was third is the second, and is the same cable");
        // The second modulator drove the first's third cable's depth: it is the second now.
        assert_eq!(cables(rack)[1], [(depth_of(0, 0), 0.4), (depth_of(0, 1), 0.5)]);
        assert_eq!(cables(rack)[2], [(depth_of(1, 0), 0.6)]);
        model.check_ids().unwrap();
    }

    #[test]
    fn the_cables_that_drive_the_depth_of_a_cable_go_with_it_and_those_that_drive_theirs() {
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        let (detune, pan) = (edge_of(&model, 0, 0, 0), edge_of(&model, 0, 0, 2));
        let survivor = edge_of(&model, 0, 1, 1);
        let removals = model.remove_edge(detune).unwrap();
        // The third modulator's cable drove the depth of the second's first, which drove the depth of this one: the
        // deepest goes first, and each is told the place it has at its turn.
        assert_eq!(removals, [(ModulatorLocation::Lane(0), 2, 0), (ModulatorLocation::Lane(0), 1, 0), (ModulatorLocation::Lane(0), 0, 0)]);
        let rack = &model.instruments[0].modulators;
        assert_eq!(rack[0].targets.iter().map(|t| t.depth).collect::<Vec<_>>(), [0.2, 0.3], "volume and pan are left, in their order");
        assert_eq!(rack[0].targets.len(), 2);
        assert_eq!(rack[0].targets[1].edge, pan);
        // What is left of the second modulator is the cable that drove the first's third, now its second.
        assert_eq!(cables(rack)[1], [(depth_of(0, 1), 0.5)]);
        assert_eq!(rack[1].targets[0].edge, survivor);
        assert!(rack[2].targets.is_empty());
        model.check_ids().unwrap();
    }

    #[test]
    fn the_cables_of_a_group_that_drive_the_depth_of_a_members_cable_go_with_it() {
        use crate::app::testutil::CROSS_MOD;
        use crate::plugin::chain::CrossModField as F;
        let mut model = load(CROSS_MOD).model;
        // The member's first modulator drives one parameter. The member's third drives the depth of that cable, and
        // so does the group's second.
        let detune = edge_of(&model, 0, 0, 0);
        let group_cables_before = model.groups[0].modulators[1].targets.len();
        let removals = model.remove_edge(detune).unwrap();
        assert_eq!(
            removals,
            [(ModulatorLocation::Group(0), 1, 7), (ModulatorLocation::Lane(0), 2, 1), (ModulatorLocation::Lane(0), 0, 0)],
            "the group's cable, then the lane's, then the cable"
        );
        assert!(model.instruments[0].modulators[0].targets.is_empty());
        let third = &model.instruments[0].modulators[2];
        assert!(third.targets.iter().all(|t| !matches!(t.kind, K::ModulatorDepth { .. })), "{:?}", cables(std::slice::from_ref(third)));
        assert_eq!(third.targets.len(), 5);
        let group = &model.groups[0].modulators[1];
        assert_eq!(group.targets.len(), group_cables_before - 1);
        assert!(group.targets.iter().all(|t| !matches!(t.kind, K::GroupMemberMod { field: F::Depth(_), .. })));
        model.check_ids().unwrap();
    }

    #[test]
    fn a_members_cable_that_is_taken_out_moves_the_groups_cables_to_the_depths_of_its_later_ones_and_no_others() {
        use crate::plugin::chain::CrossModField as F;
        let cables = |count: usize| {
            (0..count).map(|i| format!("[[instrument.modulator.target]]\nparam = \"detune\"\ndepth = 0.{}\n", i + 1)).collect::<String>()
        };
        // Two members. The first has two modulators, the second one, each with three cables. The group's modulator
        // drives the depth of the first member's first modulator's first and third cables, of its second modulator's
        // third, and of the second member's modulator's third.
        let mut model = load(&format!(
            r#"
[[group]]
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
member = 0
mod_depth = [0, 0]
depth = 0.1
[[group.modulator.target]]
member = 0
mod_depth = [0, 2]
depth = 0.2
[[group.modulator.target]]
member = 0
mod_depth = [1, 2]
depth = 0.3
[[group.modulator.target]]
member = 1
mod_depth = [0, 2]
depth = 0.4

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 1.0
{}[[instrument.modulator]]
rate = 2.0
{}
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 3.0
{}"#,
            cables(3),
            cables(3),
            cables(3)
        ))
        .model;
        let drive = |member, mod_index, depth| K::GroupMemberMod { member, mod_index, field: F::Depth(depth) };
        let in_the_group = |model: &SessionModel| model.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).collect::<Vec<_>>();
        assert_eq!(in_the_group(&model), [drive(0, 0, 0), drive(0, 0, 2), drive(0, 1, 2), drive(1, 0, 2)]);

        // The first member's first modulator loses its second cable, which no cable of the group drives the depth of:
        // its third is its second now, and so is the group's cable that drove that one's depth. The cables that drive
        // the depths of the first member's other modulator's, and of the other member's, are where they were.
        let second = edge_of(&model, 0, 0, 1);
        assert_eq!(model.remove_edge(second), Some(vec![(ModulatorLocation::Lane(0), 0, 1)]));
        assert_eq!(in_the_group(&model), [drive(0, 0, 0), drive(0, 0, 1), drive(0, 1, 2), drive(1, 0, 2)]);

        // The second member's modulator loses its first cable: its third is its second, and its group cable follows.
        let first_of_second_member = edge_of(&model, 1, 0, 0);
        model.remove_edge(first_of_second_member).unwrap();
        assert_eq!(in_the_group(&model), [drive(0, 0, 0), drive(0, 0, 1), drive(0, 1, 2), drive(1, 0, 1)]);
        model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_of_a_group_modulator_is_taken_out_with_the_cables_that_drive_its_depth() {
        let mut model = load(
            r#"
[[group]]
[[group.effect]]
plugin = "builtin:reverb"
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
bus = 0
param = "size"
depth = 0.1
[[group.modulator.target]]
bus = 0
param = "damping"
depth = 0.2
[[group.modulator]]
rate = 2.0
[[group.modulator.target]]
mod_depth = [0, 0]
depth = 0.3
[[group.modulator.target]]
mod_depth = [0, 1]
depth = 0.4
"#,
        )
        .model;
        let size = model.groups[0].modulators[0].targets[0].edge;
        let removals = model.remove_edge(size).unwrap();
        assert_eq!(removals, [(ModulatorLocation::Group(0), 1, 0), (ModulatorLocation::Group(0), 0, 0)]);
        let rack = &model.groups[0].modulators;
        assert_eq!(rack[0].targets.len(), 1, "damping is left");
        assert_eq!(cables(rack)[1], [(depth_of(0, 0), 0.4)], "and what drove its depth, which is its first now");
        model.check_ids().unwrap();
    }

    #[test]
    fn cables_that_drive_each_others_depths_are_all_taken_out_and_it_ends() {
        let mut model = load(CABLES_THAT_DRIVE_EACH_OTHER).model;
        let first = edge_of(&model, 0, 0, 0);
        let removals = model.remove_edge(first).unwrap();
        // Each drives the depth of the other: the second goes first, and then the cable itself. What was after it is
        // the modulator's first cable now, and drove nothing of the others.
        assert_eq!(removals, [(ModulatorLocation::Lane(0), 1, 0), (ModulatorLocation::Lane(0), 0, 0)]);
        assert!(model.instruments[0].modulators[1].targets.is_empty());
        assert_eq!(model.instruments[0].modulators[0].targets.len(), 1, "the parameter it drives is left");
    }

    #[test]
    fn a_cable_that_is_not_there_is_not_taken_out_and_nothing_changes() {
        let mut model = load(CABLES_THAT_DRIVE_CABLES).model;
        let before = cables(&model.instruments[0].modulators);
        assert_eq!(model.remove_edge(EdgeId(999)), None);
        let gone = edge_of(&model, 0, 0, 1);
        assert!(model.remove_edge(gone).is_some());
        assert_eq!(model.remove_edge(gone), None, "it is not there the second time");
        assert_ne!(cables(&model.instruments[0].modulators), before, "(the first time did change it)");
    }

    // -- Lanes change places --

    /// The member each cable of the first group's first modulator drives, in order (a plugin's parameter or a
    /// modulator's rate), and the group of each lane.
    fn members_and_groups(model: &SessionModel) -> (Vec<usize>, Vec<Option<usize>>) {
        let members = model.groups[0].modulators[0]
            .targets
            .iter()
            .map(|t| match t.kind {
                K::GroupMember { member, .. } | K::GroupMemberMod { member, .. } => member,
                ref other => panic!("{other:?}"),
            })
            .collect();
        (members, model.instruments.iter().map(|lane| lane.group).collect())
    }

    #[test]
    fn two_lanes_that_swap_places_each_take_what_it_has_and_the_groups_cables_follow_the_members() {
        use crate::app::testutil::LANES_IN_GROUPS;
        let mut model = load(LANES_IN_GROUPS).model;
        // Group 0's members are lanes 0 and 3 (members 0 and 1); its modulator drives parameters of both, and the
        // rate of the second's first modulator.
        assert_eq!(members_and_groups(&model), (vec![0, 1, 1], vec![Some(0), None, Some(1), Some(0)]));
        let (ids, volumes): (Vec<NodeId>, Vec<f32>) = model.instruments.iter().map(|lane| (lane.node, lane.volume)).unzip();

        // Two members change places: so do the members the cables drive.
        assert!(model.swap_lanes(0, 3));
        assert_eq!(members_and_groups(&model), (vec![1, 0, 0], vec![Some(0), None, Some(1), Some(0)]));
        assert_eq!(model.instruments.iter().map(|lane| lane.node).collect::<Vec<_>>(), [ids[3], ids[1], ids[2], ids[0]], "each lane is its own node, and moved");
        assert_eq!(model.instruments.iter().map(|lane| lane.volume).collect::<Vec<_>>(), [volumes[3], volumes[1], volumes[2], volumes[0]]);
        assert_eq!(model.instruments[3].range, Some((12, 59)), "the range of the lane went with it");
        assert_eq!(model.instruments[0].effects.len(), 1);
        assert_eq!(model.locate(ids[0]), Some(NodePath::Instrument(3)), "the node is found where the lane is");
        model.check_ids().unwrap();

        // A member and a lane in no group: the member's place among the members does not change, the lane's group goes with it.
        assert!(model.swap_lanes(0, 1));
        assert_eq!(members_and_groups(&model), (vec![1, 0, 0], vec![None, Some(0), Some(1), Some(0)]));
        // A member goes past the other group's lane and then past the other member: they change places again.
        assert!(model.swap_lanes(1, 3));
        assert_eq!(members_and_groups(&model), (vec![0, 1, 1], vec![None, Some(0), Some(1), Some(0)]));
        model.check_ids().unwrap();
    }

    #[test]
    fn lanes_that_are_not_there_or_are_one_do_not_swap_and_nothing_changes() {
        use crate::app::testutil::LANES_IN_GROUPS;
        let mut model = load(LANES_IN_GROUPS).model;
        let before = format!("{:?}", model.instruments.iter().map(|lane| (lane.node, lane.group)).collect::<Vec<_>>());
        for (a, b) in [(1, 1), (0, 4), (7, 0), (9, 9)] {
            assert!(!model.swap_lanes(a, b), "{a} {b}");
        }
        assert_eq!(format!("{:?}", model.instruments.iter().map(|lane| (lane.node, lane.group)).collect::<Vec<_>>()), before);
        assert_eq!(members_and_groups(&model).0, [0, 1, 1]);
    }

    #[test]
    fn a_member_with_no_plugin_is_still_a_member_and_keeps_its_place() {
        let mut model = load("[[group]]\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n").model;
        model.instruments[0].instrument = None;
        let second = model.instruments[1].node;
        assert_eq!(plugins_driven(&model, ModulatorLocation::Group(0)), [(second, K::GroupMember { member: 1, slot: 0, param_index: 0 })]);
    }

    #[test]
    fn what_is_not_there_drives_nothing() {
        let mut model = load("[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        assert!(model.param_targets(ModulatorLocation::Lane(1)).is_empty(), "no such lane");
        assert!(model.param_targets(ModulatorLocation::Group(0)).is_empty(), "no such group");
        model.instruments[0].instrument = None;
        assert!(model.param_targets(ModulatorLocation::Lane(0)).is_empty(), "an empty lane has no chain");
        // A lane that says it is in a group that is not there does not make one.
        model.instruments[0].instrument = load("[[instrument]]\nplugin = \"builtin:osc\"\n").model.instruments.remove(0).instrument;
        model.instruments[0].group = Some(3);
        assert!(model.param_targets(ModulatorLocation::Group(3)).is_empty(), "no such group, whatever the lanes say");
    }

    #[test]
    fn a_separator_is_not_a_parameter_to_drive() {
        let mut model = load("[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        let plugin = model.instruments[0].instrument.as_mut().unwrap();
        let all = plugin.params.len();
        plugin.params[0].kind = ParamKind::Separator;
        assert_eq!(model.param_targets(ModulatorLocation::Lane(0)).len(), all - 1);
    }

    fn target(kind: K) -> ModTargetSlot {
        ModTargetSlot {
            edge: EdgeId(0),
            slot: 0,
            param_name: "p".to_string(),
            kind,
            depth: 0.5,
            param_min: 0.0,
            param_max: 1.0,
            effective: None,
        }
    }

    fn lfo(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            node: NodeId(0),
            source: ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate: 2.0 },
            targets,
            output: None,
        }
    }

    fn adsr(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            node: NodeId(0),
            source: ModSourceSlot::Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 },
            targets,
            output: None,
        }
    }

    fn midi(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            node: NodeId(0),
            source: ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(74)), learning: false },
            targets,
            output: None,
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

    /// A lane modulator target on chain `slot` (0 = instrument, 1.. = effects).
    fn plugin_target(slot: usize, param_index: u32) -> ModTargetSlot {
        ModTargetSlot { slot, ..target(K::PluginParam { slot, param_index }) }
    }

    /// Each target's own `slot` field and its kind, as the tree and the save code see them.
    fn slots(m: &ModulatorSlot) -> Vec<(usize, String)> {
        m.targets.iter().map(|t| (t.slot, format!("{:?}", t.kind))).collect()
    }

    #[test]
    fn slot_fixup_after_effect_removal_drops_that_slot_and_shifts_later_ones() {
        // Chain: instrument, then effects 0, 1, 2 at slots 1, 2, 3. Effect 0 is removed.
        let mut mods = vec![lfo(vec![
            plugin_target(0, 1),
            plugin_target(1, 5),
            plugin_target(2, 6),
            plugin_target(3, 7),
            target(K::ModulatorRate { mod_index: 1 }),
            target(K::GroupMember { member: 0, slot: 2, param_index: 4 }),
        ])];
        fixup_slots_after_remove(&mut mods, 0);
        assert_eq!(
            slots(&mods[0]),
            [
                (0, "PluginParam { slot: 0, param_index: 1 }".to_string()),
                (1, "PluginParam { slot: 1, param_index: 6 }".to_string()),
                (2, "PluginParam { slot: 2, param_index: 7 }".to_string()),
                (0, "ModulatorRate { mod_index: 1 }".to_string()),
                (0, "GroupMember { member: 0, slot: 2, param_index: 4 }".to_string()),
            ]
        );
        // Removing the last effect only drops its own targets.
        fixup_slots_after_remove(&mut mods, 1);
        assert_eq!(
            slots(&mods[0]).iter().map(|(slot, _)| *slot).collect::<Vec<_>>(),
            [0, 1, 0, 0],
        );
    }

    #[test]
    fn slot_remap_after_reorder_follows_the_plugins() {
        // Effects A, B, C at slots 1, 2, 3, each with one target; A moves to the end.
        let mut mods = vec![lfo(vec![
            plugin_target(0, 9),
            plugin_target(1, 10),
            plugin_target(2, 11),
            plugin_target(3, 12),
            target(K::ModulatorRate { mod_index: 0 }),
        ])];
        remap_slots_after_reorder(&mut mods, 0, 2);
        let slot_of = |m: &ModulatorSlot| m.targets.iter().map(|t| t.slot).collect::<Vec<_>>();
        assert_eq!(slot_of(&mods[0]), [0, 3, 1, 2, 0], "A -> 3, B -> 1, C -> 2");
        // The kind agrees with the target's own slot, and the parameters are unchanged.
        assert_eq!(
            kinds(&mods[0])[1..4],
            [
                "PluginParam { slot: 3, param_index: 10 }",
                "PluginParam { slot: 1, param_index: 11 }",
                "PluginParam { slot: 2, param_index: 12 }",
            ]
        );
        // Moving it back restores the original slots.
        remap_slots_after_reorder(&mut mods, 2, 0);
        assert_eq!(slot_of(&mods[0]), [0, 1, 2, 3, 0]);
        // Moving an effect onto itself changes nothing.
        remap_slots_after_reorder(&mut mods, 1, 1);
        assert_eq!(slot_of(&mods[0]), [0, 1, 2, 3, 0]);
    }

    /// A group target on member `member`'s chain `slot`, as the target selector makes it.
    fn member_target(member: usize, slot: usize, param_index: u32) -> ModTargetSlot {
        ModTargetSlot { slot, ..target(K::GroupMember { member, slot, param_index }) }
    }

    #[test]
    fn member_slot_fixup_after_effect_removal_touches_only_that_members_slots() {
        // Member 0 has effects at slots 1..=3, member 1 one at slot 1. Effect 0 of member 0 is removed.
        let mut mods = vec![lfo(vec![
            member_target(0, 0, 1),
            member_target(0, 1, 5),
            member_target(0, 2, 6),
            member_target(0, 3, 7),
            member_target(1, 1, 8),
            member_target(1, 2, 9),
            plugin_target(2, 4),
            target(K::GroupBus { effect_index: 2, param_index: 3 }),
        ])];
        fixup_member_slots_after_remove(&mut mods, 0, 0);
        assert_eq!(
            slots(&mods[0]),
            [
                (0, "GroupMember { member: 0, slot: 0, param_index: 1 }".to_string()),
                (1, "GroupMember { member: 0, slot: 1, param_index: 6 }".to_string()),
                (2, "GroupMember { member: 0, slot: 2, param_index: 7 }".to_string()),
                (1, "GroupMember { member: 1, slot: 1, param_index: 8 }".to_string()),
                (2, "GroupMember { member: 1, slot: 2, param_index: 9 }".to_string()),
                (2, "PluginParam { slot: 2, param_index: 4 }".to_string()),
                (0, "GroupBus { effect_index: 2, param_index: 3 }".to_string()),
            ],
            "the removed slot's target is gone, later slots of that member move down, nothing else does"
        );
        // Removing what is now the last effect of member 0 only drops its own targets.
        fixup_member_slots_after_remove(&mut mods, 0, 1);
        assert_eq!(
            kinds(&mods[0]),
            [
                "GroupMember { member: 0, slot: 0, param_index: 1 }",
                "GroupMember { member: 0, slot: 1, param_index: 6 }",
                "GroupMember { member: 1, slot: 1, param_index: 8 }",
                "GroupMember { member: 1, slot: 2, param_index: 9 }",
                "PluginParam { slot: 2, param_index: 4 }",
                "GroupBus { effect_index: 2, param_index: 3 }",
            ]
        );
        // The other racks' targets move too.
        let mut racks = vec![lfo(vec![member_target(0, 2, 1)]), adsr(vec![member_target(0, 3, 2)])];
        fixup_member_slots_after_remove(&mut racks, 0, 0);
        assert_eq!((racks[0].targets[0].slot, racks[1].targets[0].slot), (1, 2));
    }

    #[test]
    fn member_slot_remap_after_reorder_follows_the_plugins_of_that_member() {
        // Member 0's effects A, B, C at slots 1, 2, 3, each with a target; A moves to the end.
        let mut mods = vec![lfo(vec![
            member_target(0, 0, 9),
            member_target(0, 1, 10),
            member_target(0, 2, 11),
            member_target(0, 3, 12),
            member_target(1, 1, 13),
            member_target(1, 2, 14),
            plugin_target(1, 15),
        ])];
        remap_member_slots_after_reorder(&mut mods, 0, 0, 2);
        let slot_of = |m: &ModulatorSlot| m.targets.iter().map(|t| t.slot).collect::<Vec<_>>();
        assert_eq!(slot_of(&mods[0]), [0, 3, 1, 2, 1, 2, 1], "A -> 3, B -> 1, C -> 2; member 1 and the lane's own stay");
        // The kind agrees with the target's own slot, and the parameters are unchanged.
        assert_eq!(
            kinds(&mods[0])[1..4],
            [
                "GroupMember { member: 0, slot: 3, param_index: 10 }",
                "GroupMember { member: 0, slot: 1, param_index: 11 }",
                "GroupMember { member: 0, slot: 2, param_index: 12 }",
            ]
        );
        // Moving it back restores the original slots; moving an effect onto itself changes nothing.
        remap_member_slots_after_reorder(&mut mods, 0, 2, 0);
        assert_eq!(slot_of(&mods[0]), [0, 1, 2, 3, 1, 2, 1]);
        remap_member_slots_after_reorder(&mut mods, 0, 1, 1);
        assert_eq!(slot_of(&mods[0]), [0, 1, 2, 3, 1, 2, 1]);
        // The other way: the last effect moves to the front.
        remap_member_slots_after_reorder(&mut mods, 0, 2, 0);
        assert_eq!(slot_of(&mods[0]), [0, 2, 3, 1, 1, 2, 1], "C -> 1, A -> 2, B -> 3");
    }

    /// An ungrouped lane with a filter and a reverb, then a group member (lane 1,
    /// member 0) with the same, driven by a lane LFO and by a group LFO that each
    /// have a target on both: told apart by parameter index, `cutoff` (0) is the
    /// filter's and `damping` (2) the reverb's.
    const MEMBER_WITH_EFFECTS: &str = r#"
[[group]]
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

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.5
[[instrument.modulator.target]]
slot = 2
param = "damping"
depth = 0.5
"#;

    fn lane_and_group_slots(model: &SessionModel) -> (Vec<usize>, Vec<usize>) {
        let of = |m: &ModulatorSlot| m.targets.iter().map(|t| t.slot).collect::<Vec<_>>();
        (of(&model.instruments[1].modulators[0]), of(&model.groups[0].modulators[0]))
    }

    #[test]
    fn removing_an_effect_moves_the_targets_of_its_lane_and_of_its_group() {
        let mut model = load(MEMBER_WITH_EFFECTS).model;
        assert_eq!(lane_and_group_slots(&model), ([1, 2].to_vec(), [1, 2].to_vec()));
        let removed = model.remove_effect(1, 0).expect("the filter");
        assert_eq!(removed.name, "Filter");
        assert_eq!(model.instruments[1].effects.len(), 1);
        assert_eq!(model.instruments[1].effects[0].name, "Reverb");
        // The reverb's `damping` is in slot 1 now, in both racks, and the filter's targets are gone.
        assert_eq!(lane_and_group_slots(&model), ([1].to_vec(), [1].to_vec()));
        assert_eq!(kinds(&model.instruments[1].modulators[0]), ["PluginParam { slot: 1, param_index: 2 }"]);
        assert_eq!(kinds(&model.groups[0].modulators[0]), ["GroupMember { member: 0, slot: 1, param_index: 2 }"]);
    }

    #[test]
    fn removing_an_effect_of_an_ungrouped_lane_leaves_the_groups_targets() {
        let mut model = load(MEMBER_WITH_EFFECTS).model;
        assert_eq!(model.remove_effect(0, 0).expect("the ungrouped lane's filter").name, "Filter");
        assert_eq!(model.instruments[0].effects.len(), 1);
        assert_eq!(lane_and_group_slots(&model), ([1, 2].to_vec(), [1, 2].to_vec()));
    }

    #[test]
    fn removing_an_effect_that_is_not_there_changes_nothing() {
        let mut model = load(MEMBER_WITH_EFFECTS).model;
        assert!(model.remove_effect(1, 2).is_none(), "the chain has two effects");
        assert!(model.remove_effect(1, 9).is_none());
        assert!(model.remove_effect(0, 2).is_none(), "the ungrouped lane has two");
        assert!(model.remove_effect(9, 0).is_none(), "there is no lane 9");
        assert_eq!(model.instruments[1].effects.len(), 2);
        assert_eq!(model.instruments[0].effects.len(), 2);
        assert_eq!(lane_and_group_slots(&model), ([1, 2].to_vec(), [1, 2].to_vec()));
    }

    #[test]
    fn the_cables_into_a_lane_are_those_of_its_group_to_the_lane_and_its_modulators() {
        use crate::app::testutil::{CHAINS, CROSS_MOD, LANES_IN_GROUPS};
        let model = load(CHAINS).model;
        let group = |m: usize, t: usize| model.groups[0].modulators[m].targets[t].edge;
        // The first member: the group's cables to its filter, its reverb and its first modulator's rate (not to the
        // bus effects, nor the second member's detune, nor the cables that drive the depth of the group's own).
        assert_eq!(model.group_cables_into_lane(0), [group(0, 0), group(0, 1), group(0, 5)]);
        assert_eq!(model.group_cables_into_lane(1), [group(0, 2)]);
        assert!(model.group_cables_into_lane(2).is_empty(), "a lane in no group has none");
        assert!(model.group_cables_into_lane(9).is_empty());
        // A cable to the depth of a cable of a member's modulator is one into the member.
        let model = load(CROSS_MOD).model;
        assert_eq!(model.group_cables_into_lane(0).len(), 3, "the rate, the attack and the depth of the first modulator");
        // The members are counted in their group, whatever lanes are between them.
        let model = load(LANES_IN_GROUPS).model;
        assert_eq!(model.group_cables_into_lane(0).len(), 1); // group 0's member 0: its detune
        assert_eq!(model.group_cables_into_lane(3).len(), 2); // group 0's member 1: its pan and the rate of its modulator
        assert_eq!(model.group_cables_into_lane(2).len(), 1); // group 1's only member: its volume (not group 0's cables to member 0)
    }

    #[test]
    fn removing_a_lane_takes_it_with_everything_it_has_and_moves_the_members_after_it_down() {
        use crate::app::testutil::CHAINS;
        let mut model = load(CHAINS).model;
        let first = model.instruments[0].node;
        let removed = model.remove_lane(0).expect("the first lane is there");
        assert_eq!((removed.node, removed.effects.len(), removed.modulators.len()), (first, 2, 2));
        assert_eq!(model.instruments.len(), 2);
        // The second member is the first now: the group's cable to its detune says member 0. (The cables to the
        // removed member went with it, which `Controller::remove_lane` makes sure of before: here they are dropped
        // the way the audio thread drops them.)
        let kinds: Vec<K> = model.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).collect();
        assert!(kinds.contains(&K::GroupMember { member: 0, slot: 0, param_index: 1 }), "{kinds:?}");
        assert!(!kinds.iter().any(|k| matches!(k, K::GroupMember { member: 1, .. })), "{kinds:?}");
        assert!(model.remove_lane(2).is_none() && model.remove_lane(9).is_none(), "no lane 2 now");
        assert_eq!(model.instruments.len(), 2);
    }

    #[test]
    fn removing_a_group_leaves_its_members_in_no_group_and_numbers_the_later_groups_again() {
        use crate::app::testutil::LANES_IN_GROUPS;
        let mut model = load(LANES_IN_GROUPS).model;
        let groups: Vec<_> = model.instruments.iter().map(|lane| lane.group).collect();
        assert_eq!(groups, [Some(0), None, Some(1), Some(0)]);
        let first = model.groups[0].node;
        let removed = model.remove_group(0).expect("the first group is there");
        assert_eq!((removed.node, removed.modulators.len()), (first, 1));
        assert_eq!(model.groups.len(), 1);
        let groups: Vec<_> = model.instruments.iter().map(|lane| lane.group).collect();
        assert_eq!(groups, [None, None, Some(0), None], "its members are in none, and the second group is the first now");
        assert!(model.remove_group(1).is_none() && model.remove_group(9).is_none());
        assert_eq!(model.groups.len(), 1);
        // The last one: nothing is left.
        assert!(model.remove_group(0).is_some());
        assert!(model.groups.is_empty() && model.instruments.iter().all(|lane| lane.group.is_none()));
    }

    #[test]
    fn removing_a_bus_effect_drops_the_groups_targets_on_it_and_moves_the_later_ones_down() {
        use crate::app::testutil::CHAINS;
        let mut model = load(CHAINS).model;
        let bus_cables = |model: &SessionModel| -> Vec<K> { model.groups[0].modulators[0].targets.iter().map(|t| t.kind.clone()).filter(|kind| matches!(kind, K::GroupBus { .. })).collect() };
        assert_eq!(bus_cables(&model), [K::GroupBus { effect_index: 0, param_index: 0 }, K::GroupBus { effect_index: 1, param_index: 2 }]);
        let filter = model.groups[0].effects[0].node;
        let removed = model.remove_bus_effect(0, 0).expect("the filter is there");
        assert_eq!(removed.node, filter);
        assert_eq!(model.groups[0].effects.len(), 1);
        // The cable to the filter is gone, and the one to the reverb has its place now.
        assert_eq!(bus_cables(&model), [K::GroupBus { effect_index: 0, param_index: 2 }]);
        // The reverb is the only one left, and the last: nothing moves after it.
        assert!(model.remove_bus_effect(0, 0).is_some());
        assert!(bus_cables(&model).is_empty());
    }

    #[test]
    fn removing_a_bus_effect_that_is_not_there_changes_nothing() {
        use crate::app::testutil::CHAINS;
        let mut model = load(CHAINS).model;
        assert!(model.remove_bus_effect(0, 2).is_none(), "the bus has two effects");
        assert!(model.remove_bus_effect(1, 0).is_none(), "there is no group 1");
        assert_eq!(model.groups[0].effects.len(), 2);
        assert_eq!(model.groups[0].modulators[0].targets.len(), 6);
    }

    #[test]
    fn moving_an_effect_moves_the_targets_of_its_lane_and_of_its_group_with_it() {
        let mut model = load(MEMBER_WITH_EFFECTS).model;
        assert!(model.move_effect(1, 0, 1));
        let names: Vec<&str> = model.instruments[1].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Reverb", "Filter"]);
        // `cutoff` followed the filter to slot 2, `damping` the reverb to slot 1.
        assert_eq!(lane_and_group_slots(&model), ([2, 1].to_vec(), [2, 1].to_vec()));
        assert_eq!(
            kinds(&model.groups[0].modulators[0]),
            ["GroupMember { member: 0, slot: 2, param_index: 0 }", "GroupMember { member: 0, slot: 1, param_index: 2 }"]
        );

        // Back again, the other way round.
        assert!(model.move_effect(1, 1, 0));
        let names: Vec<&str> = model.instruments[1].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Filter", "Reverb"]);
        assert_eq!(lane_and_group_slots(&model), ([1, 2].to_vec(), [1, 2].to_vec()));
    }

    #[test]
    fn moving_an_effect_of_an_ungrouped_lane_leaves_the_groups_targets() {
        let mut model = load(MEMBER_WITH_EFFECTS).model;
        assert!(model.move_effect(0, 0, 1));
        let names: Vec<&str> = model.instruments[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Reverb", "Filter"]);
        assert_eq!(lane_and_group_slots(&model), ([1, 2].to_vec(), [1, 2].to_vec()));
    }

    #[test]
    fn moving_an_effect_nowhere_changes_nothing() {
        let mut model = load(MEMBER_WITH_EFFECTS).model;
        assert!(!model.move_effect(1, 0, 0), "onto itself");
        assert!(!model.move_effect(1, 0, 2), "past the end");
        assert!(!model.move_effect(1, 2, 0), "from past the end");
        assert!(!model.move_effect(9, 0, 1), "no such lane");
        let names: Vec<&str> = model.instruments[1].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Filter", "Reverb"]);
        assert_eq!(lane_and_group_slots(&model), ([1, 2].to_vec(), [1, 2].to_vec()));
    }

    #[test]
    fn bus_remap_after_reorder_follows_the_effects() {
        let mut mods = vec![lfo(vec![
            target(K::GroupBus { effect_index: 0, param_index: 1 }),
            target(K::GroupBus { effect_index: 1, param_index: 2 }),
            target(K::GroupBus { effect_index: 2, param_index: 3 }),
            target(K::PluginParam { slot: 2, param_index: 4 }),
            target(K::GroupMember { member: 1, slot: 2, param_index: 5 }),
        ])];
        remap_group_bus_after_reorder(&mut mods, 2, 0);
        assert_eq!(
            kinds(&mods[0]),
            [
                "GroupBus { effect_index: 1, param_index: 1 }",
                "GroupBus { effect_index: 2, param_index: 2 }",
                "GroupBus { effect_index: 0, param_index: 3 }",
                "PluginParam { slot: 2, param_index: 4 }",
                "GroupMember { member: 1, slot: 2, param_index: 5 }",
            ]
        );
    }

    #[test]
    fn remap_index_agrees_with_actually_moving_an_element() {
        for n in 1..=6usize {
            for from in 0..n {
                for to in 0..n {
                    let mut chain: Vec<usize> = (0..n).collect();
                    let moved = chain.remove(from);
                    chain.insert(to, moved);
                    for original in 0..n {
                        assert_eq!(
                            chain[remap_index(original, from, to)],
                            original,
                            "n={n} from={from} to={to}: element {original} should land where the move put it"
                        );
                    }
                }
            }
        }
    }

    fn node(group: Option<usize>) -> InstrumentNode {
        InstrumentNode {
            node: NodeId(0),
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
            meter: None,
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
    fn new_modulators_start_as_a_sine_of_one_hertz_or_the_default_envelope() {
        use crate::plugin::chain::LfoWaveform;
        assert!(matches!(
            ModSourceSlot::new_lfo(),
            ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate } if rate == 1.0
        ));
        assert!(matches!(
            ModSourceSlot::new_envelope(),
            ModSourceSlot::Envelope { attack, decay, sustain, release }
                if (attack, decay, sustain, release) == (0.01, 0.3, 0.7, 0.5)
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
    fn a_row_of_the_pane_is_what_the_layout_of_its_source_says() {
        use ModulatorField as F;
        use ModulatorRow::*;
        let two = || vec![target(K::PluginParam { slot: 0, param_index: 0 }), target(K::PluginParam { slot: 0, param_index: 1 })];
        // (One row past the end too: there is nothing there.)
        let rows = |m: &ModulatorSlot| (0..=modulator_param_len(m)).map(|r| modulator_row(m, r)).collect::<Vec<_>>();
        assert_eq!(
            rows(&lfo(two())),
            [Some(Type), Some(Waveform), Some(Setting(F::Rate)), Some(Separator), Some(Depth(0)), Some(Depth(1)), None]
        );
        assert_eq!(
            rows(&adsr(two())),
            [
                Some(Type),
                Some(Setting(F::Attack)),
                Some(Setting(F::Decay)),
                Some(Setting(F::Sustain)),
                Some(Setting(F::Release)),
                Some(Separator),
                Some(Depth(0)),
                Some(Depth(1)),
                None
            ]
        );
        assert_eq!(rows(&midi(two())), [Some(Info), Some(Separator), Some(Depth(0)), Some(Depth(1)), None]);
        assert_eq!(rows(&lfo(vec![])), [Some(Type), Some(Waveform), Some(Setting(F::Rate)), Some(Separator), None]);
    }

    #[test]
    fn the_rows_that_are_numbers_have_the_range_the_controller_clamps_to() {
        use ModulatorField as F;
        let two = || vec![target(K::PluginParam { slot: 0, param_index: 0 })];
        for (m, field, row) in [
            (lfo(two()), F::Rate, 2),
            (adsr(two()), F::Attack, 1),
            (adsr(two()), F::Decay, 2),
            (adsr(two()), F::Sustain, 3),
            (adsr(two()), F::Release, 4),
        ] {
            assert_eq!(modulator_param_range(&m, row), Some(field.range()), "{field:?}");
        }
    }

    #[test]
    fn a_source_says_the_value_of_a_setting_it_has_and_nothing_of_one_it_has_not() {
        use ModulatorField as F;
        let triangle = ModSourceSlot::Lfo { waveform: LfoWaveform::Triangle, rate: 2.5 };
        let envelope = ModSourceSlot::Envelope { attack: 0.1, decay: 0.2, sustain: 0.3, release: 0.4 };
        let control = ModSourceSlot::MidiLearn { source: None, learning: true };
        let all = [F::Rate, F::Waveform, F::Attack, F::Decay, F::Sustain, F::Release];
        let of = |source: &ModSourceSlot| all.map(|field| source.setting(field));
        assert_eq!(of(&triangle), [Some(2.5), Some(1.0), None, None, None, None]);
        assert_eq!(of(&envelope), [None, None, Some(0.1), Some(0.2), Some(0.3), Some(0.4)]);
        assert_eq!(of(&control), [None; 6]);
    }

    #[test]
    fn the_intent_of_a_row_sets_the_setting_or_the_depth_it_stands_for() {
        use ModulatorField as F;
        let with_edges = |m: ModulatorSlot, node: u32| ModulatorSlot {
            node: NodeId(node),
            targets: m.targets.into_iter().enumerate().map(|(i, t)| ModTargetSlot { edge: EdgeId(50 + i as u32), ..t }).collect(),
            ..m
        };
        let two = || vec![target(K::PluginParam { slot: 0, param_index: 0 }), target(K::PluginParam { slot: 0, param_index: 1 })];
        let intents = |m: &ModulatorSlot| (0..=modulator_param_len(m)).map(|r| m.row_intent(r, |v| v + 1.0)).collect::<Vec<_>>();
        let set = |node: u32, field, value| Some(Intent::SetModulatorField { node: NodeId(node), field, value });
        let depth = |edge: u32, depth| Some(Intent::SetEdgeDepth { edge: EdgeId(edge), depth });

        // A value is what the closure makes of the one the row has now (the controller clamps it).
        assert_eq!(
            intents(&with_edges(lfo(two()), 7)),
            [None, None, set(7, F::Rate, 3.0), None, depth(50, 1.5), depth(51, 1.5), None]
        );
        assert_eq!(
            intents(&with_edges(adsr(two()), 8)),
            [
                None,
                set(8, F::Attack, 1.01),
                set(8, F::Decay, 1.3),
                set(8, F::Sustain, 1.7),
                set(8, F::Release, 1.5),
                None,
                depth(50, 1.5),
                depth(51, 1.5),
                None
            ]
        );
        assert_eq!(intents(&with_edges(midi(two()), 9)), [None, None, depth(50, 1.5), depth(51, 1.5), None]);
        // And the closure may ignore what the row has: a value that was typed.
        assert_eq!(with_edges(lfo(two()), 7).row_intent(2, |_| 12.5), set(7, F::Rate, 12.5));
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
