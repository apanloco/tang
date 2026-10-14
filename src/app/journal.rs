//! What puts back what was taken out (`docs/web-ui.md` §5, D37).
//!
//! A removal is undone by putting back what it took, where it was, under its ids and the telemetry slots it had: the cable, with what it was
//! centred on, as `SessionModel::remove_edge` hands it back, and the model and the audio thread are given it again (`SessionModel::insert_target_at`,
//! `GraphCommand::InsertModTarget`). What an operation of the controller takes out is written down as it goes, in a `Journal`, and a step of the history
//! that holds one puts everything back in the reverse order (`Controller::rewind`): the last thing taken out is the first put back, so that each goes back
//! into the session as it was when its turn to go came, and the places that the things taken out after it had are the places they have again.

use crate::app::apply::Done;
use crate::app::controller::Controller;
use crate::app::intent::{Applied, IntentError};
use crate::app::model::RemovedCable;
use crate::plugin::chain::{GraphCommand, ModTarget, ModulatorLocation};

/// One thing that was taken out, and what it takes to put it back.
#[derive(Debug)]
pub(super) enum Inverse {
    /// A cable, which goes back to its place in its modulator.
    Cable(RemovedCable),
    /// Something went that cannot be put back one thing at a time (cables that drive each other's depths, `RemovedCable::still_driven`): what
    /// the operation did is not a step of the history, which is forgotten as it is for what cannot be undone.
    NoWayBack,
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

    /// Whether something was taken out and all of it can be put back.
    pub fn can_be_put_back(&self) -> bool {
        !self.inverses.is_empty() && !self.inverses.iter().any(|inverse| matches!(inverse, Inverse::NoWayBack))
    }
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

    /// Put back what `journal` says was taken out, the last thing first, as one revision: the structure is said to have changed. Refused, with nothing
    /// changed, if what is to come back is there already (a cable has the id it had: another front-end has put it back by itself).
    pub(super) fn rewind(&mut self, journal: Journal) -> Result<Applied, IntentError> {
        for inverse in &journal.inverses {
            if let Inverse::Cable(cable) = inverse {
                if self.model.locate_edge(cable.slot.edge).is_some() {
                    return Err(IntentError::Invalid(format!("cable {} is there already", cable.slot.edge.0)));
                }
            }
        }
        for inverse in journal.inverses.into_iter().rev() {
            self.put_back(inverse);
        }
        // (Nothing was taken out by this.)
        self.taken_last = Journal::default();
        Ok(self.conclude(Done::Restructured))
    }

    /// Put one thing back: in the model, and on the audio thread.
    fn put_back(&mut self, inverse: Inverse) {
        match inverse {
            Inverse::Cable(RemovedCable { at, mod_index, target_index, slot, base_value, .. }) => {
                let target = ModTarget {
                    kind: slot.kind.clone(),
                    depth: slot.depth,
                    base_value,
                    param_min: slot.param_min,
                    param_max: slot.param_max,
                    effective: slot.effective,
                };
                // (The place it went to, which is the last for one past the end: the audio thread is told the same.)
                let Some(place) = self.model.insert_target_at(at, mod_index, target_index, slot) else { return };
                let _ = self.cmd_tx.send(match at {
                    ModulatorLocation::Lane(inst) => GraphCommand::InsertModTarget { inst, mod_index, target_index: place, target },
                    ModulatorLocation::Group(group) => GraphCommand::InsertGroupModTarget { group, mod_index, target_index: place, target },
                });
            }
            Inverse::NoWayBack => {}
        }
    }
}
