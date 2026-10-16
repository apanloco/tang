use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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

/// While one of these is alive, the thread's floating point unit takes the
/// numbers nearer to zero than a normal 32-bit float can be (denormals, under
/// 1.2e-38) as zero, in what it computes and in what it computes with, and
/// the state it had is given back when this goes.
///
/// The tails of filters, reverbs and envelopes run down through the denormals,
/// and arithmetic on them takes many times as long on some processors (a
/// hundred times on x86): the tail of a filter that was shut made more than half
/// the samples of its two seconds denormal. Inaudible and slow is what to flush,
/// for the plugins the block runs as much as for the built-in ones. A guard is for the
/// time of a block and not set once for the thread, since a host's callback is
/// not always on a thread of its own and a plugin may change the unit.
struct NoDenormals {
    saved: fpu::State,
    /// The state is the thread's: this is put back on the thread it was taken from.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl NoDenormals {
    fn new() -> Self {
        NoDenormals { saved: fpu::flush_denormals(), _not_send: std::marker::PhantomData }
    }
}

impl Drop for NoDenormals {
    fn drop(&mut self) {
        fpu::restore(self.saved);
    }
}

/// The floating point control register of the thread, where the processor has one that this knows.
#[cfg(target_arch = "x86_64")]
mod fpu {
    use std::arch::asm;

    /// MXCSR: the unit's state.
    pub type State = u32;

    /// Flush to zero (bit 15: a result that is a denormal is zero) and denormals are zero (bit 6: an operand that is one is).
    const FLUSH: State = (1 << 15) | (1 << 6);

    /// Turn the flushing on, and say what the state was.
    pub fn flush_denormals() -> State {
        let mut saved: State = 0;
        // SAFETY: `stmxcsr` stores this thread's MXCSR through the pointer, to a variable that is here.
        unsafe { asm!("stmxcsr [{}]", in(reg) &mut saved, options(nostack, preserves_flags)) };
        restore(saved | FLUSH);
        saved
    }

    pub fn restore(state: State) {
        // SAFETY: `ldmxcsr` loads this thread's MXCSR from the pointer, to a variable that is here. The bits are the ones
        // `stmxcsr` gave with the flushing ones set, so none that is reserved is.
        unsafe { asm!("ldmxcsr [{}]", in(reg) &state, options(nostack, readonly, preserves_flags)) };
    }
}

#[cfg(target_arch = "aarch64")]
mod fpu {
    use std::arch::asm;

    /// FPCR: the unit's control register.
    pub type State = u64;

    /// Flush to zero (bit 24: an operand that is a denormal is zero, and so is a result that is one).
    const FLUSH: State = 1 << 24;

    /// Turn the flushing on, and say what the state was.
    pub fn flush_denormals() -> State {
        let saved: State;
        // SAFETY: reads this thread's FPCR.
        unsafe { asm!("mrs {}, fpcr", out(reg) saved, options(nostack, preserves_flags)) };
        restore(saved | FLUSH);
        saved
    }

    pub fn restore(state: State) {
        // SAFETY: writes this thread's FPCR with the value it had, and the flushing bit.
        unsafe { asm!("msr fpcr, {}", in(reg) state, options(nostack, preserves_flags)) };
    }
}

/// Where the unit's state is not known, nothing is flushed.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod fpu {
    pub type State = ();

    pub fn flush_denormals() -> State {}

    pub fn restore(_: State) {}
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
        let _no_denormals = NoDenormals::new();
        self.render_block(data, cb_num);
    }

    /// `render`, with the floating point unit as it is.
    fn render_block(&mut self, data: &mut [f32], cb_num: u64) {
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

        // Log peak level when there were MIDI events (and somebody is listening: the scan
        // is for the log alone, and this is the audio thread).
        if !self.midi_events.is_empty() && log::log_enabled!(log::Level::Debug) {
            let peak = data.iter().fold(0.0f32, |max, &s| max.max(s.abs()));
            log::debug!("Audio cb #{cb_num}: output peak = {peak:.6}");
        }
    }
}

/// The device name that stands for none: `--audio-device none` renders the engine
/// on a timer with no sound card, for CI, headless runs (`tang serve`) and tests.
pub const NULL_DEVICE: &str = "none";

/// The time one block of `buffer_size` frames lasts at `sample_rate`.
fn block_period(buffer_size: u32, sample_rate: u32) -> Duration {
    Duration::from_secs_f64(buffer_size as f64 / sample_rate as f64)
}

/// When the next block of the null backend is due, once the one due at `deadline`
/// has been rendered and the time is `now`. Blocks keep the cadence (a period after
/// the one before, so that waking up late does not make it drift), unless the
/// engine has fallen more than a period behind (a stall): then the blocks it
/// missed are not rendered in a burst, the cadence starts again from `now`.
fn next_deadline(deadline: Instant, period: Duration, now: Instant) -> Instant {
    let next = deadline + period;
    if now.saturating_duration_since(next) > period { now } else { next }
}

/// The null backend: a thread that renders a block every `period`, and throws
/// the samples away.
struct NullBackend {
    /// What renders, until `play` hands it to the thread.
    renderer: Option<Renderer>,
    frames: usize,
    period: Duration,
    running: Option<Running>,
}

struct Running {
    stop: Arc<AtomicBool>,
    /// Gives the renderer back when it ends, so that the plugins are dropped by
    /// whoever stops the engine, not by the rendering thread.
    thread: JoinHandle<Renderer>,
}

impl NullBackend {
    fn play(&mut self) -> anyhow::Result<()> {
        let Some(renderer) = self.renderer.take() else {
            return Ok(()); // already playing
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (frames, period) = (self.frames, self.period);
        let thread = std::thread::Builder::new().name("tang-null-audio".to_string()).spawn({
            let stop = stop.clone();
            move || render_on_a_timer(renderer, frames, period, &stop)
        })?;
        self.running = Some(Running { stop, thread });
        log::info!("Audio stream started");
        Ok(())
    }

    fn stop(mut self) {
        if let Some(running) = self.running.take() {
            running.stop.store(true, Ordering::Relaxed);
            match running.thread.join() {
                Ok(renderer) => drop(renderer),
                Err(_) => log::error!("The null audio thread panicked"),
            }
        }
        log::info!("Audio stream stopped");
    }
}

/// Render a block every `period` until `stop`, into a buffer nobody listens to.
fn render_on_a_timer(mut renderer: Renderer, frames: usize, period: Duration, stop: &AtomicBool) -> Renderer {
    let mut data = vec![0.0f32; frames * renderer.num_channels];
    let mut cb_num: u64 = 0;
    let mut deadline = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if cb_num == 0 {
            log::info!("Audio callback running (null backend, {frames} frames every {period:?})");
        }
        renderer.render(&mut data, cb_num);
        cb_num += 1;
        let now = Instant::now();
        deadline = next_deadline(deadline, period, now);
        if let Some(wait) = deadline.checked_duration_since(now) {
            std::thread::sleep(wait);
        }
    }
    renderer
}

pub struct AudioEngine {
    backend: Backend,
}

enum Backend {
    Cpal(cpal::Stream),
    /// Boxed: the renderer it holds is much bigger than a stream.
    Null(Box<NullBackend>),
}

impl AudioEngine {
    /// Stop the audio stream. Call this before dropping the plugin.
    pub fn stop(self) {
        let stream = match self.backend {
            Backend::Cpal(stream) => stream,
            Backend::Null(null) => return null.stop(),
        };
        // Pause the stream first so the callback stops being invoked
        if let Err(e) = stream.pause() {
            log::warn!("Failed to pause audio stream: {e}");
        }
        // Give the audio callback time to finish if it's mid-flight
        std::thread::sleep(std::time::Duration::from_millis(50));
        // Now drop the stream
        drop(stream);
        log::info!("Audio stream stopped");
    }

    /// Start playing the audio stream. Call after initial commands are queued.
    pub fn play(&mut self) -> anyhow::Result<()> {
        match &mut self.backend {
            Backend::Cpal(stream) => {
                stream.play()?;
                log::info!("Audio stream started");
                Ok(())
            }
            Backend::Null(null) => null.play(),
        }
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
        if device_name == Some(NULL_DEVICE) {
            log::info!("Using audio device: {NULL_DEVICE} (no audio output; the engine runs on a timer)");
            return Ok(AudioEngine {
                backend: Backend::Null(Box::new(NullBackend {
                    renderer: Some(Renderer::new(graph, midi_rx, buffer_size as usize)),
                    frames: buffer_size as usize,
                    period: block_period(buffer_size, sample_rate),
                    running: None,
                })),
            });
        }
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

        Ok(AudioEngine { backend: Backend::Cpal(stream) })
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
                keep: None,
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

    // -- Denormals --

    /// A note through a filter that is shut (its lowest cutoff) and let go at once: what it leaves
    /// in the filter takes a long time to run down, through every number a 32-bit float can hold.
    const SHUT_FILTER: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\nrelease = 0.002\n\
                               [[instrument.effect]]\nplugin = \"builtin:filter\"\n[instrument.effect.params]\ncutoff = 0.0\n";

    /// Render the note of `SHUT_FILTER` and the two seconds after it (the key is let go in the
    /// fifth block) with `render`, and count the samples that are denormal: not zero, and nearer
    /// to it than the smallest normal number.
    fn denormals_in_the_tail(render: impl Fn(&mut Renderer, &mut [f32], u64)) -> usize {
        let mut rig = rig(SHUT_FILTER, 256);
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        let mut data = vec![0.0f32; 256 * 2];
        let mut denormals = 0;
        for cb in 0..400 {
            if cb == 4 {
                rig.midi_tx.send((0, [0x80, 69, 0])).unwrap();
            }
            render(&mut rig.renderer, &mut data, cb);
            denormals += data.iter().filter(|s| s.is_subnormal()).count();
        }
        denormals
    }

    #[test]
    fn the_tail_of_a_shut_filter_goes_through_denormals_when_the_unit_is_left_as_it_is() {
        // The witness of the test below: without it, that test would pass for a tail that never had one.
        let denormals = denormals_in_the_tail(|renderer, data, cb| renderer.render_block(data, cb));
        assert!(denormals > 0, "the tail has denormal samples, which are slow to compute with on some processors");
    }

    #[test]
    fn a_rendered_block_has_no_denormals_in_it_even_in_the_tail_of_a_shut_filter() {
        let denormals = denormals_in_the_tail(|renderer, data, cb| renderer.render(data, cb));
        assert_eq!(denormals, 0, "denormals are flushed to zero while a block is rendered");
    }

    #[test]
    fn a_guard_makes_a_denormal_a_zero_for_as_long_as_it_lives_and_gives_the_unit_back() {
        // `black_box` at each product, so that the compiler does not make it itself or move it out of the guard.
        let times = |a: f32, b: f32| std::hint::black_box(a) * std::hint::black_box(b);
        let (smallest_normal, smallest_denormal) = (f32::MIN_POSITIVE, f32::from_bits(1));
        assert!(times(smallest_normal, 0.5).is_subnormal(), "as the unit is: half of the smallest normal number is a denormal");
        assert!(times(smallest_denormal, 1.0) > 0.0, "and a denormal is a number");
        {
            let _guard = NoDenormals::new();
            assert_eq!(times(smallest_normal, 0.5), 0.0, "a result that is a denormal is zero");
            assert_eq!(times(smallest_denormal, 1.0), 0.0, "and so is an operand");
        }
        assert!(times(smallest_normal, 0.5).is_subnormal(), "when the guard has gone, the unit is as it was");
        assert!(times(smallest_denormal, 1.0) > 0.0);
    }

    // -- The null backend: `--audio-device none` --

    use std::sync::atomic::AtomicUsize;

    use crate::telemetry::{self, Telemetry};

    /// An engine on the null backend, with the channels it listens on, the cells it
    /// publishes into and a sender for commands, built and not yet playing.
    struct NullRig {
        engine: AudioEngine,
        midi_tx: crossbeam_channel::Sender<MidiEvent>,
        cmd_tx: crossbeam_channel::Sender<crate::plugin::chain::GraphCommand>,
        telemetry: Arc<Telemetry>,
        _return_rx: Receiver<Box<dyn crate::plugin::Plugin>>,
    }

    fn null_rig(buffer_size: u32) -> NullRig {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let (midi_tx, midi_rx) = crossbeam_channel::bounded(1024);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);
        let telemetry = Arc::new(Telemetry::new());
        graph.set_telemetry(telemetry.clone());
        let engine = AudioEngine::build(graph, midi_rx, Some(NULL_DEVICE), 48_000, buffer_size).expect("the null backend needs no device");
        NullRig { engine, midi_tx, cmd_tx, telemetry, _return_rx: return_rx }
    }

    /// Wait up to five seconds for `done`.
    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "gave up waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn without_a_device_a_note_is_rendered_on_a_timer_and_measured() {
        let mut rig = null_rig(256);
        load_onto("[[instrument]]\nplugin = \"builtin:osc\"\n", &rig.cmd_tx);
        rig.engine.play().unwrap();
        assert_eq!(rig.telemetry.load(telemetry::MASTER.peak), 0.0, "silence before a note");
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        wait_for("the master's peak to rise", || rig.telemetry.load(telemetry::MASTER.peak) > 0.1);
        assert_eq!(rig.telemetry.count(telemetry::MASTER.clips), 0, "and it does not clip");
        rig.engine.stop();
    }

    #[test]
    fn a_lane_that_is_too_loud_clips_and_the_clips_are_counted() {
        // Three oscillators at four times the volume: far past full scale. Safe to
        // run: nothing is played.
        let mut rig = null_rig(256);
        let lane = "[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 4.0\n";
        load_onto(&lane.repeat(3), &rig.cmd_tx);
        rig.engine.play().unwrap();
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        wait_for("a clip to be counted", || rig.telemetry.count(telemetry::MASTER.clips) > 0);
        assert!(rig.telemetry.load(telemetry::MASTER.peak) > 1.0, "the peak is past full scale");
        let counted = rig.telemetry.count(telemetry::MASTER.clips);
        wait_for("another buffer to clip", || rig.telemetry.count(telemetry::MASTER.clips) > counted);
        rig.engine.stop();
    }

    /// An instrument that renders nothing, counts the blocks it is asked for, and
    /// says which thread dropped it.
    struct Probe {
        blocks: Arc<AtomicUsize>,
        /// How many frames the last block had.
        frames: Arc<AtomicUsize>,
        dropped_on: crossbeam_channel::Sender<std::thread::ThreadId>,
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = self.dropped_on.send(std::thread::current().id());
        }
    }

    impl crate::plugin::Plugin for Probe {
        fn name(&self) -> &str {
            "Probe"
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
        fn process(&mut self, _: &[(u64, [u8; 3])], _: &[&[f32]], out: &mut [&mut [f32]]) -> anyhow::Result<()> {
            self.frames.store(out[0].len(), Ordering::Relaxed);
            self.blocks.fetch_add(1, Ordering::Relaxed);
            Ok(())
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

    /// A rig with a probe in the first lane, the blocks it has been asked for, the
    /// frames of the last one, and where it reports being dropped.
    fn probed(buffer_size: u32) -> (NullRig, Arc<AtomicUsize>, Arc<AtomicUsize>, Receiver<std::thread::ThreadId>) {
        use crate::plugin::chain::GraphCommand;
        let rig = null_rig(buffer_size);
        let blocks = Arc::new(AtomicUsize::new(0));
        let frames = Arc::new(AtomicUsize::new(0));
        let (dropped_on, dropped) = crossbeam_channel::unbounded();
        rig.cmd_tx.send(GraphCommand::AddInstrument { range: None }).unwrap();
        rig.cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: Box::new(Probe { blocks: blocks.clone(), frames: frames.clone(), dropped_on }),
                inst_buf: vec![Vec::new(), Vec::new()],
                remapper: None,
                keep: None,
            })
            .unwrap();
        (rig, blocks, frames, dropped)
    }

    #[test]
    fn nothing_is_rendered_before_play_and_blocks_come_at_the_pace_of_the_buffer() {
        // 480 frames at 48 kHz: one block every 10 ms.
        let (mut rig, blocks, frames, _dropped) = probed(480);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(blocks.load(Ordering::Relaxed), 0, "not before play");
        rig.engine.play().unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let rendered = blocks.load(Ordering::Relaxed);
        // About 50, with room for a busy machine: not a free-running loop (thousands),
        // and not stalled.
        assert!((15..=60).contains(&rendered), "{rendered} blocks in half a second of 10 ms blocks");
        assert_eq!(frames.load(Ordering::Relaxed), 480, "each block is the buffer's size");
        rig.engine.stop();
    }

    #[test]
    fn stopping_ends_the_rendering_and_drops_the_plugins_on_the_thread_that_stops() {
        let (mut rig, blocks, _frames, dropped) = probed(128);
        rig.engine.play().unwrap();
        wait_for("blocks to be rendered", || blocks.load(Ordering::Relaxed) > 3);
        rig.engine.stop();
        let after_stop = blocks.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(blocks.load(Ordering::Relaxed), after_stop, "nothing is rendered after stop");
        let thread = dropped.recv_timeout(Duration::from_secs(5)).expect("the plugin was dropped");
        assert_eq!(thread, std::thread::current().id(), "here, not on the rendering thread");
    }

    #[test]
    fn an_engine_that_never_played_stops_cleanly() {
        let (rig, blocks, _frames, _dropped) = probed(128);
        rig.engine.stop();
        assert_eq!(blocks.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn play_twice_does_not_start_two_threads() {
        let (mut rig, blocks, _frames, _dropped) = probed(480);
        rig.engine.play().unwrap();
        rig.engine.play().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let rendered = blocks.load(Ordering::Relaxed);
        assert!(rendered <= 40, "one thread's worth of 10 ms blocks in 300 ms: {rendered}");
        rig.engine.stop();
    }

    #[test]
    fn a_block_is_as_long_as_the_buffer_in_time() {
        assert_eq!(block_period(480, 48_000), Duration::from_millis(10));
        assert_eq!(block_period(1024, 48_000), Duration::from_secs_f64(1024.0 / 48_000.0));
        assert_eq!(block_period(256, 44_100), Duration::from_secs_f64(256.0 / 44_100.0));
    }

    #[test]
    fn the_next_block_is_due_a_period_after_the_last_unless_the_engine_fell_far_behind() {
        let t0 = Instant::now();
        let period = Duration::from_millis(10);
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // On time, early or a little late: the cadence holds, so that sleeping does not make it drift.
        assert_eq!(next_deadline(at(0), period, at(2)), at(10));
        assert_eq!(next_deadline(at(0), period, at(0)), at(10));
        assert_eq!(next_deadline(at(0), period, at(15)), at(10), "5 ms behind the next one is still caught up by running early");
        assert_eq!(next_deadline(at(0), period, at(20)), at(10), "exactly one period behind: the next block is due now-ish");
        // More than a period behind (a stall): do not burst through the missed blocks.
        assert_eq!(next_deadline(at(0), period, at(35)), at(35));
    }
}
