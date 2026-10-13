//! Who hears what the controller does; see `docs/web-ui.md` §3.1.
//!
//! A front-end that mirrors the session (the web runtime, which tells its clients)
//! subscribes a sink with `Controller::subscribe`, and the controller tells every
//! sink about each intent it applies, in the order the sinks subscribed.

#![cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime subscribes"))]

use crate::app::intent::Applied;

/// Someone who wants to hear about every intent the controller applies, whichever
/// front-end asked for it (`Controller::subscribe`): the web runtime, to tell its
/// clients. It is called on the owner thread, inside `apply`, after the model and
/// the audio thread have been changed, so it must not block.
pub trait EventSink: Send {
    fn applied(&mut self, applied: &Applied);
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

/// The sinks that have subscribed, in the order they did.
#[derive(Default)]
pub struct Subscribers {
    list: Vec<Subscriber>,
    next: u64,
}

impl Subscribers {
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
    }
}
