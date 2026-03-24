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
        // Set PIPEWIRE_QUANTUM before connecting so PipeWire allocates the
        // requested buffer size for this JACK client.
        let quantum = format!("{buffer_size}/{sample_rate}");
        // SAFETY: called once at startup before other threads read env vars.
        unsafe { std::env::set_var("PIPEWIRE_QUANTUM", &quantum) };
        log::info!("Set PIPEWIRE_QUANTUM={quantum}");

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
        unsafe { std::env::remove_var("PIPEWIRE_QUANTUM") };
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
        mut graph: AudioGraph,
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

        // Pre-allocate buffers that live in the closure and are reused every callback
        let mut midi_events: Vec<MidiEvent> = Vec::with_capacity(64);
        let mut channel_bufs: Vec<Vec<f32>> = (0..num_channels)
            .map(|_| vec![0.0f32; buffer_size as usize])
            .collect();

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

                // Drain all pending MIDI events (reuse pre-allocated vec)
                midi_events.clear();
                while let Ok(event) = midi_rx.try_recv() {
                    midi_events.push(event);
                }

                if !midi_events.is_empty() {
                    log::debug!(
                        "Audio cb #{cb_num}: processing {} MIDI event(s) into {} frames",
                        midi_events.len(),
                        data.len() / num_channels
                    );
                }

                let frames = data.len() / num_channels;

                // Resize and zero pre-allocated per-channel buffers
                for buf in channel_bufs.iter_mut() {
                    buf.resize(frames, 0.0);
                    buf.fill(0.0);
                }

                if let Err(e) = graph.process(&midi_events, &mut channel_bufs) {
                    log::error!("Audio graph process error: {e}");
                    data.fill(0.0);
                    return;
                }

                // Interleave back into cpal output buffer
                for frame in 0..frames {
                    for ch in 0..num_channels {
                        data[frame * num_channels + ch] = channel_bufs[ch][frame];
                    }
                }

                // Log peak level when there were MIDI events
                if !midi_events.is_empty() {
                    let peak = data.iter().fold(0.0f32, |max, &s| max.max(s.abs()));
                    log::debug!("Audio cb #{cb_num}: output peak = {peak:.6}");
                }
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
