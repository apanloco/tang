use std::collections::HashMap;
use std::f32::consts::PI;

use super::{ParameterInfo, Plugin, PluginInfo, Preset};

#[derive(Copy, Clone)]
enum Waveform {
    Sine,
    Triangle,
    Square,
}

impl Waveform {
    fn name(self) -> &'static str {
        match self {
            Waveform::Sine => "Sine Oscillator",
            Waveform::Triangle => "Triangle Oscillator",
            Waveform::Square => "Square Oscillator",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Waveform::Sine => "builtin:sine",
            Waveform::Triangle => "builtin:triangle",
            Waveform::Square => "builtin:square",
        }
    }

    /// Render one sample for a phase in [0.0, 1.0).
    fn sample(self, phase: f32) -> f32 {
        match self {
            Waveform::Sine => (2.0 * PI * phase).sin(),
            // Triangle in [-1, 1]: rises 0→1 over [0, 0.25), falls 1→-1 over
            // [0.25, 0.75), rises -1→0 over [0.75, 1.0).
            Waveform::Triangle => 4.0 * (phase - (phase + 0.5).floor()).abs() - 1.0,
            // Naïve square (no anti-aliasing): +1 for first half, -1 for second.
            Waveform::Square => if phase < 0.5 { 1.0 } else { -1.0 },
        }
    }
}

/// A simple polyphonic oscillator, useful for testing audio/MIDI without
/// external plugins.
pub struct Oscillator {
    sample_rate: f32,
    /// Active voices: MIDI note number → phase accumulator (0.0..1.0)
    voices: HashMap<u8, f32>,
    /// Detune in semitones, applied to all voices. Modulator-friendly.
    detune: f32,
    waveform: Waveform,
}

const DETUNE_MIN: f32 = -2.0;
const DETUNE_MAX: f32 = 2.0;

impl Oscillator {
    fn new(sample_rate: f32, waveform: Waveform) -> Self {
        Self {
            sample_rate,
            voices: HashMap::new(),
            detune: 0.0,
            waveform,
        }
    }

    fn note_to_freq(note: u8, detune_semitones: f32) -> f32 {
        440.0 * 2.0_f32.powf((note as f32 - 69.0 + detune_semitones) / 12.0)
    }
}

impl Plugin for Oscillator {
    fn name(&self) -> &str {
        self.waveform.name()
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

        let mut event_idx = 0;

        for frame in 0..block_size {
            // Process MIDI events at this frame
            while event_idx < midi_events.len() && midi_events[event_idx].0 as usize <= frame {
                let [status, note, velocity] = midi_events[event_idx].1;
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

            // Render all active voices with fixed per-voice gain.
            // Using a fixed gain avoids amplitude discontinuities when
            // voices are added/removed (dividing by voice count causes
            // existing voices to suddenly change volume).
            const VOICE_GAIN: f32 = 0.25;
            let mut sample = 0.0_f32;
            for (&note, phase) in self.voices.iter_mut() {
                let freq = Self::note_to_freq(note, self.detune);
                sample += self.waveform.sample(*phase) * VOICE_GAIN;
                *phase += freq / self.sample_rate;
                if *phase >= 1.0 {
                    *phase -= 1.0;
                }
            }

            // Mono signal to both channels
            audio_out[0][frame] = sample;
            if audio_out.len() > 1 {
                audio_out[1][frame] = sample;
            }
        }

        Ok(())
    }

    fn parameters(&self) -> Vec<ParameterInfo> {
        vec![ParameterInfo {
            index: 0,
            name: "detune".into(),
            min: DETUNE_MIN,
            max: DETUNE_MAX,
            default: 0.0,
        }]
    }

    fn get_parameter(&mut self, index: u32) -> Option<f32> {
        match index {
            0 => Some(self.detune),
            _ => None,
        }
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        match index {
            0 => {
                self.detune = value.clamp(DETUNE_MIN, DETUNE_MAX);
                Ok(())
            }
            _ => anyhow::bail!("no parameter with index {index}"),
        }
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
        "sine" => Ok(Box::new(Oscillator::new(sample_rate, Waveform::Sine))),
        "triangle" => Ok(Box::new(Oscillator::new(sample_rate, Waveform::Triangle))),
        "square" => Ok(Box::new(Oscillator::new(sample_rate, Waveform::Square))),
        _ => anyhow::bail!(
            "Unknown built-in plugin: {name:?}\n\
             Available built-ins: sine, triangle, square\n\
             Usage: builtin:sine"
        ),
    }
}

/// Return enumeration info for all built-in plugins.
pub fn enumerate_plugins() -> Vec<PluginInfo> {
    [Waveform::Sine, Waveform::Triangle, Waveform::Square]
        .iter()
        .map(|w| PluginInfo {
            name: w.name().into(),
            id: w.id().into(),
            is_instrument: true,
            param_count: 1,
            preset_count: 0,
            path: "(built-in)".into(),
            scan_ms: 0,
        })
        .collect()
}
