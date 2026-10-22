use std::sync::Arc;

use midir::{MidiInput, MidiInputConnection, MidiInputPort};

use crate::midi_monitor::MidiMonitor;
use crate::note_input::{Delivery, NoteInput};
use crate::source::{Source, SourceSet};

/// The sources of the MIDI devices (P4a of `docs/patch-graph.md` §11.6): each device has a number of its own for as long as the program runs, handed out in the order the devices are
/// first seen, so that a device that is unplugged and plugged in again is the source it was (what a lane hears it by, and what it has sounding, stay with the number).
#[derive(Default)]
pub struct SourceTable {
    /// The names of the devices, in the order they were first seen: the device at place `n` is the source `Source::device(n)`.
    names: Vec<String>,
}

impl SourceTable {
    /// The source of the device called `name`: the one it was given when it was first seen, or the next that is free if it is new.
    pub fn source_of(&mut self, name: &str) -> Source {
        let nth = match self.names.iter().position(|seen| seen == name) {
            Some(nth) => nth,
            None => {
                self.names.push(name.to_string());
                self.names.len() - 1
            }
        };
        Source::device(nth)
    }
}

/// CoreMIDI (macOS) gives a program the list of the MIDI ports as the server last told it, and the server tells a thread on its run loop: a program that never runs one sees the ports that were
/// there when it started, a port that goes is a stale one that has no name, and a device that is plugged in later is never listed. One turn of the loop, with no waiting, lets it see.
#[cfg(target_os = "macos")]
fn let_the_system_see_the_devices() {
    use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
    // SAFETY: a constant of CoreFoundation's.
    CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, std::time::Duration::ZERO, false);
}

#[cfg(not(target_os = "macos"))]
fn let_the_system_see_the_devices() {}

/// What a look at the MIDI ports found (P4b of `docs/patch-graph.md` §11.6): the devices that were plugged in and opened, and the devices that have gone, each by its source.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DeviceChanges {
    pub came: Vec<Source>,
    pub gone: Vec<Source>,
}

/// What to do about the ports that are listed now, given the devices that are open.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    /// The places, among the listed ports, of those to open.
    to_open: Vec<usize>,
    /// The places, among the open devices, of those that have gone.
    gone: Vec<usize>,
}

/// A MIDI input port as the system lists it: its id, which is its own for as long as it is there (a name is not: two ports may be called the same), and its name, which a port that is
/// not there any more, or never was (a stale endpoint), cannot give.
type Listed<'a> = (&'a str, Option<&'a str>);

/// A device that is open is still there when its port is listed, with the name it was opened by if the port can say one. It has gone when it is not (the program looks at the ports once a
/// second: a device that is unplugged and plugged in again between two looks is not seen to go).
/// A port is opened when it has a name, passes the filter (its name has the filter in it) and no device of that name stays open, as a name that is listed twice is opened once.
fn plan(open: &[(&str, &str)], listed: &[Listed], filter: Option<&str>) -> Plan {
    let there = |(id, name): &(&str, &str)| listed.iter().any(|port| port.0 == *id && port.1.is_none_or(|listed| listed == *name));
    let gone: Vec<usize> = (0..open.len()).filter(|&at| !there(&open[at])).collect();
    let stays = |name: &str| open.iter().enumerate().any(|(at, device)| device.1 == name && !gone.contains(&at));
    let to_open = (0..listed.len())
        .filter(|&at| {
            let Some(name) = listed[at].1 else { return false };
            filter.is_none_or(|filter| name.contains(filter)) && !stays(name) && !listed[..at].iter().any(|earlier| earlier.1 == Some(name))
        })
        .collect();
    Plan { to_open, gone }
}

/// The sources of the open devices that the device called `name` in a session stands for: the devices that are called exactly that, or, if there is none, those that have it in their name,
/// whatever the case (a Launchkey Mini has two ports, "Launchkey Mini MK4 37 MIDI Out" and "Launchkey Mini MK4 37 DAW Out", and "Launchkey Mini" is both). A name that is blank stands for none.
fn matching(open: &[(&str, Source)], name: &str) -> SourceSet {
    let name = name.trim();
    if name.is_empty() {
        return SourceSet::NONE;
    }
    let sources = |called: &dyn Fn(&str) -> bool| open.iter().filter(|(device, _)| called(device)).fold(SourceSet::NONE, |set, (_, source)| set.with(*source));
    let exact = sources(&|device| device == name);
    if !exact.is_empty() {
        return exact;
    }
    let lower = name.to_lowercase();
    sources(&|device| device.to_lowercase().contains(&lower))
}

/// A device that is open: its port's id and name, its source and the connection, which is closed when the device is dropped.
struct Open<C> {
    id: String,
    name: String,
    source: Source,
    _connection: C,
}

/// Take the devices at the places `gone` (ascending) out of `open`, which closes them: the sources they had, in the order they were open.
fn take_out<C>(open: &mut Vec<Open<C>>, gone: &[usize]) -> Vec<Source> {
    // The last first, so that the places of the others stay where they are.
    let mut sources: Vec<Source> = gone
        .iter()
        .rev()
        .map(|&at| {
            let device = open.remove(at);
            log::info!("MIDI input gone: {} ({})", device.name, device.source);
            device.source
        })
        .collect();
    sources.reverse();
    sources
}

pub struct MidiManager {
    notes: NoteInput,
    device_filter: Option<String>,
    /// The devices that are open, in the order they were opened.
    open: Vec<Open<MidiInputConnection<()>>>,
    monitor: Arc<MidiMonitor>,
    /// The source of each device that has been opened, kept for the run: a device that has gone is the source it was when it comes back.
    sources: SourceTable,
}

impl MidiManager {
    pub fn new(notes: NoteInput, device_filter: Option<String>, monitor: Arc<MidiMonitor>) -> Self {
        MidiManager { notes, device_filter, open: Vec::new(), monitor, sources: SourceTable::default() }
    }

    /// Look at the MIDI input ports: open those that are new (all of them, or those that match the filter) and close the devices that are not listed any more. The devices that came
    /// and went, by their sources.
    pub fn open_ports(&mut self) -> anyhow::Result<DeviceChanges> {
        let_the_system_see_the_devices();
        let midi_in = MidiInput::new("tang")?;
        // Every port, with its name if it has one: a stale endpoint is listed and has none, and is there for as long as it is, so that it must not hide a device that has gone.
        let listed: Vec<(String, Option<String>, MidiInputPort)> = midi_in.ports().into_iter().map(|port| (port.id(), midi_in.port_name(&port).ok(), port)).collect();
        let ports: Vec<Listed> = listed.iter().map(|(id, name, _)| (id.as_str(), name.as_deref())).collect();
        let devices: Vec<(&str, &str)> = self.open.iter().map(|device| (device.id.as_str(), device.name.as_str())).collect();
        let Plan { to_open, gone } = plan(&devices, &ports, self.device_filter.as_deref());

        let mut changes = DeviceChanges { gone: take_out(&mut self.open, &gone), ..DeviceChanges::default() };

        for at in to_open {
            let (id, name, port) = &listed[at];
            // (`plan` has only ports that have a name.)
            let Some(name) = name else { continue };
            // Each device is a source of its own: what it plays is tagged with it, so that a lane can hear it or not.
            let source = self.sources.source_of(name);
            let notes = self.notes.for_source(source);
            let monitor = self.monitor.clone();
            let log_name = name.clone();
            let conn_name = name.clone();

            // Need a fresh MidiInput for each connection
            let midi_in_for_port = MidiInput::new("tang")?;
            match midi_in_for_port.connect(
                port,
                &conn_name,
                move |_timestamp_us, bytes, _| {
                    let status = bytes[0];
                    let kind = match status & 0xF0 {
                        0x90 => "NoteOn ",
                        0x80 => "NoteOff",
                        0xB0 => "CC     ",
                        0xE0 => "Bend   ",
                        0xD0 => "ChanPrs",
                        0xA0 => "KeyPrs ",
                        0xC0 => "PgmChg ",
                        _ => "Other  ",
                    };
                    let ch = status & 0x0F;
                    let note_info = match status & 0xF0 {
                        0x90 | 0x80 if bytes.len() >= 2 => {
                            format!(" {}", crate::note_name(bytes[1]))
                        }
                        _ => String::new(),
                    };
                    log::info!("MIDI in  [{log_name}] {kind} ch={ch}{note_info} data={bytes:02x?}");
                    // Feed the on-screen `--debug` monitor (no-op unless enabled).
                    monitor.record(bytes);
                    // Locked mode, the held notes and the channel to the audio
                    // thread: the note path every source shares.
                    if notes.send(bytes) == Delivery::Full {
                        log::warn!("MIDI channel full — dropping event from {log_name}");
                    }
                },
                (),
            ) {
                Ok(connection) => {
                    log::info!("Opened MIDI input: {name} ({source})");
                    self.open.push(Open { id: id.clone(), name: name.clone(), source, _connection: connection });
                    changes.came.push(source);
                }
                Err(e) => {
                    log::warn!("Failed to open MIDI input {name}: {e}");
                }
            }
        }

        Ok(changes)
    }

    /// Poll for MIDI devices that have been connected or have gone. Call periodically from main loop.
    pub fn poll_new_devices(&mut self) -> DeviceChanges {
        match self.open_ports() {
            Ok(changes) => {
                if !changes.came.is_empty() {
                    log::info!("Opened {} new MIDI device(s)", changes.came.len());
                }
                changes
            }
            Err(e) => {
                log::warn!("MIDI poll error: {e}");
                DeviceChanges::default()
            }
        }
    }

    pub fn connection_count(&self) -> usize {
        self.open.len()
    }

    /// The sources of the open devices that the device called `name` in a session stands for (none while it is not plugged in).
    pub fn sources_of(&self, name: &str) -> SourceSet {
        let open: Vec<(&str, Source)> = self.open.iter().map(|device| (device.name.as_str(), device.source)).collect();
        matching(&open, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_has_the_source_it_was_given_when_it_was_first_seen_for_as_long_as_the_program_runs() {
        let mut table = SourceTable::default();
        let launchkey = table.source_of("Launchkey 25");
        let seaboard = table.source_of("Seaboard RISE");
        assert_eq!((launchkey, seaboard), (Source::device(0), Source::device(1)));
        assert_ne!(launchkey, Source::KEYS, "no device is the keys");
        assert_eq!(table.source_of("Launchkey 25"), launchkey, "plugged in again, it is the source it was");
        assert_eq!(table.source_of("Seaboard RISE"), seaboard);
        assert_eq!(table.source_of("IAC Driver Bus 1"), Source::device(2), "a device that is new has the next");
    }

    #[test]
    fn the_devices_past_the_last_source_share_it() {
        let mut table = SourceTable::default();
        let sources: Vec<Source> = (0..70).map(|n| table.source_of(&format!("device {n}"))).collect();
        assert_eq!(sources[61], Source::device(61));
        assert!(sources[..62].windows(2).all(|pair| pair[0] != pair[1]), "62 devices have a source each");
        assert!(sources[62..].iter().all(|&source| source == Source::LAST), "the rest share the last: {sources:?}");
        assert_eq!(table.source_of("device 3"), Source::device(3), "and the ones before it are what they were");
    }

    /// What `plan` says about devices that are open (by their id and name) and ports that are listed (by their id and name, if they have one): the places of those to open and of those
    /// that have gone.
    fn plan_for(open: &[(&str, &str)], listed: &[Listed], filter: Option<&str>) -> (Vec<usize>, Vec<usize>) {
        let Plan { to_open, gone } = plan(open, listed, filter);
        (to_open, gone)
    }

    /// Ports whose id is their name, as most of the tests have them.
    fn named<'a>(names: &[&'a str]) -> Vec<Listed<'a>> {
        names.iter().map(|&name| (name, Some(name))).collect()
    }

    /// Devices that are open, with an id that is their name.
    fn opened<'a>(names: &[&'a str]) -> Vec<(&'a str, &'a str)> {
        names.iter().map(|&name| (name, name)).collect()
    }

    #[test]
    fn a_port_that_is_listed_and_not_open_is_opened_and_one_that_is_open_and_listed_is_left_alone() {
        assert_eq!(plan_for(&[], &named(&["Launchkey", "Seaboard"]), None), (vec![0, 1], vec![]));
        assert_eq!(plan_for(&opened(&["Seaboard"]), &named(&["Launchkey", "Seaboard"]), None), (vec![0], vec![]));
        assert_eq!(plan_for(&opened(&["Launchkey", "Seaboard"]), &named(&["Launchkey", "Seaboard"]), None), (vec![], vec![]), "nothing has changed");
    }

    #[test]
    fn a_device_that_is_open_and_not_listed_has_gone_and_is_named_by_its_place_among_the_open_ones() {
        assert_eq!(plan_for(&opened(&["Launchkey", "Seaboard", "IAC"]), &named(&["Launchkey", "IAC"]), None), (vec![], vec![1]));
        assert_eq!(plan_for(&opened(&["Launchkey", "Seaboard"]), &[], None), (vec![], vec![0, 1]), "with no port listed, every device has gone");
    }

    #[test]
    fn a_device_that_went_and_one_that_came_are_told_apart_in_one_look() {
        assert_eq!(plan_for(&opened(&["Launchkey", "Seaboard"]), &named(&["Seaboard", "IAC"]), None), (vec![1], vec![0]));
    }

    #[test]
    fn a_stale_port_that_has_no_name_is_not_opened_and_does_not_hide_a_device_that_has_gone() {
        // (The ports of this machine's MIDI system, as `tang` found them: the endpoint that is no more is listed, and cannot say its name.)
        let listed: Vec<Listed> = vec![("stale", None), ("launchkey", Some("Launchkey"))];
        assert_eq!(plan_for(&[], &listed, None), (vec![1], vec![]), "it is not opened");
        assert_eq!(plan_for(&[("seaboard", "Seaboard")], &listed, None), (vec![1], vec![0]), "and the device that is not listed has gone all the same");
    }

    #[test]
    fn a_device_whose_port_is_listed_but_cannot_give_its_name_is_still_there() {
        // The name is read as the port is looked at, and may fail for a moment: the port is what the device is open on.
        assert_eq!(plan_for(&[("launchkey", "Launchkey")], &[("launchkey", None)], None), (vec![], vec![]));
    }

    #[test]
    fn a_device_is_the_port_it_was_opened_on_and_not_the_name_that_a_port_has() {
        // The port that the device is open on is listed under another name: the device is not there, and the name is a new one to open.
        let listed: Vec<Listed> = vec![("port 1", Some("Seaboard"))];
        assert_eq!(plan_for(&[("port 1", "Launchkey")], &listed, None), (vec![0], vec![0]));
        // A port of another id with the name of a device that has gone (a virtual port that was made again) is the device, come back.
        assert_eq!(plan_for(&[("port 1", "IAC")], &[("port 2", Some("IAC"))], None), (vec![0], vec![0]));
        // But not while the device of that name is there: one device is open for a name.
        assert_eq!(plan_for(&[("port 1", "IAC")], &[("port 1", Some("IAC")), ("port 2", Some("IAC"))], None), (vec![], vec![]));
    }

    #[test]
    fn a_name_that_is_listed_twice_is_opened_once() {
        assert_eq!(plan_for(&[], &[("a", Some("IAC")), ("b", Some("Launchkey")), ("c", Some("IAC"))], None), (vec![0, 1], vec![]));
    }

    #[test]
    fn the_filter_decides_which_ports_are_opened_by_the_text_in_their_name() {
        let listed = named(&["Launchkey 25 MIDI 1", "Seaboard RISE", "Launchkey 25 MIDI 2"]);
        assert_eq!(plan_for(&[], &listed, Some("Launchkey")), (vec![0, 2], vec![]));
        assert_eq!(plan_for(&[], &listed, Some("Seaboard RISE")), (vec![1], vec![]));
        assert_eq!(plan_for(&[], &listed, Some("nothing like it")), (vec![], vec![]));
        assert_eq!(plan_for(&[], &listed, Some("")), (vec![0, 1, 2], vec![]), "an empty filter has every name in it");
    }

    #[test]
    fn the_filter_does_not_say_that_an_open_device_has_gone_or_has_not() {
        // A device that is open passed the filter when it was opened: it is gone when it is not listed, and not before.
        assert_eq!(plan_for(&opened(&["Launchkey 25"]), &named(&["Launchkey 25", "Seaboard"]), Some("Launchkey")), (vec![], vec![]));
        assert_eq!(plan_for(&opened(&["Launchkey 25"]), &named(&["Seaboard"]), Some("Launchkey")), (vec![], vec![0]));
    }

    fn open(names: &[&'static str]) -> Vec<(&'static str, Source)> {
        names.iter().enumerate().map(|(nth, &name)| (name, Source::device(nth))).collect()
    }

    fn numbers(set: SourceSet) -> Vec<u8> {
        set.iter().map(Source::number).collect()
    }

    #[test]
    fn a_device_of_a_session_stands_for_the_open_ports_that_are_called_that_or_if_there_is_none_have_it_in_their_names() {
        let ports = open(&["Launchkey Mini MK4 37 MIDI Out", "Launchkey Mini MK4 37 DAW Out", "ROLI Airwave Expression", "IAC Driver Bus 1", "IAC Driver Bus 10"]);
        assert_eq!(numbers(matching(&ports, "ROLI Airwave Expression")), [3], "a name is itself");
        assert_eq!(numbers(matching(&ports, "Launchkey Mini MK4 37")), [1, 2], "both ports of a device have its name in theirs");
        assert_eq!(numbers(matching(&ports, "airwave")), [3], "whatever the case");
        assert_eq!(numbers(matching(&ports, "IAC Driver Bus 1")), [4], "a port that is called exactly that is the device, and not the other that has it in its name");
        assert_eq!(numbers(matching(&ports, "IAC Driver Bus")), [4, 5], "and when none is called that, every port that has it");
        assert_eq!(numbers(matching(&ports, "  Launchkey  ")), [1, 2], "the spaces round a name go");
    }

    #[test]
    fn a_device_that_is_not_open_or_has_no_name_stands_for_no_source() {
        let ports = open(&["Launchkey Mini MK4 37 MIDI Out", "Seaboard RISE"]);
        assert!(matching(&ports, "Pianoteq").is_empty());
        assert!(matching(&[], "Seaboard").is_empty(), "nothing is open");
        assert!(matching(&ports, "").is_empty(), "a name that is blank is in every name, and is no device");
        assert!(matching(&ports, "   ").is_empty());
        assert!(matching(&ports, "launchkey mini mk4 37 midi out and more").is_empty(), "the name is in the port's name, and not the other way");
    }

    #[test]
    fn the_ports_that_share_the_last_source_are_one_source_for_a_device() {
        let names: Vec<String> = (0..66).map(|n| format!("device {n}")).collect();
        let ports: Vec<(&str, Source)> = names.iter().enumerate().map(|(nth, name)| (name.as_str(), Source::device(nth))).collect();
        assert_eq!(numbers(matching(&ports, "device 65")), [63], "the 66th is the last source, which the ones after the 62nd share");
        assert_eq!(numbers(matching(&ports, "device 3")), [4]);
    }

    fn device(name: &str, nth: usize) -> Open<()> {
        Open { id: format!("port of {name}"), name: name.to_string(), source: Source::device(nth), _connection: () }
    }

    #[test]
    fn the_devices_that_have_gone_are_taken_out_by_their_places_and_the_others_stay_open_in_their_order() {
        let mut open = vec![device("Launchkey", 0), device("Seaboard", 1), device("IAC", 2), device("Keystation", 3)];
        let gone = take_out(&mut open, &[0, 2]);
        assert_eq!(gone, [Source::device(0), Source::device(2)], "the sources of the devices that went, in the order they were open");
        let left: Vec<&str> = open.iter().map(|device| device.name.as_str()).collect();
        assert_eq!(left, ["Seaboard", "Keystation"], "the places of the others do not matter to which went");

        assert_eq!(take_out(&mut open, &[]), [], "none gone, none taken out");
        assert_eq!(take_out(&mut open, &[0, 1]), [Source::device(1), Source::device(3)]);
        assert!(open.is_empty());
    }
}
