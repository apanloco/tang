use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Receiver;

use crate::plugin::chain::AudioGraph;

/// A MIDI event: (frame_offset, raw_bytes).
/// Standard MIDI messages are 1–3 bytes; we use a fixed array to avoid heap allocation.
pub type MidiEvent = (u64, [u8; 3]);

/// Select the best audio host for the current platform.
/// On Linux, prefer JACK (PipeWire exposes a JACK interface) to avoid the
/// ALSA compatibility layer which can destabilize PipeWire's global quantum.
#[cfg(target_os = "linux")]
fn select_audio_host(buffer_size: u32, sample_rate: u32) -> cpal::Host {
    let available = cpal::available_hosts();
    if available.contains(&cpal::HostId::Jack) {
        // Request our latency from PipeWire as a cooperative hint. PIPEWIRE_LATENCY
        // is per-client: PipeWire negotiates the global quantum as the min across
        // active clients, and other apps recover when ours is paused or xruns.
        // PIPEWIRE_QUANTUM, by contrast, would force the global quantum and make
        // tang's xruns crackle YouTube and every other PipeWire client.
        let latency = format!("{buffer_size}/{sample_rate}");
        // SAFETY: called once at startup before other threads read env vars.
        unsafe { std::env::set_var("PIPEWIRE_LATENCY", &latency) };
        log::info!("Set PIPEWIRE_LATENCY={latency}");

        match cpal::host_from_id(cpal::HostId::Jack) {
            Ok(h) => {
                // Verify JACK can actually find an output device before committing.
                // Without PipeWire's JACK bridge (pw-jack or ld.so.conf), the host
                // object is created but cannot connect to any server.
                if h.default_output_device().is_some() {
                    log::info!("Using JACK audio host (PipeWire)");
                    return h;
                }
                log::warn!("JACK host has no output devices, falling back to ALSA");
            }
            Err(_) => {
                log::warn!("JACK host unavailable, falling back to ALSA");
            }
        }
        // SAFETY: same single-threaded startup context.
        unsafe { std::env::remove_var("PIPEWIRE_LATENCY") };
    }
    log::info!("Using default audio host (ALSA)");
    cpal::default_host()
}

#[cfg(not(target_os = "linux"))]
fn select_audio_host(_buffer_size: u32, _sample_rate: u32) -> cpal::Host {
    log::info!("Using default audio host");
    cpal::default_host()
}

/// Attempt to promote the current thread to SCHED_FIFO real-time scheduling.
/// Called once from the audio callback thread on its first invocation.
/// Fails silently (with a log warning) if the process lacks `rtprio` privileges.
#[cfg(target_os = "linux")]
fn promote_to_realtime() {
    unsafe {
        // Check if the thread already has RT scheduling (e.g. PipeWire's JACK
        // backend promotes callback threads via rtkit).
        let mut current_param: libc::sched_param = std::mem::zeroed();
        let mut policy: i32 = 0;
        if libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut current_param) == 0 {
            // Mask off flag bits — PipeWire may set extra bits on the policy value.
            let base_policy = policy & 0xF;
            if base_policy == libc::SCHED_FIFO || base_policy == libc::SCHED_RR {
                log::info!(
                    "Audio thread already has RT scheduling (policy={}, priority={})",
                    if base_policy == libc::SCHED_FIFO { "FIFO" } else { "RR" },
                    current_param.sched_priority
                );
                return;
            }
        }

        let param = libc::sched_param { sched_priority: 50 };
        let ret = libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param);
        if ret == 0 {
            log::info!("Audio thread promoted to SCHED_FIFO priority 50");
        } else {
            log::warn!(
                "Could not set real-time scheduling (err {ret}). \
                 For RT audio, add your user to the 'audio' group \
                 and ensure rtprio is set in /etc/security/limits.d/"
            );
        }
    }
}

#[cfg(target_os = "macos")]
fn promote_to_realtime() {
    // macOS cpal (CoreAudio) already runs the audio callback on a real-time thread.
    log::info!("macOS: CoreAudio handles real-time scheduling");
}

#[cfg(target_os = "windows")]
fn promote_to_realtime() {
    // Windows cpal (WASAPI) already uses MMCSS for real-time audio.
    log::info!("Windows: WASAPI handles real-time scheduling");
}

/// What an audio callback does with the audio thread's end of the session, apart
/// from where the samples go: take the MIDI that has arrived, render the graph into
/// one buffer per channel, and interleave them into the output. The buffers are
/// made once and reused, so a block is rendered without allocating.
struct Renderer {
    graph: AudioGraph,
    midi_rx: Receiver<MidiEvent>,
    /// The MIDI that arrived since the last block.
    midi_events: Vec<MidiEvent>,
    /// One buffer per channel, a block long.
    channel_bufs: Vec<Vec<f32>>,
    num_channels: usize,
}

impl Renderer {
    fn new(graph: AudioGraph, midi_rx: Receiver<MidiEvent>, buffer_size: usize) -> Self {
        let num_channels = graph.num_channels();
        Renderer {
            graph,
            midi_rx,
            midi_events: Vec::with_capacity(64),
            channel_bufs: (0..num_channels).map(|_| vec![0.0f32; buffer_size]).collect(),
            num_channels,
        }
    }

    /// Fill `data`, interleaved (`frames * channels` samples), with the next block;
    /// `cb_num` is the number of the block, for the log. Silence if the graph fails.
    fn render(&mut self, data: &mut [f32], cb_num: u64) {
        let num_channels = self.num_channels;

        // Drain all pending MIDI events (reuse the pre-allocated vec)
        self.midi_events.clear();
        while let Ok(event) = self.midi_rx.try_recv() {
            self.midi_events.push(event);
        }

        if !self.midi_events.is_empty() {
            log::debug!(
                "Audio cb #{cb_num}: processing {} MIDI event(s) into {} frames",
                self.midi_events.len(),
                data.len() / num_channels
            );
        }

        let frames = data.len() / num_channels;

        // Resize and zero pre-allocated per-channel buffers
        for buf in self.channel_bufs.iter_mut() {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }

        if let Err(e) = self.graph.process(&self.midi_events, &mut self.channel_bufs) {
            log::error!("Audio graph process error: {e}");
            data.fill(0.0);
            return;
        }

        // Interleave back into the output buffer
        for frame in 0..frames {
            for ch in 0..num_channels {
                data[frame * num_channels + ch] = self.channel_bufs[ch][frame];
            }
        }

        // Log peak level when there were MIDI events
        if !self.midi_events.is_empty() {
            let peak = data.iter().fold(0.0f32, |max, &s| max.max(s.abs()));
            log::debug!("Audio cb #{cb_num}: output peak = {peak:.6}");
        }
    }
}

pub struct AudioEngine {
    stream: cpal::Stream,
}

impl AudioEngine {
    /// Stop the audio stream. Call this before dropping the plugin.
    pub fn stop(self) {
        // Pause the stream first so the callback stops being invoked
        if let Err(e) = self.stream.pause() {
            log::warn!("Failed to pause audio stream: {e}");
        }
        // Give the audio callback time to finish if it's mid-flight
        std::thread::sleep(std::time::Duration::from_millis(50));
        // Now drop the stream
        drop(self.stream);
        log::info!("Audio stream stopped");
    }

    /// Start playing the audio stream. Call after initial commands are queued.
    pub fn play(&self) -> anyhow::Result<()> {
        self.stream.play()?;
        log::info!("Audio stream started");
        Ok(())
    }

    /// Build the audio engine but don't start playing yet.
    /// Call `play()` after queuing initial graph commands.
    pub fn build(
        graph: AudioGraph,
        midi_rx: Receiver<MidiEvent>,
        device_name: Option<&str>,
        sample_rate: u32,
        buffer_size: u32,
    ) -> anyhow::Result<Self> {
        let host = select_audio_host(buffer_size, sample_rate);

        let device = if let Some(name) = device_name {
            host.output_devices()?
                .find(|d| d.name().map(|n| n.contains(name)).unwrap_or(false))
                .ok_or_else(|| anyhow::anyhow!("Audio device not found: {name}"))?
        } else {
            host.default_output_device()
                .ok_or_else(|| anyhow::anyhow!("No default audio output device"))?
        };

        let dev_name = device.name().unwrap_or_else(|_| "Unknown".into());
        log::info!("Using audio device: {dev_name}");

        let num_channels = graph.num_channels();

        let config = cpal::StreamConfig {
            channels: num_channels as u16,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Fixed(buffer_size),
        };

        log::info!(
            "Audio config: {}ch, {}Hz, buffer={}",
            num_channels,
            sample_rate,
            buffer_size
        );

        // The renderer lives in the closure; its buffers are reused every callback
        let mut renderer = Renderer::new(graph, midi_rx, buffer_size as usize);

        let mut callback_count: u64 = 0;

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let cb_num = callback_count;
                callback_count += 1;

                // Log first callback to confirm audio is running
                if cb_num == 0 {
                    log::info!("Audio callback running (first call, {} frames)", data.len() / num_channels);
                    promote_to_realtime();
                }

                renderer.render(data, cb_num);
            },
            move |err| {
                let msg = err.to_string();
                if msg.contains("buffer size changed") {
                    log::info!("Audio: {msg}");
                } else {
                    log::error!("Audio stream error: {msg}");
                }
            },
            None,
        )?;

        Ok(AudioEngine { stream })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::load_onto;

    const NOTE_ON_A4: MidiEvent = (0, [0x90, 69, 100]);

    /// A renderer for `session`, with the channels it listens on and the audio
    /// thread's end of the plugin hand-back channel kept alive.
    struct Rig {
        renderer: Renderer,
        midi_tx: crossbeam_channel::Sender<MidiEvent>,
        _return_rx: Receiver<Box<dyn crate::plugin::Plugin>>,
    }

    fn rig(session: &str, buffer_size: usize) -> Rig {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let (midi_tx, midi_rx) = crossbeam_channel::bounded(1024);
        let graph = AudioGraph::new(2, cmd_rx, return_tx);
        let mut renderer = Renderer::new(graph, midi_rx, buffer_size);
        load_onto(session, &cmd_tx);
        renderer.graph.drain_commands();
        Rig { renderer, midi_tx, _return_rx: return_rx }
    }

    #[test]
    fn a_renderer_with_nothing_in_the_graph_renders_silence() {
        let mut rig = rig("", 64);
        let mut data = vec![0.5f32; 64 * 2];
        rig.renderer.render(&mut data, 0);
        assert!(data.iter().all(|&s| s == 0.0), "the block is overwritten, not mixed into");
    }

    #[test]
    fn a_note_sent_before_the_block_sounds_in_it_and_the_channels_are_interleaved() {
        // Hard left: the right channel is silent, so the interleaving shows.
        let mut rig = rig("[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\npan = -1.0\n", 256);
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        let mut data = vec![0.0f32; 256 * 2];
        // The note is on from the first block; its envelope has risen a few blocks in.
        for cb in 0..4 {
            rig.renderer.render(&mut data, cb);
        }
        let left: Vec<f32> = data.iter().step_by(2).copied().collect();
        let right: Vec<f32> = data.iter().skip(1).step_by(2).copied().collect();
        let peak = |channel: &[f32]| channel.iter().fold(0.0f32, |max, &s| max.max(s.abs()));
        assert!(peak(&left) > 0.1, "the left channel carries the note: {left:?}");
        // The pan is smoothed over a few milliseconds, so the right channel has not
        // quite settled to silence; it is far below the left.
        assert!(peak(&right) < peak(&left) / 10.0, "and the right is nearly silent: {right:?}");
    }

    #[test]
    fn a_block_is_as_long_as_the_buffer_it_is_given() {
        let mut rig = rig("[[instrument]]\nplugin = \"builtin:osc\"\n", 256);
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        // Shorter than the size the renderer was made for, and then longer: both are fine.
        for frames in [100, 300] {
            let mut data = vec![0.0f32; frames * 2];
            rig.renderer.render(&mut data, 0);
            assert_eq!(data.len(), frames * 2);
        }
    }

    #[test]
    fn the_midi_of_one_block_is_not_rendered_again_in_the_next() {
        let mut rig = rig("[[instrument]]\nplugin = \"builtin:osc\"\n", 64);
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        let mut data = vec![0.0f32; 64 * 2];
        rig.renderer.render(&mut data, 0);
        assert_eq!(rig.renderer.midi_events.len(), 1);
        rig.renderer.render(&mut data, 1);
        assert!(rig.renderer.midi_events.is_empty(), "nothing arrived, so nothing is in the second block");
    }

    #[test]
    fn a_block_is_rendered_from_zero_not_on_top_of_the_one_before() {
        // A note that has been released and has died away leaves nothing behind.
        let mut rig = rig("[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\nrelease = 0.002\n", 64);
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        let mut data = vec![0.0f32; 64 * 2];
        rig.renderer.render(&mut data, 0);
        assert!(data.iter().any(|&s| s != 0.0));
        rig.midi_tx.send((0, [0x80, 69, 0])).unwrap();
        for cb in 1..20 {
            rig.renderer.render(&mut data, cb);
        }
        assert!(data.iter().all(|&s| s == 0.0), "the tail is over, and the block is silent: {:?}", &data[..8]);
    }

    /// An instrument whose `process` always fails.
    struct Failing;

    impl crate::plugin::Plugin for Failing {
        fn name(&self) -> &str {
            "Failing"
        }
        fn is_instrument(&self) -> bool {
            true
        }
        fn sample_rate(&self) -> f32 {
            48_000.0
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            0
        }
        fn process(&mut self, _: &[(u64, [u8; 3])], _: &[&[f32]], _: &mut [&mut [f32]]) -> anyhow::Result<()> {
            anyhow::bail!("this plugin cannot render")
        }
        fn parameters(&self) -> Vec<crate::plugin::ParameterInfo> {
            Vec::new()
        }
        fn get_parameter(&mut self, _: u32) -> Option<f32> {
            None
        }
        fn set_parameter(&mut self, _: u32, _: f32) -> anyhow::Result<()> {
            Ok(())
        }
        fn presets(&self) -> Vec<crate::plugin::Preset> {
            Vec::new()
        }
        fn load_preset(&mut self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_graph_that_fails_leaves_silence_in_the_block() {
        use crate::plugin::chain::GraphCommand;
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(8);
        let (return_tx, _return_rx) = crossbeam_channel::bounded(8);
        let (midi_tx, midi_rx) = crossbeam_channel::bounded(8);
        let mut renderer = Renderer::new(AudioGraph::new(2, cmd_rx, return_tx), midi_rx, 64);
        cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: Box::new(Failing),
                inst_buf: vec![Vec::new(), Vec::new()],
                remapper: None,
            })
            .unwrap();
        midi_tx.send(NOTE_ON_A4).unwrap();
        let mut data = vec![0.7f32; 64 * 2];
        renderer.render(&mut data, 0);
        assert!(data.iter().all(|&s| s == 0.0), "what the callback was handed is not played");
    }

    #[test]
    fn midi_that_arrived_is_taken_off_the_channel() {
        let mut rig = rig("[[instrument]]\nplugin = \"builtin:osc\"\n", 64);
        for _ in 0..3 {
            rig.midi_tx.send(NOTE_ON_A4).unwrap();
        }
        let mut data = vec![0.0f32; 64 * 2];
        rig.renderer.render(&mut data, 0);
        assert!(rig.midi_tx.is_empty(), "all of it, in one block");
    }
}
