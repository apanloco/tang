use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Receiver;

use crate::plugin::chain::PluginChain;

/// A MIDI event: (frame_offset, raw_bytes).
/// Standard MIDI messages are 1–3 bytes; we use a fixed array to avoid heap allocation.
pub type MidiEvent = (u64, [u8; 3]);

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

    pub fn start(
        mut chain: PluginChain,
        midi_rx: Receiver<MidiEvent>,
        device_name: Option<&str>,
        sample_rate: u32,
        buffer_size: u32,
    ) -> anyhow::Result<Self> {
        let host = cpal::default_host();

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

        let num_channels = chain.num_channels();

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
                    log::info!("Audio callback running (first call, buffer={})", data.len());
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

                if let Err(e) = chain.process(&midi_events, &mut channel_bufs) {
                    log::error!("Plugin chain process error: {e}");
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
                log::error!("Audio stream error: {err}");
            },
            None,
        )?;

        stream.play()?;
        log::info!("Audio stream started");

        Ok(AudioEngine { stream })
    }
}
