//! The terminal's events, read on a thread of their own.
//!
//! The TUI loop waits for several things at once: the terminal, the requests of the
//! web interface when there is one, and a tick. Crossterm can only be waited on by
//! polling it, so a thread does that and passes what it reads on over a channel,
//! which the loop selects on along with the others (`docs/web-ui.md` step 13).

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, unbounded};
use crossterm::event::{self, Event};

/// How long the thread waits for the terminal at a time before it looks at whether it
/// is to stop: the most that stopping it waits for.
const WAIT: Duration = Duration::from_millis(50);

/// The terminal's events, as they come. Dropping it stops the thread that reads them.
pub struct TerminalReader {
    events: Receiver<io::Result<Event>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TerminalReader {
    /// Start reading the terminal. Call it after the terminal is set up, and not
    /// before anything that asks the terminal a question and waits for the answer
    /// (crossterm reads that with the same reader).
    pub fn start() -> io::Result<TerminalReader> {
        TerminalReader::from(event::poll, event::read)
    }

    /// Read events from `poll` (is there one within the time given?) and `read`.
    /// A failure of either is passed on, and ends the reading.
    fn from(
        mut poll: impl FnMut(Duration) -> io::Result<bool> + Send + 'static,
        mut read: impl FnMut() -> io::Result<Event> + Send + 'static,
    ) -> io::Result<TerminalReader> {
        let (tx, events) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::Builder::new().name("tang-terminal".into()).spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let event = match poll(WAIT) {
                    Ok(false) => continue,
                    Ok(true) => read(),
                    Err(e) => Err(e),
                };
                let failed = event.is_err();
                // (Nobody listening any more is the end of it, too.)
                if tx.send(event).is_err() || failed {
                    return;
                }
            }
        })?;
        Ok(TerminalReader { events, stop, thread: Some(thread) })
    }

    pub fn events(&self) -> &Receiver<io::Result<Event>> {
        &self.events
    }
}

impl Drop for TerminalReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // (A thread that panicked has nothing more to say.)
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    fn key(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    /// A terminal that has `events` to give, one at a time, and nothing after them
    /// (it waits as long as it is asked to, which is a little).
    #[derive(Clone)]
    struct Fake {
        queue: Arc<Mutex<VecDeque<io::Result<Event>>>>,
    }

    impl Fake {
        fn with(events: impl IntoIterator<Item = io::Result<Event>>) -> Fake {
            Fake { queue: Arc::new(Mutex::new(events.into_iter().collect())) }
        }

        fn poll(&self) -> impl FnMut(Duration) -> io::Result<bool> + Send + 'static {
            let queue = self.queue.clone();
            move |wait| {
                let next = queue.lock().unwrap().front().map(|e| e.is_ok());
                match next {
                    // A failure is there to be read, too.
                    Some(_) => Ok(true),
                    None => {
                        std::thread::sleep(wait.min(Duration::from_millis(2)));
                        Ok(false)
                    }
                }
            }
        }

        fn read(&self) -> impl FnMut() -> io::Result<Event> + Send + 'static {
            let queue = self.queue.clone();
            move || queue.lock().unwrap().pop_front().expect("read after poll said there was something")
        }

        fn push(&self, event: Event) {
            self.queue.lock().unwrap().push_back(Ok(event));
        }
    }

    fn reader(fake: &Fake) -> TerminalReader {
        TerminalReader::from(fake.poll(), fake.read()).unwrap()
    }

    fn next(reader: &TerminalReader) -> io::Result<Event> {
        reader.events().recv_timeout(Duration::from_secs(5)).expect("an event within five seconds")
    }

    #[test]
    fn the_events_come_in_the_order_the_terminal_gave_them() {
        let fake = Fake::with([Ok(key('a')), Ok(key('b')), Ok(key('c'))]);
        let reader = reader(&fake);
        assert_eq!([next(&reader).unwrap(), next(&reader).unwrap(), next(&reader).unwrap()], [key('a'), key('b'), key('c')]);
    }

    #[test]
    fn an_event_that_comes_later_is_passed_on_when_it_comes() {
        let fake = Fake::with([]);
        let reader = reader(&fake);
        assert!(reader.events().recv_timeout(Duration::from_millis(50)).is_err(), "nothing yet");
        fake.push(key('x'));
        assert_eq!(next(&reader).unwrap(), key('x'));
    }

    #[test]
    fn a_failure_is_passed_on_and_ends_the_reading() {
        let fake = Fake::with([Ok(key('a')), Err(io::Error::new(io::ErrorKind::BrokenPipe, "the terminal went")), Ok(key('b'))]);
        let reader = reader(&fake);
        assert_eq!(next(&reader).unwrap(), key('a'));
        assert_eq!(next(&reader).unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert!(
            reader.events().recv_timeout(Duration::from_secs(5)).is_err_and(|e| e == crossbeam_channel::RecvTimeoutError::Disconnected),
            "and that is the end of it: the b is never read"
        );
    }

    #[test]
    fn dropping_the_reader_stops_its_thread() {
        let fake = Fake::with([]);
        let token = Arc::new(());
        let held = token.clone();
        let mut poll = fake.poll();
        let reader = TerminalReader::from(
            move |wait| {
                let _held = &held;
                poll(wait)
            },
            fake.read(),
        )
        .unwrap();
        assert_eq!(Arc::strong_count(&token), 2, "the thread has it");
        let started = std::time::Instant::now();
        drop(reader);
        assert_eq!(Arc::strong_count(&token), 1, "the thread is gone, and has let go of what it held");
        assert!(started.elapsed() < Duration::from_secs(2), "and does not take long about it");
    }

    #[test]
    fn dropping_the_reader_does_not_wait_for_a_terminal_that_has_nothing_to_say() {
        // A poll that takes as long as it is asked to, the way the real one does.
        let reader = TerminalReader::from(
            |wait| {
                std::thread::sleep(wait);
                Ok(false)
            },
            || unreachable!("there was nothing to read"),
        )
        .unwrap();
        let started = std::time::Instant::now();
        drop(reader);
        assert!(started.elapsed() < Duration::from_millis(500), "it waits for one poll, which is short: {:?}", started.elapsed());
    }

    #[test]
    fn many_events_are_all_passed_on() {
        let fake = Fake::with((0..2_000).map(|n| Ok(key(char::from_u32(0x100 + n).unwrap()))));
        let reader = reader(&fake);
        for n in 0..2_000 {
            assert_eq!(next(&reader).unwrap(), key(char::from_u32(0x100 + n).unwrap()));
        }
    }
}
