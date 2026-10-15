//! What puts back what was taken out (`docs/web-ui.md` §5, D37).
//!
//! A removal is undone by putting back what it took, where it was, under its ids and the telemetry slots it had: a cable, with what it was centred on,
//! as `SessionModel::remove_edge` hands it back, a modulator with its own cables, as `SessionModel::remove_modulator_at` does, and an effect (a lane's or
//! a bus's) with the plugin that was in the chain; the model and the audio thread are given them again (`SessionModel::insert_target_at`,
//! `insert_modulator_at`, `insert_effect_at` and `insert_bus_effect_at`; `GraphCommand::InsertModTarget`, `InsertModulator`, `InsertEffect` and
//! `InsertGroupEffect`). What an operation of the controller takes out is written down as it goes, in a `Journal`, and a step of the history that holds one
//! puts everything back in the reverse order (`Controller::rewind`): the last thing taken out is the first put back, so that each goes back into the
//! session as it was when its turn to go came, and the places that the things taken out after it had are the places they have again.
//!
//! The plugin that is taken out of a chain is the audio thread's to hand back: the controller makes a channel for it (`RemoveEffect { keep }`) and the
//! journal holds the other end (`Kept::Asked`), so that undo puts the very plugin back, with whatever it holds, at once; if it did not come (the audio thread
//! is not running, or the history let go of it, `History` keeps only the newest few) the plugin is loaded again with its preset and the values the user set.

use std::fmt;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};

use crate::app::apply::Done;
use crate::app::controller::{ChainAt, Controller};
use crate::app::intent::{Applied, IntentError};
use crate::app::model::{mod_source_slot_to_graph, ModTargetSlot, ModulatorSlot, ParamKind, PluginSlot, RemovedCable};
use crate::plugin::chain::{GraphCommand, ModTarget, ModulatorLocation, CHAIN_CHANNELS};
use crate::plugin::{self, Plugin};

/// One thing that was taken out, and what it takes to put it back.
#[derive(Debug)]
pub(super) enum Inverse {
    /// A cable, which goes back to its place in its modulator.
    Cable(RemovedCable),
    /// A modulator with its own cables, which goes back to its place in its rack: `bases` is what each of the cables was centred on, in order (what the
    /// user had set what it drives to).
    Modulator { at: ModulatorLocation, index: usize, modulator: ModulatorSlot, bases: Vec<f32> },
    /// An effect of a lane's chain or of a group's bus, which goes back to its place in the chain: `slot` is the model's plugin (its node, its values, its mix),
    /// and `kept` the plugin that was in the chain, or what puts one back.
    Effect { at: ChainAt, index: usize, slot: PluginSlot, kept: Kept },
    /// Something went that cannot be put back one thing at a time (cables that drive each other's depths, `RemovedCable::still_driven`): what
    /// the operation did is not a step of the history, which is forgotten as it is for what cannot be undone.
    NoWayBack,
}

/// A plugin that was taken out of a chain, and whether the controller has the very plugin to put back.
pub(super) enum Kept {
    /// The audio thread was asked to hand it to this channel: it is there, or on its way.
    Asked(Receiver<Box<dyn Plugin>>),
    /// The very plugin, in hand.
    InHand(Box<dyn Plugin>),
    /// None is kept (nothing asked for one, or the history let go of it): it is loaded again.
    Nothing,
}

impl fmt::Debug for Kept {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kept::Asked(_) => "Kept::Asked",
            Kept::InHand(_) => "Kept::InHand",
            Kept::Nothing => "Kept::Nothing",
        })
    }
}

/// What an operation took out, in the order it did.
#[derive(Debug, Default)]
pub struct Journal {
    inverses: Vec<Inverse>,
}

impl Journal {
    /// Write down one more thing that was taken out.
    pub(super) fn push(&mut self, inverse: Inverse) {
        self.inverses.push(inverse);
    }

    /// How many plugins it keeps to put back (asked for, or in hand).
    pub fn plugins_kept(&self) -> usize {
        self.inverses.iter().filter(|inverse| matches!(inverse, Inverse::Effect { kept: Kept::Asked(_) | Kept::InHand(_), .. })).count()
    }

    /// Let go of the plugins it keeps: they are loaded again if what it took out is put back. (A plugin that is dropped is dropped here, on the thread that
    /// holds the history, as the main thread drops what the audio thread hands back.)
    pub fn let_go_of_plugins(&mut self) {
        for inverse in &mut self.inverses {
            if let Inverse::Effect { kept, .. } = inverse {
                *kept = Kept::Nothing;
            }
        }
    }

    /// Whether something was taken out and all of it can be put back.
    pub fn can_be_put_back(&self) -> bool {
        !self.inverses.is_empty() && !self.inverses.iter().any(|inverse| matches!(inverse, Inverse::NoWayBack))
    }
}

/// A cable as the audio thread is told of it when it goes in, centred on `base_value`.
fn cable_for_the_audio_thread(slot: &ModTargetSlot, base_value: f32) -> ModTarget {
    ModTarget { kind: slot.kind.clone(), depth: slot.depth, base_value, param_min: slot.param_min, param_max: slot.param_max, effective: slot.effective }
}

impl Controller {
    /// Write down, if an intent is being carried out, that something was taken out. (What the TUI and the tests take out by themselves, with no intent,
    /// is not written down: nobody asked for a step.)
    pub(super) fn took_out(&mut self, inverse: Inverse) {
        if let Some(journal) = &mut self.journal {
            journal.push(inverse);
        }
    }

    /// A structural edit that no intent asks for (what the TUI and the tests call by themselves) is no step of the history, and a step that took something
    /// out puts it back by the places it had, which the edit changes: the history is forgotten. (While an intent is carried out, which is a step of the history
    /// or forgets it as it should, the journal is there, and nothing is forgotten here.)
    pub(super) fn forget_the_history_unless_an_intent_asks(&mut self) {
        if self.journal.is_none() {
            self.history.clear();
        }
    }

    /// A channel for the plugin that an effect's removal takes out of the audio thread's chain, when an intent is being carried out: the sender is for the
    /// command, the `Kept` is what the journal holds. (Nothing, for a call that no intent asks for: nobody puts that back.)
    pub(super) fn channel_for_a_plugin(&self) -> (Option<Sender<Box<dyn Plugin>>>, Kept) {
        if self.journal.is_none() {
            return (None, Kept::Nothing);
        }
        let (keep, kept) = crossbeam_channel::bounded(1);
        (Some(keep), Kept::Asked(kept))
    }

    /// The plugin to put back: the very one if the audio thread handed it back (it is waited for a moment, `recovery_wait`, if it has not yet), else the plugin
    /// loaded again, with the preset it had and the values the user set. Refused, with the reason, if that does not load or its outputs are not the chain's.
    fn recover_plugin(&self, slot: &PluginSlot, kept: Kept, deadline: Instant) -> Result<Box<dyn Plugin>, IntentError> {
        let handed_back = match kept {
            Kept::InHand(plugin) => Some(plugin),
            Kept::Asked(channel) => channel.recv_deadline(deadline).ok(),
            Kept::Nothing => None,
        };
        let loaded = match handed_back {
            Some(plugin) => plugin,
            None => self.reload_plugin(slot)?,
        };
        let outputs = loaded.audio_output_count();
        if outputs != CHAIN_CHANNELS {
            return Err(IntentError::Invalid(format!("{} has {outputs} audio outputs, and an effect here needs {CHAIN_CHANNELS}", slot.name)));
        }
        Ok(loaded)
    }

    /// The plugin of `slot`, loaded again from what it was loaded from, with the preset it had and then the values that the model has, which are the user's.
    fn reload_plugin(&self, slot: &PluginSlot) -> Result<Box<dyn Plugin>, IntentError> {
        let mut loaded = plugin::load(&slot.id, self.sample_rate, self.max_block_size, &self.runtime).map_err(|e| {
            log::error!("Failed to load plugin '{}': {e}", slot.id);
            IntentError::Invalid(format!("{} could not be loaded", slot.name))
        })?;
        if let Some(preset) = slot.current_preset.as_ref().and_then(|name| slot.presets.iter().find(|preset| preset.name == *name)) {
            if let Err(e) = loaded.load_preset(&preset.id) {
                log::warn!("{}: preset '{}' did not load again: {e}", slot.name, preset.name);
            }
        }
        for param in slot.params.iter().filter(|param| !matches!(param.kind, ParamKind::Separator)) {
            // (A plugin that does not take a value that it gave is not worth refusing the undo for.)
            if let Err(e) = loaded.set_parameter(param.index, param.value) {
                log::debug!("{}: parameter {} did not take {}: {e}", slot.name, param.index, param.value);
            }
        }
        Ok(loaded)
    }

    /// Put back what `journal` says was taken out, the last thing first, as one revision: the structure is said to have changed. Refused, with nothing
    /// changed, if what is to come back is there already (a node or a cable has the id it had: another front-end has put it back by itself), or if a plugin
    /// cannot be had again.
    pub(super) fn rewind(&mut self, mut journal: Journal) -> Result<Applied, IntentError> {
        for inverse in &journal.inverses {
            match inverse {
                Inverse::Cable(cable) => self.refuse_a_cable_that_is_there(&cable.slot)?,
                Inverse::Effect { slot, .. } => {
                    if self.model.locate(slot.node).is_some() {
                        return Err(IntentError::Invalid(format!("node {} is there already", slot.node.0)));
                    }
                }
                Inverse::Modulator { modulator, .. } => {
                    if self.model.locate(modulator.node).is_some() {
                        return Err(IntentError::Invalid(format!("node {} is there already", modulator.node.0)));
                    }
                    for cable in &modulator.targets {
                        self.refuse_a_cable_that_is_there(cable)?;
                    }
                }
                Inverse::NoWayBack => {}
            }
        }
        // The plugins first: what can fail fails before anything is changed. (What was recovered stays in hand if a later one cannot be had, and goes with the
        // step, which is dropped.)
        let deadline = Instant::now() + self.recovery_wait;
        for inverse in &mut journal.inverses {
            if let Inverse::Effect { slot, kept, .. } = inverse {
                let plugin = self.recover_plugin(slot, std::mem::replace(kept, Kept::Nothing), deadline)?;
                *kept = Kept::InHand(plugin);
            }
        }
        for inverse in journal.inverses.into_iter().rev() {
            self.put_back(inverse);
        }
        // (Nothing was taken out by this.)
        self.taken_last = Journal::default();
        Ok(self.conclude(Done::Restructured))
    }

    fn refuse_a_cable_that_is_there(&self, cable: &ModTargetSlot) -> Result<(), IntentError> {
        match self.model.locate_edge(cable.edge) {
            Some(_) => Err(IntentError::Invalid(format!("cable {} is there already", cable.edge.0))),
            None => Ok(()),
        }
    }

    /// Put one thing back: in the model, and on the audio thread.
    fn put_back(&mut self, inverse: Inverse) {
        match inverse {
            Inverse::Cable(RemovedCable { at, mod_index, target_index, slot, base_value, .. }) => {
                let target = cable_for_the_audio_thread(&slot, base_value);
                // (The place it went to, which is the last for one past the end: the audio thread is told the same.)
                let Some(place) = self.model.insert_target_at(at, mod_index, target_index, slot) else { return };
                let _ = self.cmd_tx.send(match at {
                    ModulatorLocation::Lane(inst) => GraphCommand::InsertModTarget { inst, mod_index, target_index: place, target },
                    ModulatorLocation::Group(group) => GraphCommand::InsertGroupModTarget { group, mod_index, target_index: place, target },
                });
            }
            Inverse::Modulator { at, index, modulator, bases } => {
                debug_assert_eq!(modulator.targets.len(), bases.len(), "a base for each cable");
                let source = mod_source_slot_to_graph(&modulator.source);
                let output = modulator.output;
                let cables: Vec<ModTarget> = modulator.targets.iter().zip(bases).map(|(slot, base)| cable_for_the_audio_thread(slot, base)).collect();
                // (The place it went to, which is the last for one past the end: the audio thread is told the same.)
                let Some(place) = self.model.insert_modulator_at(at, index, modulator) else { return };
                let _ = self.cmd_tx.send(match at {
                    ModulatorLocation::Lane(inst) => GraphCommand::InsertModulator { inst, index: place, source },
                    ModulatorLocation::Group(group) => GraphCommand::InsertGroupModulator { group, index: place, source },
                });
                // It publishes what it puts out where it did, and its own cables come after it, in the order they had.
                if let Some(slot) = output {
                    let _ = self.cmd_tx.send(GraphCommand::SetModulatorOutput { location: at, index: place, slot });
                }
                for target in cables {
                    let _ = self.cmd_tx.send(match at {
                        ModulatorLocation::Lane(inst) => GraphCommand::AddModTarget { inst, mod_index: place, target },
                        ModulatorLocation::Group(group) => GraphCommand::AddGroupModTarget { group, mod_index: place, target },
                    });
                }
            }
            Inverse::Effect { at, index, slot, kept: Kept::InHand(effect) } => {
                let mix = f64::from(slot.mix);
                // (The place it went to, which is the last for one past the end: the audio thread is told the same. A chain that is not there takes nothing, and
                // the plugin is dropped here.)
                match at {
                    ChainAt::Lane(inst) => {
                        let Some(place) = self.model.insert_effect_at(inst, index, slot) else { return };
                        let _ = self.cmd_tx.send(GraphCommand::InsertEffect { inst, index: place, effect, mix });
                    }
                    ChainAt::Bus(group) => {
                        let Some(place) = self.model.insert_bus_effect_at(group, index, slot) else { return };
                        let _ = self.cmd_tx.send(GraphCommand::InsertGroupEffect { group, index: place, effect, mix });
                    }
                }
            }
            // (`rewind` has every plugin in hand by now.)
            Inverse::Effect { .. } | Inverse::NoWayBack => {}
        }
    }
}
