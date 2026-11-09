//! The plugin run the way a DAW runs it (`common`: a host in the same process): the two plugins that the file has, their ports, notes at their frames, audio through, the state kept and given back, and a session that
//! the host gives while the plugin is playing.

mod common;

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::note_ports::{NoteDialect, NoteDialects, NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, ParamRescanFlags, PluginParams};
use clack_host::prelude::*;
use common::*;

const NOTE_ON: [u8; 3] = [0x90, 69, 100];
const NOTE_OFF: [u8; 3] = [0x80, 69, 0];

#[test]
fn the_file_has_two_plugins_that_a_host_lists_by_these_ids_names_and_features() {
    let bundle = bundle();
    let factory = factory(&bundle);
    assert_eq!(factory.plugin_count(), 2);
    let instrument = factory.plugin_descriptor(0).unwrap();
    assert_eq!((instrument.id().unwrap(), instrument.name().unwrap()), (instrument_id(), c"Tang"));
    assert_eq!(instrument.features().map(|feature| feature.to_bytes()).collect::<Vec<_>>(), [&b"instrument"[..], b"synthesizer", b"stereo"]);
    let effect = factory.plugin_descriptor(1).unwrap();
    assert_eq!((effect.id().unwrap(), effect.name().unwrap()), (effect_id(), c"Tang FX"));
    assert_eq!(effect.features().map(|feature| feature.to_bytes()).collect::<Vec<_>>(), [&b"audio-effect"[..], b"stereo"]);
    assert!(factory.plugin_descriptor(2).is_none());
    assert!(instrument.version().is_some() && instrument.vendor().is_some() && instrument.description().is_some());
}

#[test]
fn a_plugin_that_is_not_in_the_file_is_not_made() {
    let bundle = bundle();
    let info = HostInfo::new("a test host", "", "", "").unwrap();
    assert!(PluginInstance::<Handlers>::new(|_| TestShared::default(), |shared| TestMainThread { shared }, &bundle, c"no.such.plugin", &info).is_err());
}

#[test]
fn the_instrument_has_notes_in_and_a_stereo_output_and_the_effect_has_a_stereo_input_as_well() {
    let bundle = bundle();
    for (id, audio_in) in [(instrument_id(), 0), (effect_id(), 1)] {
        let mut instance = instance(&bundle, id);
        let mut handle = instance.plugin_handle();
        let audio = handle.get_extension::<PluginAudioPorts>().unwrap();
        assert_eq!((audio.count(&mut handle, true), audio.count(&mut handle, false)), (audio_in, 1), "{id:?}");
        let mut buffer = AudioPortInfoBuffer::new();
        let out = audio.get(&mut handle, 0, false, &mut buffer).unwrap();
        assert_eq!((out.channel_count, out.flags.contains(AudioPortFlags::IS_MAIN)), (2, true));
        if audio_in == 1 {
            let mut buffer = AudioPortInfoBuffer::new();
            let input = audio.get(&mut handle, 0, true, &mut buffer).unwrap();
            assert_eq!(input.channel_count, 2);
            assert_ne!(input.id, out.id, "an input and an output are not the same port");
        }
        let notes = handle.get_extension::<PluginNotePorts>().unwrap();
        assert_eq!((notes.count(&mut handle, true), notes.count(&mut handle, false)), (1, 0), "{id:?}");
        let mut buffer = NotePortInfoBuffer::new();
        let port = notes.get(&mut handle, 0, true, &mut buffer).unwrap();
        assert_eq!(port.preferred_dialect, Some(NoteDialect::Midi), "the notes that a host sends are MIDI");
        assert!(port.supported_dialects.contains(NoteDialects::MIDI | NoteDialects::CLAP));
    }
}

#[test]
fn the_first_parameter_is_the_switch_that_opens_the_page_and_it_starts_closed() {
    let bundle = bundle();
    for id in [instrument_id(), effect_id()] {
        let mut instance = instance(&bundle, id);
        let mut handle = instance.plugin_handle();
        let params = handle.get_extension::<PluginParams>().expect("the plugin has parameters");
        assert_eq!(params.count(&mut handle), 1 + tang::embed::SLOTS as u32, "{id:?}: the switch and the bank");
        let mut buffer = ParamInfoBuffer::new();
        let info = params.get_info(&mut handle, 0, &mut buffer).expect("its description");
        assert_eq!((info.id, info.name, info.min_value, info.max_value, info.default_value), (ClapId::new(0), &b"Open the page"[..], 0.0, 1.0, 0.0));
        assert!(info.flags.contains(ParamInfoFlags::IS_STEPPED), "a switch has two places");
        assert!(params.get_info(&mut handle, 1 + tang::embed::SLOTS as u32, &mut ParamInfoBuffer::new()).is_none(), "and there is no other after the bank");
        assert_eq!(params.get_value(&mut handle, ClapId::new(0)), Some(0.0), "closed to begin with");
        let past = ClapId::new(1 + tang::embed::SLOTS as u32);
        assert_eq!(params.get_value(&mut handle, past), None, "no parameter past the bank has a value");
        let mut text = [std::mem::MaybeUninit::<u8>::uninit(); 32];
        assert_eq!(params.value_to_text(&mut handle, ClapId::new(0), 0.0, &mut text).unwrap(), b"Closed");
        assert_eq!(params.value_to_text(&mut handle, ClapId::new(0), 1.0, &mut text).unwrap(), b"Open");
        assert!(params.value_to_text(&mut handle, past, 1.0, &mut text).is_err());
        for (written, value) in [(c"open", 1.0), (c"On", 1.0), (c"1", 1.0), (c"closed", 0.0), (c"OFF", 0.0), (c"0", 0.0)] {
            assert_eq!(params.text_to_value(&mut handle, ClapId::new(0), written), Some(value), "{written:?}");
        }
        assert_eq!(params.text_to_value(&mut handle, ClapId::new(0), c"maybe"), None);
        assert_eq!(params.text_to_value(&mut handle, past, c"open"), None);
    }
}

#[test]
fn after_the_switch_the_bank_has_a_parameter_for_each_slot_that_a_host_can_automate_and_modulate_and_that_is_called_macro_and_its_number() {
    let bundle = bundle();
    for id in [instrument_id(), effect_id()] {
        let mut rig = Rig::start(&bundle, id);
        for slot in 0..tang::embed::SLOTS as u32 {
            let facts = rig.param_at(1 + slot).unwrap_or_else(|| panic!("{id:?}: a parameter for slot {slot}"));
            assert_eq!((facts.id, facts.name.as_str(), facts.module.as_str()), (1 + slot, format!("Macro {}", slot + 1).as_str(), "Macros"), "{id:?}");
            assert!(facts.flags.contains(ParamInfoFlags::IS_AUTOMATABLE | ParamInfoFlags::IS_MODULATABLE), "a host automates and modulates it: {:?}", facts.flags);
            assert!(!facts.flags.intersects(ParamInfoFlags::IS_STEPPED | ParamInfoFlags::IS_HIDDEN), "it is a knob, and it is listed");
            assert_eq!(facts.range, (0.0, 1.0, 0.0), "a macro goes from 0 to 1");
            assert_eq!(rig.param_value(1 + slot), Some(0.0), "and starts at rest");
        }
        let mut handle = rig.instance.plugin_handle();
        let params = handle.get_extension::<PluginParams>().unwrap();
        let mut text = [std::mem::MaybeUninit::<u8>::uninit(); 32];
        assert_eq!(params.value_to_text(&mut handle, ClapId::new(1), 0.5, &mut text).unwrap(), b"0.50");
        for (written, value) in [(c"0.25", Some(0.25)), (c" 50% ", Some(0.5)), (c"150%", Some(1.0)), (c"-3", Some(0.0)), (c"nan", None), (c"loud", None), (c"", None)] {
            assert_eq!(params.text_to_value(&mut handle, ClapId::new(2), written), value, "{written:?}");
        }
        rig.stop();
    }
}

/// A session with one macro, called Level, that takes the oscillator's volume down as it is turned up: at 0 the note is as loud as the oscillator makes it, at 1 there is none.
const LEVEL: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n\n[[instrument.modulator]]\ntype = \"macro\"\nvalue = 0.0\nname = \"Level\"\n\n[[instrument.modulator.target]]\nparam = \"volume\"\ndepth = -1.0\n\n[master]\nlimiter = false\n";

/// A rig of the instrument that has been given `LEVEL` and whose bank has been told of it: the macro is the first parameter once the controller's thread has said so (it takes a look at most a tick after the session is there).
fn level_rig(bundle: &PluginBundle) -> Rig {
    let mut rig = Rig::start(bundle, instrument_id());
    rig.load(&state_of(LEVEL)).expect("the session is taken");
    wait_until("the bank to know the macro", || rig.param_at(1).is_some_and(|facts| facts.name == "Macro 1: Level"));
    rig
}

/// Look every few milliseconds until `done` is true, and fail with `what` if it is not within five seconds: the controller's thread does what the host set, and it does it on its own time.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !done() {
        assert!(start.elapsed() < std::time::Duration::from_secs(5), "waited five seconds for {what}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// How loud the instrument plays `NOTE_ON` that was begun some blocks ago: the loudest sample of eight blocks.
fn level(rig: &mut Rig) -> f32 {
    rig.loudest(&[], 8)
}

#[test]
fn a_macro_of_the_session_is_a_parameter_called_by_its_name_and_the_host_is_asked_to_read_the_bank_again() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    assert!(rig.rescans().is_empty() && rig.callbacks() == 0, "a session without a macro asks the host for nothing");
    rig.load(&state_of(LEVEL)).expect("the session is taken");
    wait_until("the bank to know the macro", || rig.param_at(1).is_some_and(|facts| facts.name == "Macro 1: Level"));
    assert_eq!(rig.param_at(2).unwrap().name, "Macro 2", "the slots after it are as they were");
    assert_eq!(rig.param_value(1), Some(0.0));
    assert!(rig.callbacks() > 0, "the controller's thread had the host's main thread called back");
    rig.call_back();
    assert_eq!(rig.rescans(), [(ParamRescanFlags::VALUES | ParamRescanFlags::INFO).bits()], "the host reads values and names, once");
    rig.call_back();
    assert_eq!(rig.rescans().len(), 1, "and a call back that has nothing for it asks for nothing");
    rig.stop();
}

#[test]
fn a_value_that_the_host_sets_for_the_macro_changes_the_sound_and_is_the_value_that_is_saved() {
    let bundle = bundle();
    let mut rig = level_rig(&bundle);
    rig.play(&[(0, NOTE_ON)]);
    let loud = level(&mut rig);
    assert!(loud > 0.05, "at rest the note is as loud as the oscillator is");
    let (_, _, said) = rig.block_params(&[], &[Param::Value(1, 1.0)]);
    assert!(said.is_empty(), "the host is not told back what it set: {said:?}");
    let mut quietest = f32::MAX;
    wait_until("the macro to take the oscillator's volume down", || {
        quietest = level(&mut rig);
        quietest < 1e-3
    });
    assert!(quietest < 1e-3, "at full there is no volume left");
    let saved = rig.save();
    let text = String::from_utf8(saved[9..].to_vec()).unwrap();
    assert!(text.contains("type = \"macro\"") && text.contains("value = 1"), "the value that the host set is the one that the session keeps:\n{text}");
    assert_eq!(rig.param_value(1), Some(1.0));
    // A value that the host sets when it is not processing is taken as well.
    let said = rig.flush_params(&[Param::Value(1, 0.0)]);
    assert!(said.is_empty());
    wait_until("the macro to be at rest again", || level(&mut rig) > loud * 0.9);
    assert_eq!(rig.param_value(1), Some(0.0));
    rig.stop();
}

#[test]
fn the_hosts_modulation_is_added_to_what_is_played_and_the_value_that_was_set_stays_what_is_saved_and_shown() {
    let bundle = bundle();
    let mut rig = level_rig(&bundle);
    rig.play(&[(0, NOTE_ON)]);
    let full = level(&mut rig);
    rig.block_params(&[], &[Param::Modulation(1, 0.5)]);
    let mut half = 0.0;
    wait_until("the modulation to take the volume down by half", || {
        half = level(&mut rig);
        half < full * 0.65
    });
    assert!(half > full * 0.35, "the macro is at a half: the volume is a half ({half} against {full})");
    let text = String::from_utf8(rig.save()[9..].to_vec()).unwrap();
    assert!(text.contains("type = \"macro\"") && text.contains("value = 0.0"), "the session keeps the value that was set, not what the modulation made of it:\n{text}");
    assert_eq!(rig.param_value(1), Some(0.0), "and the host that asks is told that");
    rig.block_params(&[], &[Param::Modulation(1, 0.0)]);
    wait_until("the modulation to end", || level(&mut rig) > full * 0.9);
    rig.stop();
}

/// The text of the session that the plugin would keep in the host's project.
fn session_of(rig: &mut Rig) -> String {
    String::from_utf8(rig.save()[9..].to_vec()).unwrap()
}

#[test]
fn the_hosts_tempo_is_the_sessions_and_follows_it_when_it_moves() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    assert!(!session_of(&mut rig).contains("bpm"), "a new session has the tempo that every one has, and says none");
    rig.block_time(&[], &time(Some(93.0), None));
    wait_until("the session to have the host's tempo", || session_of(&mut rig).contains("bpm = 93"));
    rig.block_time(&[], &time(Some(133.5), Some(4.0)));
    wait_until("the session to follow the host's tempo", || session_of(&mut rig).contains("bpm = 133.5"));
    assert_eq!(rig.dirty_marks(), 0, "the host is not told that its project has changed: its own tempo is not an edit");
    // A host that says no tempo leaves the one that the session has.
    rig.block_time(&[], &time(None, None));
    std::thread::sleep(std::time::Duration::from_millis(120));
    assert!(session_of(&mut rig).contains("bpm = 133.5"));
    rig.stop();
}

/// A session with a square LFO, a cycle of which is a quarter note (a beat) and which does not start again with a note, that takes the oscillator's volume to a half in the half of the cycle that it is high in.
const SYNCED: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n\n[[instrument.modulator]]\ntype = \"lfo\"\nwaveform = \"square\"\nsync = \"1/4\"\n\n[[instrument.modulator.target]]\nparam = \"volume\"\ndepth = -0.5\n\n[master]\nlimiter = false\n";

/// How loud the instrument is when the host is at `beat`: the blocks go on at that beat until the sound has settled (the oscillator takes a moment to go to a volume), and the loudest sample of the last is what it is.
fn loudness_at(rig: &mut Rig, tempo: f64, beat: f64) -> f32 {
    let mut loudest = 0.0f32;
    for _ in 0..12 {
        let (left, right) = rig.block_time(&[], &time(Some(tempo), Some(beat)));
        loudest = left.iter().chain(&right).fold(0.0f32, |loudest, sample| loudest.max(sample.abs()));
    }
    loudest
}

#[test]
fn an_lfo_that_follows_the_tempo_is_where_the_hosts_beat_says_in_what_is_heard_whatever_the_plugin_did_before() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    rig.load(&state_of(SYNCED)).expect("the session is taken");
    rig.block_time(&[(0, NOTE_ON)], &time(Some(120.0), Some(0.0)));
    // The first and the second half of a cycle (a beat) are the loud and the quiet one, or the other way round: they are not alike.
    let first = loudness_at(&mut rig, 120.0, 0.25);
    let second = loudness_at(&mut rig, 120.0, 0.75);
    let (loud, quiet) = (first.max(second), first.min(second));
    assert!(loud > 0.05 && quiet < loud * 0.7, "a half of the cycle takes the volume down: {first} and {second}");
    // The same places a beat, and many beats, later are the same, and the host that goes back gets what it got before.
    assert!((loudness_at(&mut rig, 120.0, 1.25) - first).abs() < loud * 0.02, "a beat later: the same half");
    assert!((loudness_at(&mut rig, 120.0, 1.75) - second).abs() < loud * 0.02);
    assert!((loudness_at(&mut rig, 120.0, 100.25) - first).abs() < loud * 0.02, "a hundred beats on");
    assert!((loudness_at(&mut rig, 120.0, 0.25) - first).abs() < loud * 0.02, "and back");
    // At another tempo a beat is as long and is the same cycle: the beat says where, and the tempo how fast it would go.
    assert!((loudness_at(&mut rig, 77.0, 0.75) - second).abs() < loud * 0.02);
    rig.stop();
}

/// How many times the instrument's volume goes from the loud half to the quiet half or back in `blocks` blocks (the first ones are left out: the note begins) while the host says a tempo and no beat, as a host that is not playing does:
/// an LFO that follows the tempo and goes by itself goes round as fast as the tempo makes it.
fn turns_of_the_volume(rig: &mut Rig, tempo: f64, blocks: usize) -> usize {
    rig.block_time(&[(0, NOTE_ON)], &time(Some(tempo), None));
    let levels: Vec<f32> = (0..blocks)
        .map(|_| {
            let (left, right) = rig.block_time(&[], &time(Some(tempo), None));
            left.iter().chain(&right).fold(0.0f32, |loudest, sample| loudest.max(sample.abs()))
        })
        .collect();
    let top = levels.iter().copied().fold(0.0f32, f32::max);
    let loud: Vec<bool> = levels.iter().skip(20).map(|level| *level > top * 0.75).collect();
    loud.windows(2).filter(|pair| pair[0] != pair[1]).count()
}

#[test]
fn the_hosts_tempo_alone_is_the_tempo_that_an_lfo_that_follows_it_goes_by_when_the_host_does_not_play() {
    let bundle = bundle();
    // A square LFO that goes round in a thirty-second note: 8 times a second at 60 beats a minute and 32 at 240.
    let fast = SYNCED.replace("1/4", "1/32");
    let mut rig = Rig::start(&bundle, instrument_id());
    rig.load(&state_of(&fast)).expect("the session is taken");
    let slow = turns_of_the_volume(&mut rig, 60.0, 140);
    let quick = turns_of_the_volume(&mut rig, 240.0, 140);
    assert!(slow >= 4, "it goes round at 60 beats a minute: {slow} turns");
    assert!(quick > slow * 2, "and at four times the tempo it goes round about four times as fast: {quick} turns against {slow}");
    rig.stop();
}

#[test]
fn a_value_or_a_modulation_for_a_parameter_that_is_not_in_the_bank_is_left_alone() {
    let bundle = bundle();
    let mut rig = level_rig(&bundle);
    rig.play(&[(0, NOTE_ON)]);
    let full = level(&mut rig);
    let (.., said) = rig.block_params(&[], &[Param::Value(99, 1.0), Param::Modulation(99, 1.0), Param::Value(17, 1.0), Param::Value(2, 1.0), Param::Value(1, f64::NAN)]);
    assert!(said.is_empty());
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(level(&mut rig) > full * 0.9, "no macro was turned: slot 2 has none, the others are not slots, and a number that is not one is not set");
    rig.stop();
}

#[test]
fn the_instrument_plays_the_midi_that_a_host_sends_from_the_frame_it_says_and_is_silent_when_it_ends() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    assert_eq!(rig.loudest(&[], 4), 0.0, "nothing is playing");
    let (left, right) = rig.play(&[(100, NOTE_ON)]);
    assert!(left[..100].iter().chain(&right[..100]).all(|&sample| sample == 0.0), "silence up to the note's frame, over what the host had in its buffers");
    assert!(left[100..].iter().any(|&sample| sample.abs() > 0.01), "and the note from it");
    assert!(rig.loudest(&[], 8) > 0.05, "a held note goes on");
    assert!(rig.settled(&[(0, NOTE_OFF)], 60) < 1e-4, "silent when it has ended");
    rig.stop();
}

#[test]
fn a_host_that_gives_one_channel_has_the_left_side_played_into_it() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    let mut loudest = 0.0f32;
    for block in 0..8 {
        let mut input_events = EventBuffer::with_capacity(1);
        if block == 0 {
            input_events.push(&clack_host::events::event_types::MidiEvent::new(0, 0, NOTE_ON));
        }
        let mut mono = vec![9.0f32; BLOCK];
        let mut ports = AudioPorts::with_capacity(1, 1);
        let mut outputs = ports.with_output_buffers([AudioPortBuffer { channels: AudioPortBufferType::f32_output_only([mono.as_mut_slice()].into_iter()), latency: 0 }]);
        let mut output_events = EventBuffer::with_capacity(1);
        rig.processor.as_mut().unwrap().process(&InputAudioBuffers::empty(), &mut outputs, &input_events.as_input(), &mut output_events.as_output(), None, None).unwrap();
        loudest = mono.iter().fold(loudest, |loudest, sample| loudest.max(sample.abs()));
    }
    // (The buffer was full of 9s: a plugin that leaves it alone is as loud as that, which is not what a note is.)
    assert!(loudest > 0.05 && loudest < 1.0, "a host with one channel is played into, over what it had in its buffer: {loudest}");
    rig.stop();
}

#[test]
fn a_block_of_no_frames_is_processed_without_trouble_and_the_next_block_plays() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    let (left, right) = rig.play_frames(&[(0, NOTE_ON)], 0);
    assert!((left.len(), right.len()) == (0, 0), "a block of nothing has nothing in it");
    assert_eq!(rig.loudest(&[], 4), 0.0, "and the note that came with it, which has no frame to be at, is not played");
    assert!(rig.loudest(&[(0, NOTE_ON)], 8) > 0.05, "the next block that has frames plays what it is sent");
    rig.stop();
}

#[test]
fn the_notes_of_claps_own_dialect_are_played_too() {
    use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent};
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    // (Through the same block function with its own events: a note on key 69, channel 0.)
    let mut input_events = EventBuffer::with_capacity(2);
    input_events.push(&NoteOnEvent::new(0, Pckn::new(0u16, 0u16, 69u16, 1u32), 0.8));
    let (mut left, mut right) = (vec![9.0f32; BLOCK], vec![9.0f32; BLOCK]);
    let mut ports = AudioPorts::with_capacity(2, 1);
    let mut outputs = ports.with_output_buffers([AudioPortBuffer {
        channels: AudioPortBufferType::f32_output_only([left.as_mut_slice(), right.as_mut_slice()].into_iter()),
        latency: 0,
    }]);
    let mut output_events = EventBuffer::with_capacity(1);
    rig.processor.as_mut().unwrap().process(&InputAudioBuffers::empty(), &mut outputs, &input_events.as_input(), &mut output_events.as_output(), None, None).unwrap();
    assert!(left.iter().any(|&sample| sample.abs() > 0.01), "a note in the CLAP dialect plays");
    let mut off = EventBuffer::with_capacity(1);
    off.push(&NoteOffEvent::new(0, Pckn::new(0u16, 0u16, 69u16, 1u32), 0.0));
    let mut ports = AudioPorts::with_capacity(2, 1);
    let (mut left, mut right) = (vec![9.0f32; BLOCK], vec![9.0f32; BLOCK]);
    let mut outputs = ports.with_output_buffers([AudioPortBuffer {
        channels: AudioPortBufferType::f32_output_only([left.as_mut_slice(), right.as_mut_slice()].into_iter()),
        latency: 0,
    }]);
    rig.processor.as_mut().unwrap().process(&InputAudioBuffers::empty(), &mut outputs, &off.as_input(), &mut output_events.as_output(), None, None).unwrap();
    assert!(rig.settled(&[], 60) < 1e-4, "and ends");
    rig.stop();
}

#[test]
fn a_reset_silences_what_is_sounding() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    assert!(rig.loudest(&[(0, NOTE_ON)], 8) > 0.05);
    rig.processor.as_mut().unwrap().reset();
    assert!(rig.settled(&[], 60) < 1e-4, "the note that was held has been let go of");
    rig.stop();
}

/// A sine of 0.25 for a block, on both sides.
fn sine() -> [Vec<f32>; 2] {
    let side: Vec<f32> = (0..BLOCK).map(|i| 0.25 * (std::f32::consts::TAU * 480.0 * i as f32 / RATE as f32).sin()).collect();
    [side.clone(), side]
}

#[test]
fn the_effect_plays_what_comes_in_and_a_silent_input_is_a_silent_output() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, effect_id());
    let silence = [vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]];
    let mut loudest = 0.0f32;
    for _ in 0..8 {
        let (left, right) = rig.filter(&silence);
        loudest = left.iter().chain(&right).fold(loudest, |loudest, sample| loudest.max(sample.abs()));
    }
    assert_eq!(loudest, 0.0, "nothing in, nothing out");
    for _ in 0..40 {
        let (left, right) = rig.filter(&sine());
        loudest = left.iter().chain(&right).fold(0.0f32, |loudest, sample| loudest.max(sample.abs()));
    }
    assert!((0.2..=0.3).contains(&loudest), "what comes in is what goes out: {loudest}");
    rig.stop();
}

#[test]
fn the_effect_is_played_what_comes_in_when_the_host_gives_the_input_and_the_output_as_the_same_memory() {
    use clack_host::process::audio_buffers::{InputAudioBuffers, OutputAudioBuffers};
    use clap_sys::audio_buffer::clap_audio_buffer;
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, effect_id());
    let mut loudest = 0.0f32;
    for _ in 0..40 {
        let [mut left, mut right] = sine();
        let mut pointers = [left.as_mut_ptr(), right.as_mut_ptr()];
        let buffer = clap_audio_buffer { data32: pointers.as_mut_ptr(), data64: std::ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
        let in_buffers = [buffer];
        let mut out_buffers = [buffer];
        // SAFETY: the pointers are those of two buffers of `BLOCK` samples that are alive for the call, and the input is the output, as a host that processes in place gives it.
        let (inputs, mut outputs) = unsafe { (InputAudioBuffers::from_raw_buffers(&in_buffers, BLOCK as u32), OutputAudioBuffers::from_raw_buffers(&mut out_buffers, BLOCK as u32)) };
        let (events, mut out_events) = (EventBuffer::with_capacity(1), EventBuffer::with_capacity(1));
        rig.processor.as_mut().unwrap().process(&inputs, &mut outputs, &events.as_input(), &mut out_events.as_output(), None, None).unwrap();
        loudest = left.iter().chain(&right).fold(0.0f32, |loudest, sample| loudest.max(sample.abs()));
    }
    assert!((0.2..=0.3).contains(&loudest), "in place: what comes in is what goes out: {loudest}");
    rig.stop();
}

#[test]
fn the_state_is_the_session_and_another_instance_that_is_given_it_plays_it() {
    let bundle = bundle();
    // A host that opens a project gives the state before it activates the plugin.
    let mut quiet = instance(&bundle, instrument_id());
    {
        let mut handle = quiet.plugin_handle();
        let state = handle.get_extension::<clack_extensions::state::PluginState>().unwrap();
        state.load(&mut handle, &mut std::io::Cursor::new(state_of("[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.0\n"))).unwrap();
    }
    let processor = quiet
        .activate(|_, _| TestAudioProcessor, PluginAudioConfiguration { sample_rate: RATE, min_frames_count: 1, max_frames_count: BLOCK as u32 })
        .unwrap()
        .start_processing()
        .unwrap();
    let mut rig = Rig { instance: quiet, processor: Some(processor) };
    assert!(rig.loudest(&[(0, NOTE_ON)], 8) < 1e-4, "the oscillator of the state has no volume");
    let saved = rig.save();
    assert_eq!(&saved[..8], b"TANGSESS", "what is kept says what it is");
    let text = String::from_utf8(saved[9..].to_vec()).unwrap();
    assert!(text.contains("builtin:osc") && text.contains("volume = 0"), "the state is the session:\n{text}");
    rig.stop();

    // A new instance given the bytes that were saved plays the same.
    let mut again = Rig::start(&bundle, instrument_id());
    assert!(again.loudest(&[(0, NOTE_ON)], 8) > 0.05, "a new instance is a new session: the oscillator sounds");
    again.load(&saved).expect("the state is taken");
    assert!(again.loudest(&[(0, NOTE_OFF), (0, NOTE_ON)], 8) < 1e-4, "given the state while it plays, it plays that");
    again.stop();
}

#[test]
fn a_state_that_is_not_a_session_is_refused_and_the_plugin_goes_on_with_what_it_had() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    assert!(rig.load(&[0xFF, 0xFE, 0x00, 0x01]).is_err(), "bytes that are not text");
    let mut later = state_of("x");
    later[8] = 9;
    assert!(rig.load(&later).is_err(), "a version that this does not know");
    assert!(rig.loudest(&[(0, NOTE_ON)], 8) > 0.05, "it still plays");
    rig.stop();
}

#[test]
fn a_state_that_cannot_be_loaded_gives_a_new_session_and_is_what_is_saved_until_something_is_changed() {
    let bundle = bundle();
    let broken = "[[instrument]]\nplugin = \"builtin:no-such-thing\"\n";
    let mut rig = Rig::start(&bundle, instrument_id());
    rig.load(&state_of(broken)).expect("the state is taken: a plugin that is not installed is not the host's business");
    assert!(rig.loudest(&[(0, NOTE_ON)], 8) > 0.05, "a new session plays in its place");
    let saved = rig.save();
    assert_eq!(&saved[9..], broken.as_bytes(), "a project that is opened and saved does not lose the session that this machine could not play");
    rig.stop();
}

#[test]
fn every_instance_is_a_session_of_its_own() {
    let bundle = bundle();
    let mut a = Rig::start(&bundle, instrument_id());
    let mut b = Rig::start(&bundle, instrument_id());
    b.load(&state_of("[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.0\n")).unwrap();
    assert!(a.loudest(&[(0, NOTE_ON)], 8) > 0.05);
    assert!(b.loudest(&[(0, NOTE_ON)], 8) < 1e-4, "the other has its own");
    a.stop();
    b.stop();
}

#[test]
fn a_plugin_is_activated_again_at_another_rate_and_plays_the_same_session() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    rig.load(&state_of("[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.5\n")).unwrap();
    let before = rig.save();
    let Rig { mut instance, processor } = rig;
    instance.deactivate(processor.unwrap().stop_processing());
    let processor = instance
        .activate(|_, _| TestAudioProcessor, PluginAudioConfiguration { sample_rate: 44_100.0, min_frames_count: 1, max_frames_count: BLOCK as u32 })
        .unwrap()
        .start_processing()
        .unwrap();
    let mut rig = Rig { instance, processor: Some(processor) };
    assert!(rig.loudest(&[(0, NOTE_ON)], 8) > 0.02, "it plays at the new rate");
    let after = rig.save();
    assert_eq!(before, after, "the session came through the change of rate as it was");
    rig.stop();
}
