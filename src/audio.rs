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
///
/// What it does not do is end a tail. A state that moves by a product of its own
/// distance to something (`s += g * (x - s)`) stops where that product is flushed,
/// and stays there: the built-in filter's output was a constant of 2e-38 to 5e-35 (by its
/// type and cutoff) for as long as it was watched, and it lets go of its states under
/// `TAIL_FLOOR` now.
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

    /// A note through a delay of a millisecond with the most feedback it has, let go at once: the repeats run down, as the tail of a note does,
    /// through every number a 32-bit float can hold.
    const FAST_ECHO: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\nrelease = 0.002\n\
                             [[instrument.effect]]\nplugin = \"builtin:delay\"\nmix = 1.0\n[instrument.effect.params]\ntime = 0.001\nfeedback = 0.95\n";

    /// Render the note of `FAST_ECHO` and the two seconds after it (the key is let go in the
    /// fifth block) with `render`, and count the samples that are denormal: not zero, and nearer
    /// to it than the smallest normal number.
    fn denormals_in_the_tail(render: impl Fn(&mut Renderer, &mut [f32], u64)) -> usize {
        let mut rig = rig(FAST_ECHO, 256);
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
    fn the_tail_of_a_fast_echo_goes_through_denormals_when_the_unit_is_left_as_it_is() {
        // The witness of the test below: without it, that test would pass for a tail that never had one.
        let denormals = denormals_in_the_tail(|renderer, data, cb| renderer.render_block(data, cb));
        assert!(denormals > 0, "the tail has denormal samples, which are slow to compute with on some processors");
    }

    #[test]
    fn a_rendered_block_has_no_denormals_in_it_even_in_the_tail_of_a_fast_echo() {
        let denormals = denormals_in_the_tail(|renderer, data, cb| renderer.render(data, cb));
        assert_eq!(denormals, 0, "denormals are flushed to zero while a block is rendered");
    }

    // -- Tails that end --

    /// The loudest sample of each block of a note of `session` that is let go in block `hold`, from the release on, for `seconds`.
    fn tail_peaks(session: &str, hold: usize, seconds: f32) -> Vec<f32> {
        let mut rig = rig(session, 256);
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        let mut data = vec![0.0f32; 256 * 2];
        let mut peaks = Vec::new();
        for cb in 0..hold + (seconds * 48_000.0 / 256.0) as usize {
            if cb == hold {
                rig.midi_tx.send((0, [0x80, 69, 0])).unwrap();
            }
            rig.renderer.render(&mut data, cb as u64);
            if cb >= hold {
                peaks.push(data.iter().fold(0.0f32, |peak, s| peak.max(s.abs())));
            }
        }
        peaks
    }

    /// A saw through the built-in filter of `kind` (its `type` parameter) at `cutoff`, with the resonance the tracer has.
    fn saw_through_a_filter(kind: usize, cutoff: f32) -> String {
        format!(
            "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\nwaveform = 3.0\n\
             [[instrument.effect]]\nplugin = \"builtin:filter\"\n[instrument.effect.params]\ncutoff = {cutoff}\nresonance = 2.0\ntype = {kind}.0\n"
        )
    }

    #[test]
    fn a_note_through_a_filter_of_each_type_and_cutoff_ends_in_silence_after_it_is_let_go() {
        let mut never = Vec::new();
        for kind in 0..7 {
            for cutoff in [0.0, 0.1, 0.5, 0.9] {
                if tail_peaks(&saw_through_a_filter(kind, cutoff), 100, 8.0).last() != Some(&0.0) {
                    never.push(format!("type {kind} at cutoff {cutoff}"));
                }
            }
        }
        assert!(never.is_empty(), "still not silent 8 seconds after the note: {never:?}");
    }

    /// The tracer's filter: a low-pass at 0.5 that an LFO of 0.5 Hz sweeps by 0.4 of its range, so that the cutoff it has when the key goes up is anywhere from 0.1 to 0.9.
    const SWEPT_FILTER: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\nwaveform = 3.0\n\
        [[instrument.effect]]\nplugin = \"builtin:filter\"\n[instrument.effect.params]\ncutoff = 0.5\nresonance = 2.0\n\
        [[instrument.modulator]]\ntype = \"lfo\"\nrate = 0.5\n[[instrument.modulator.target]]\nslot = 1\nparam = \"cutoff\"\ndepth = 0.4\n";

    #[test]
    fn a_note_through_a_filter_that_an_lfo_sweeps_ends_in_silence_wherever_the_sweep_is_when_it_is_let_go() {
        // The LFO's turn takes two seconds, which is 375 blocks: the releases are spread over it.
        let never: Vec<usize> = (60..60 + 375).step_by(25).filter(|&hold| tail_peaks(SWEPT_FILTER, hold, 8.0).last() != Some(&0.0)).collect();
        assert!(never.is_empty(), "still not silent 8 seconds after the note, let go in block: {never:?}");
    }

    #[test]
    fn the_last_sound_in_the_tail_of_a_note_through_a_shut_filter_is_under_1e_15() {
        // The tail that runs down the slowest: what is cut off when the filter lets go of it is not heard.
        let peaks = tail_peaks(&saw_through_a_filter(0, 0.0), 100, 8.0);
        let last_sound = peaks.iter().rev().find(|&&peak| peak != 0.0).copied().expect("a note has a sound");
        assert!(last_sound < 1e-15, "{last_sound:e}");
        assert_eq!(peaks.last(), Some(&0.0), "and then it is silent");
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

    // -- The example sessions --

    /// The example sessions of this repository that show what the built-in plugins do (the others name a plugin that has to be installed, or are somebody's saved sound).
    const SOUND_EXAMPLES: [&str; 11] = ["supersaw.toml", "wind.toml", "bell.toml", "arp.toml", "pwm.toml", "epiano.toml", "sync-lead.toml", "sub-bass.toml", "fold.toml", "wobble.toml", "gate.toml"];

    fn example(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples").join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// The left side of `session` as it sounds for `seconds` when the keys `keys` go down after `from` seconds and up `held` seconds after that (at the velocity that the keys of
    /// the page and of the terminal have, 100).
    fn play(session: &str, keys: &[u8], from: f32, held: f32, seconds: f32) -> Vec<f32> {
        play_hit(session, keys, 100, from, held, seconds)
    }

    /// The same, with the keys hit at `velocity`.
    fn play_hit(session: &str, keys: &[u8], velocity: u8, from: f32, held: f32, seconds: f32) -> Vec<f32> {
        let mut rig = rig(session, 256);
        let blocks = (seconds * 48_000.0 / 256.0) as usize;
        let press_at = (from * 48_000.0 / 256.0) as usize;
        let release_at = ((from + held) * 48_000.0 / 256.0) as usize;
        let mut data = vec![0.0f32; 256 * 2];
        let mut left = Vec::with_capacity(blocks * 256);
        for cb in 0..blocks {
            if cb == press_at {
                for &key in keys {
                    rig.midi_tx.send((0, [0x90, key, velocity])).unwrap();
                }
            }
            if cb == release_at {
                for &key in keys {
                    rig.midi_tx.send((0, [0x80, key, 0])).unwrap();
                }
            }
            rig.renderer.render(&mut data, cb as u64);
            left.extend(data.chunks(2).map(|frame| frame[0]));
        }
        left
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn the_example_sounds_load_and_play_a_chord_that_is_heard_and_is_not_loud() {
        for name in SOUND_EXAMPLES {
            let out = play(&example(name), &[48, 52, 55], 0.0, 5.0, 8.0);
            let peak = out.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            assert!(out.iter().all(|x| x.is_finite()), "{name}");
            assert!(peak > 0.05, "{name} is heard: its loudest sample is {peak}");
            assert!(peak < 0.6, "{name} is not loud: its loudest sample is {peak}");
        }
    }

    /// The frequency of the periodic signal in `window`, from its autocorrelation: the first of the lags between 40 and 1000 Hz that is nearly as good as the best (the multiples of a
    /// period are as good as the period itself), brought between two samples.
    fn pitch_of(window: &[f32], sample_rate: f32) -> f32 {
        let (shortest, longest) = ((sample_rate / 1000.0) as usize, (sample_rate / 40.0) as usize);
        let correlation: Vec<f32> = (shortest..=longest).map(|lag| window[..window.len() - longest].iter().zip(&window[lag..]).map(|(a, b)| a * b).sum()).collect();
        let best = correlation.iter().cloned().fold(f32::MIN, f32::max);
        let first = (1..correlation.len() - 1)
            .find(|&i| correlation[i] > 0.9 * best && correlation[i] >= correlation[i - 1] && correlation[i] >= correlation[i + 1])
            .expect("a period");
        let (a, b, c) = (correlation[first - 1], correlation[first], correlation[first + 1]);
        let offset = 0.5 * (a - c) / (a - 2.0 * b + c);
        sample_rate / ((shortest + first) as f32 + offset)
    }

    #[test]
    fn the_arp_example_goes_through_the_notes_of_a_minor_seventh_in_tune_when_a_key_is_held() {
        // C3 held, after a pause that is not a whole number of steps: the steps are the semitones 0 3 7 10 12 10 7 3 of a chord, six a second, and the key starts them from the first.
        let out = play(&example("arp.toml"), &[48], 0.4, 3.0, 3.6);
        let c3 = 440.0 * 2.0f32.powf(-21.0 / 12.0);
        for (step, semitones) in [0, 3, 7, 10, 12, 10, 7, 3, 0, 3, 7, 10].into_iter().enumerate() {
            let start = ((0.4 + step as f32 / 6.0 + 0.03) * 48_000.0) as usize;
            let heard = pitch_of(&out[start..start + 4800], 48_000.0);
            let wanted = c3 * 2.0f32.powf(semitones as f32 / 12.0);
            let cents = 1200.0 * (heard / wanted).log2();
            assert!(cents.abs() < 5.0, "step {step}: {heard} Hz for {wanted} Hz, {cents} cents off");
        }
    }

    /// `session` with its tempo said to be `bpm` beats a minute: its own `bpm` line, if it has one, is replaced (a key of the top level goes before every table).
    fn at_tempo(session: &str, bpm: f32) -> String {
        let rest: Vec<&str> = session.lines().filter(|line| !line.trim_start().starts_with("bpm")).collect();
        format!("bpm = {bpm:?}\n{}", rest.join("\n"))
    }

    #[test]
    fn the_arp_example_goes_by_the_tempo_and_not_by_its_own_rate() {
        // Its steps are an eighth-note triplet each: six a second at 120 beats a minute (which is the rate it keeps), three at 60 and nine at 180.
        let c3 = 440.0 * 2.0f32.powf(-21.0 / 12.0);
        for (bpm, per_second) in [(60.0f32, 3.0f32), (180.0, 9.0)] {
            let out = play(&at_tempo(&example("arp.toml"), bpm), &[48], 0.4, 3.0, 3.4);
            for (step, semitones) in [0, 3, 7, 10, 12, 10, 7, 3].into_iter().enumerate() {
                // (A window of 75 ms, a tenth of the way into the step: it is over before the step is, at nine a second too.)
                let start = ((0.4 + (step as f32 + 0.1) / per_second) * 48_000.0) as usize;
                let heard = pitch_of(&out[start..start + 3600], 48_000.0);
                let wanted = c3 * 2.0f32.powf(semitones as f32 / 12.0);
                let cents = 1200.0 * (heard / wanted).log2();
                assert!(cents.abs() < 5.0, "at {bpm} beats a minute, step {step}: {heard} Hz for {wanted} Hz, {cents} cents off");
            }
        }
    }

    /// The period of the signal in `series`, in its samples: from its autocorrelation with the mean taken out, the first peak between the lags `shortest` and `longest` that is nearly
    /// as good as the best (the multiples of a period are as good as the period), brought between two samples.
    fn period_in(series: &[f32], shortest: usize, longest: usize) -> f32 {
        let mean = series.iter().sum::<f32>() / series.len() as f32;
        let x: Vec<f32> = series.iter().map(|v| v - mean).collect();
        let correlation: Vec<f32> = (shortest..=longest).map(|lag| x[..x.len() - longest].iter().zip(&x[lag..]).map(|(a, b)| a * b).sum()).collect();
        let best = correlation.iter().cloned().fold(f32::MIN, f32::max);
        let first = (1..correlation.len() - 1)
            .find(|&i| correlation[i] > 0.8 * best && correlation[i] >= correlation[i - 1] && correlation[i] >= correlation[i + 1])
            .expect("a period");
        let (a, b, c) = (correlation[first - 1], correlation[first], correlation[first + 1]);
        (shortest + first) as f32 + 0.5 * (a - c) / (a - 2.0 * b + c)
    }

    #[test]
    fn the_wobble_example_goes_round_in_an_eighth_note_of_its_tempo_and_in_twice_as_long_at_half_of_it() {
        // How long the filter takes to open and shut again, from the brightness of the sound (how much of it is in its changes) in a window of 40 ms every 5 ms, from a second in.
        let seconds_a_cycle = |bpm: f32| {
            let out = play(&at_tempo(&example("wobble.toml"), bpm), &[48], 0.0, 5.0, 4.5);
            let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
            let series: Vec<f32> = (0..).map(|k| 48_000 + k * 240).take_while(|from| from + 1920 <= out.len()).map(|from| brightness(&out[from..from + 1920])).collect();
            period_in(&series, 20, 250) * 240.0 / 48_000.0
        };
        let (fast, slow) = (seconds_a_cycle(140.0), seconds_a_cycle(70.0));
        // An eighth note is half a beat: 0.2143 s at 140 beats a minute and 0.4286 s at 70.
        assert!((fast - 0.5 * 60.0 / 140.0).abs() < 0.03 * fast, "a cycle takes {fast} s at 140 beats a minute");
        assert!((slow / fast - 2.0).abs() < 0.04, "and {slow} s at 70: {} times as long", slow / fast);
    }

    #[test]
    fn the_gate_example_opens_and_shuts_on_the_sixteenths_of_its_tempo_and_is_twice_as_slow_at_half_of_it() {
        // Its sixteen steps, as the file says them: a step that is heard, and one that is not.
        let heard = [true, false, true, true, false, true, false, true, true, false, false, true, false, true, true, false];
        for (bpm, step_seconds) in [(120.0f32, 0.125f32), (60.0, 0.25)] {
            let out = play(&at_tempo(&example("gate.toml"), bpm), &[48], 0.4, 5.0, 4.4);
            // (The middle of each step: from three tenths of it to seven, which a block's lateness and the 5 ms that the volume is gone to over do not reach.)
            let level = |step: usize| {
                let from = 0.4 + step as f32 * step_seconds;
                rms(&out[((from + 0.3 * step_seconds) * 48_000.0) as usize..((from + 0.7 * step_seconds) * 48_000.0) as usize])
            };
            let loudest = (0..16).map(level).fold(0.0f32, f32::max);
            assert!(loudest > 0.02, "it is heard at {bpm} beats a minute: {loudest}");
            for (step, open) in heard.into_iter().enumerate() {
                // (The three oscillators of a note beat against each other, so the steps that are heard are not all as loud: from a half to the loudest, measured.)
                if open {
                    assert!(level(step) > 0.3 * loudest, "step {step} is heard at {bpm} beats a minute: {} of {loudest}", level(step));
                } else {
                    assert!(level(step) < 0.05 * loudest, "step {step} is not heard at {bpm} beats a minute: {} of {loudest}", level(step));
                }
            }
        }
    }

    #[test]
    fn the_bell_example_starts_bright_and_dies_away_to_a_plain_tone() {
        let out = play(&example("bell.toml"), &[72], 0.0, 3.0, 4.0);
        let window = |from: f32| &out[(from * 48_000.0) as usize..((from + 0.1) * 48_000.0) as usize];
        // How much of a signal is in its changes: more for a signal with high partials than for one with a low tone.
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        let (start, later) = (brightness(window(0.02)), brightness(window(1.8)));
        assert!(start > 4.0 * later, "the first moments are brighter than the later ones: {start} and {later}");
        assert!(rms(window(1.8)) < 0.6 * rms(window(0.02)), "and louder");
    }

    #[test]
    fn the_wind_example_swells_in_over_the_attack_and_is_still_there_when_the_key_goes_up() {
        let out = play(&example("wind.toml"), &[48], 0.0, 5.0, 8.0);
        let window = |from: f32| &out[(from * 48_000.0) as usize..((from + 0.2) * 48_000.0) as usize];
        // An attack of two and a half seconds: the first moments are far quieter than the held sound, and the release (four seconds) is far from over a moment after the key goes up.
        assert!(rms(window(0.1)) < 0.2 * rms(window(4.5)), "{} and {}", rms(window(0.1)), rms(window(4.5)));
        assert!(rms(window(5.2)) > 0.5 * rms(window(4.5)), "{} and {}", rms(window(5.2)), rms(window(4.5)));
    }

    #[test]
    fn the_examples_have_the_delay_presets_that_they_name() {
        // (A preset that the plugin does not have is only logged by the loader, so a misspelled name would load and sound like no preset at all.)
        for (name, effect, preset, time) in [("pwm.toml", 1, "Slapback", 0.09), ("epiano.toml", 0, "Chorus", 0.02)] {
            let run = crate::app::testutil::load(&example(name));
            let delay = &run.model.instruments[0].effects[effect];
            let heard = delay.params.iter().find(|p| p.name == "time").unwrap().value;
            assert_eq!((delay.name.as_str(), heard), ("Delay", time), "{name}: the time of {preset}, and not the delay's own");
        }
    }

    #[test]
    fn the_sync_lead_example_opens_its_ratio_over_the_start_of_a_note_and_has_the_pitch_of_the_key_when_it_settles() {
        let out = play(&example("sync-lead.toml"), &[60], 0.0, 3.0, 4.0);
        let window = |from: f32| &out[(from * 48_000.0) as usize..((from + 0.1) * 48_000.0) as usize];
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        // The ratio goes from 1 (a plain saw) to 6 over the 0.4 s of the envelope's attack and settles at 2.75 (1.2 s later): the sound is brightest at the top of the sweep.
        let (start, top, settled) = (brightness(window(0.02)), brightness(window(0.4)), brightness(window(2.4)));
        assert!(top > 1.4 * start && top > 1.25 * settled, "{start} at the start, {top} at the top and {settled} when it has settled");
        // A saw that goes round at 2.75 times the pitch of the key would be heard at 719 Hz: it is the sync that keeps it at the key's, C4. (The pitch is not measured while the ratio moves: a wave that changes
        // from one cycle to the next has an autocorrelation whose peak is moved, by 26 cents at the start of the note, and at the top of the sweep is another one, at 314 Hz, 6/5 of the key's.)
        let c4 = 440.0 * 2.0f32.powf(-9.0 / 12.0);
        let cents = 1200.0 * (pitch_of(window(2.4), 48_000.0) / c4).log2();
        assert!(cents.abs() < 5.0, "{cents} cents from C4");
    }

    #[test]
    fn the_sub_bass_example_sounds_an_octave_under_the_key_and_plucks() {
        let out = play(&example("sub-bass.toml"), &[48], 0.0, 2.0, 3.0);
        let window = |from: f32| &out[(from * 48_000.0) as usize..((from + 0.1) * 48_000.0) as usize];
        // C3 is pressed. The sine under it is so much of the sound that it goes round once for two cycles of the saw: what is heard is C2.
        let c2 = 440.0 * 2.0f32.powf(-33.0 / 12.0);
        for from in [0.5f32, 1.0, 1.8] {
            let cents = 1200.0 * (pitch_of(window(from), 48_000.0) / c2).log2();
            assert!(cents.abs() < 5.0, "{cents} cents from C2 at {from} s");
        }
        // The filter is opened by an envelope at the start of the note: brighter then than later.
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        let (start, later) = (brightness(window(0.02)), brightness(window(1.8)));
        assert!(start > 1.3 * later, "{start} and {later}");
    }

    #[test]
    fn the_fold_example_sweeps_from_a_soft_sound_to_a_bright_one_with_the_pitch_of_the_key() {
        // The LFO goes round in 5 s (0.2 Hz), so that it is at its top at 1.25 s and at its bottom at 3.75 s: the drive is at 0.8 and at 0.2 (a gain of 27 and of 2.3 at the fold).
        let out = play(&example("fold.toml"), &[48], 0.0, 9.0, 5.0);
        let window = |from: f32| &out[(from * 48_000.0) as usize..((from + 0.1) * 48_000.0) as usize];
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        let (folded, soft) = (brightness(window(1.25)), brightness(window(3.75)));
        assert!(folded > 4.0 * soft, "{folded} at the top of the sweep and {soft} at the bottom");
        // A fold changes the colour of a wave and not its period. (Not measured at the very top: there the first peak of the autocorrelation that is nearly the best is at a lag of
        // the seventh harmonic, and it says 915 Hz for a key of 131.)
        let c3 = 440.0 * 2.0f32.powf(-21.0 / 12.0);
        for from in [1.0f32, 1.5, 2.0] {
            let cents = 1200.0 * (pitch_of(window(from), 48_000.0) / c3).log2();
            assert!(cents.abs() < 5.0, "{cents} cents from C3 at {from} s");
        }
    }

    #[test]
    fn the_epiano_example_is_softer_when_a_key_is_hit_softer_and_is_the_sound_it_was_at_the_velocity_of_the_computer_keys() {
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        let hit = |velocity: u8| play_hit(&example("epiano.toml"), &[60], velocity, 0.0, 3.0, 4.0);
        let window = |out: &[f32], from: f32| out[(from * 48_000.0) as usize..((from + 0.1) * 48_000.0) as usize].to_vec();
        let (soft, usual, hard) = (hit(40), hit(100), hit(120));
        // Half of the level is the velocity: a key hit at 40 is 0.68 of one at 120 (the measure is 0.675), and it is no different in its brightness (the velocity is the level's only).
        let ratio = rms(&window(&soft, 0.02)) / rms(&window(&hard, 0.02));
        assert!(ratio > 0.6 && ratio < 0.75, "{ratio}");
        assert!((brightness(&window(&soft, 0.02)) / brightness(&window(&hard, 0.02)) - 1.0).abs() < 0.03);
        // At 100, which the keys of the computer and of the page hit at, it is the sound that the file made before it had a velocity (these are its numbers then, in four places of a note of 4 s).
        for (from, rms_then, brightness_then) in [(0.02f32, 0.0396f32, 0.080f32), (0.3, 0.0321, 0.069), (1.0, 0.0127, 0.059), (2.0, 0.0059, 0.061)] {
            let (r, b) = (rms(&window(&usual, from)), brightness(&window(&usual, from)));
            assert!((r / rms_then - 1.0).abs() < 0.03, "at {from} s: {r} for {rms_then}");
            assert!((b / brightness_then - 1.0).abs() < 0.03, "at {from} s: {b} for {brightness_then}");
        }
    }

    #[test]
    fn the_epiano_example_is_brightest_at_the_start_of_a_note_and_mellows_as_it_rings() {
        let out = play(&example("epiano.toml"), &[60], 0.0, 3.0, 4.0);
        let window = |from: f32| &out[(from * 48_000.0) as usize..((from + 0.1) * 48_000.0) as usize];
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        let (start, later) = (brightness(window(0.02)), brightness(window(1.0)));
        assert!(start > 1.2 * later, "{start} and {later}");
    }
}
