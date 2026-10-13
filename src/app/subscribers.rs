//! Who hears what the controller does, and what they have been told; see
//! `docs/web-ui.md` §3.1.
//!
//! A front-end that mirrors the session (the web runtime, which tells its clients)
//! subscribes a sink with `Controller::subscribe`, and the controller tells every
//! sink about each intent it applies, in the order the sinks subscribed.
//!
//! Not everything that changes the session is an intent yet: the TUI edits the model
//! directly for most of what it does, and the audio thread reports a finished
//! recording or a captured MIDI control. So the list also keeps **what the
//! subscribers have been told**, as a snapshot with every intent applied to it
//! (`Snapshot::apply`), and the controller compares it with the session now
//! (`Controller::resync`): if they differ, the subscribers are given a snapshot to
//! start again from. Every TUI edit that becomes an intent makes this smaller.

#![cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime subscribes"))]

use crate::app::intent::Applied;
#[cfg(any(test, feature = "web"))]
use crate::app::snapshot::Snapshot;

/// Someone who wants to hear about every intent the controller applies, whichever
/// front-end asked for it (`Controller::subscribe`): the web runtime, to tell its
/// clients. It is called on the owner thread, inside `apply`, after the model and
/// the audio thread have been changed, so it must not block.
pub trait EventSink: Send {
    fn applied(&mut self, applied: &Applied);

    /// The session changed in a way no intent describes (`Controller::resync`):
    /// this is how it is now, at the revision the snapshot has, and what is told
    /// next is applied to it. A sink that keeps a copy of the session, as the web
    /// runtime's clients do, takes this as its new copy; one that only follows
    /// intents has no use for it.
    #[cfg(any(test, feature = "web"))]
    fn resync(&mut self, _snapshot: &Snapshot) {}
}

impl<F: FnMut(&Applied) + Send> EventSink for F {
    fn applied(&mut self, applied: &Applied) {
        self(applied)
    }
}

/// Names a subscription, to end it with `Controller::unsubscribe`. Never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SubscriberId(u64);

struct Subscriber {
    id: SubscriberId,
    sink: Box<dyn EventSink>,
}

/// The sinks that have subscribed, in the order they did, and what they have been told.
#[derive(Default)]
pub struct Subscribers {
    list: Vec<Subscriber>,
    next: u64,
    /// The session as the subscribers have it: the snapshot they started from, with
    /// the intents and the resyncs since. All of them have the same, because they are
    /// all told everything. An intent that does not fit it leaves it as it was, and
    /// so different from the session, which is what makes the next `resync` tell them.
    /// None until somebody subscribes.
    #[cfg(any(test, feature = "web"))]
    told: Option<Snapshot>,
}

impl Subscribers {
    #[cfg(any(test, feature = "web"))]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The first subscriber is to start from `snapshot`, the session as it is now.
    /// (Those that follow share what the first has been told.)
    #[cfg(any(test, feature = "web"))]
    pub fn start_from(&mut self, snapshot: Snapshot) {
        self.told = Some(snapshot);
    }

    pub fn add(&mut self, sink: Box<dyn EventSink>) -> SubscriberId {
        let id = SubscriberId(self.next);
        self.next += 1;
        self.list.push(Subscriber { id, sink });
        id
    }

    /// End a subscription. False if there is none by that id (any more).
    pub fn remove(&mut self, id: SubscriberId) -> bool {
        let before = self.list.len();
        self.list.retain(|s| s.id != id);
        self.list.len() != before
    }

    /// Tell every sink about an intent that has been applied.
    pub fn publish(&mut self, applied: &Applied) {
        for subscriber in &mut self.list {
            subscriber.sink.applied(applied);
        }
        #[cfg(any(test, feature = "web"))]
        if let Some(told) = &mut self.told {
            // (If it does not fit, `told` is as it was, and differs from the session.)
            let _ = told.apply(applied);
        }
    }

    /// Whether the subscribers need a snapshot to be where `now` is.
    #[cfg(any(test, feature = "web"))]
    pub fn differ_from(&self, now: &Snapshot) -> bool {
        let Some(told) = &self.told else { return true };
        if told == now {
            return false;
        }
        // Not equal as values. But a number that is not a number is not equal to itself, so
        // a session that holds one would differ from what was told of it for ever. What the
        // subscribers are told is JSON, in which each of them is a `null`: compare that.
        match (serde_json::to_vec(told), serde_json::to_vec(now)) {
            (Ok(told), Ok(now)) => told != now,
            _ => true,
        }
    }

    /// Give every sink `snapshot` to start again from.
    #[cfg(any(test, feature = "web"))]
    pub fn resync(&mut self, snapshot: Snapshot) {
        for subscriber in &mut self.list {
            subscriber.sink.resync(&snapshot);
        }
        self.told = Some(snapshot);
    }
}
