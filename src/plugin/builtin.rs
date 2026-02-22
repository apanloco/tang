use std::collections::HashMap;
use std::f32::consts::PI;

use super::{ParameterInfo, Plugin, PluginInfo, Preset};

/// A simple polyphonic sine oscillator, useful for testing audio/MIDI without
/// external plugins.
pub struct SineOscillator {
    sample_rate: f32,
    /// Active voices: MIDI note number → phase accumulator (0.0..1.0)
    voices: HashMap<u8, f32>,
}

impl SineOscillator {
    fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            voices: HashMap::new(),
        }
    }

    fn note_to_freq(note: u8) -> f32 {
        440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0)
    }
}

impl Plugin for SineOscillator {
    fn name(&self) -> &str {
        "Sine Oscillator"
    }

    fn is_instrument(&self) -> bool {
        true
    }

    fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    fn audio_output_count(&self) -> usize {
        2
    }

    fn audio_input_count(&self) -> usize {
        0
    }

    fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        _audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()> {
        let block_size = audio_out[0].len();

        // Clear output buffers
        for ch in audio_out.iter_mut() {
            for s in ch.iter_mut() {
                *s = 0.0;
            }
        }

        // Sort events by sample offset for correct per-sample processing
        let mut events: Vec<&(u64, [u8; 3])> = midi_events.iter().collect();
        events.sort_by_key(|(offset, _)| *offset);

        let mut event_idx = 0;

        for frame in 0..block_size {
            // Process MIDI events at this frame
            while event_idx < events.len() && events[event_idx].0 as usize <= frame {
                let [status, note, velocity] = events[event_idx].1;
                let msg_type = status & 0xF0;
                match msg_type {
                    0x90 if velocity > 0 => {
                        self.voices.insert(note, 0.0);
                    }
                    0x80 | 0x90 => {
                        self.voices.remove(&note);
                    }
                    _ => {}
                }
                event_idx += 1;
            }

            // Render all active voices
            let mut sample = 0.0_f32;
            for (&note, phase) in self.voices.iter_mut() {
                let freq = Self::note_to_freq(note);
                sample += (2.0 * PI * *phase).sin();
                *phase += freq / self.sample_rate;
                if *phase >= 1.0 {
                    *phase -= 1.0;
                }
            }

            // Clamp to avoid blowup with many voices
            sample = sample.clamp(-1.0, 1.0);

            // Mono signal to both channels
            audio_out[0][frame] = sample;
            if audio_out.len() > 1 {
                audio_out[1][frame] = sample;
            }
        }

        Ok(())
    }

    fn parameters(&self) -> Vec<ParameterInfo> {
        Vec::new()
    }

    fn get_parameter(&mut self, _index: u32) -> Option<f32> {
        None
    }

    fn set_parameter(&mut self, index: u32, _value: f32) -> anyhow::Result<()> {
        anyhow::bail!("no parameter with index {index}")
    }

    fn presets(&self) -> Vec<Preset> {
        Vec::new()
    }

    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        anyhow::bail!("no preset with id {id:?}")
    }
}

/// Load a built-in plugin by source string (e.g. `"builtin:sine"`).
pub fn load(
    source: &str,
    sample_rate: f32,
    _max_block_size: usize,
) -> anyhow::Result<Box<dyn Plugin>> {
    let name = source.strip_prefix("builtin:").unwrap_or(source);
    match name {
        "sine" => Ok(Box::new(SineOscillator::new(sample_rate))),
        _ => anyhow::bail!(
            "Unknown built-in plugin: {name:?}\n\
             Available built-ins: sine\n\
             Usage: builtin:sine"
        ),
    }
}

/// Return enumeration info for all built-in plugins.
pub fn enumerate_plugins() -> Vec<PluginInfo> {
    vec![PluginInfo {
        name: "Sine Oscillator".into(),
        id: "builtin:sine".into(),
        is_instrument: true,
        param_count: 0,
        preset_count: 0,
        path: "(built-in)".into(),
    }]
}
