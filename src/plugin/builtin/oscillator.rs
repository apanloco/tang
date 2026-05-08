use std::collections::HashMap;
use std::f32::consts::PI;

use crate::plugin::{ParameterInfo, Plugin, Preset};

#[derive(Copy, Clone)]
pub enum Waveform {
    Sine,
    Triangle,
    Square,
}

impl Waveform {
    pub fn name(self) -> &'static str {
        match self {
            Waveform::Sine => "Sine Oscillator",
            Waveform::Triangle => "Triangle Oscillator",
            Waveform::Square => "Square Oscillator",
        }
    }

    pub fn id(self) -> &'static str {
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
    voices: HashMap<u8, Voice>,
    /// Detune in semitones, applied to all voices. Modulator-friendly.
    detune: f32,
    waveform: Waveform,
}

#[derive(Copy, Clone)]
enum VoiceState {
    Attack,
    Sustain,
    Release,
}

struct Voice {
    phase: f32,
    amp: f32,
    state: VoiceState,
}

const DETUNE_MIN: f32 = -2.0;
const DETUNE_MAX: f32 = 2.0;

// Short attack/release ramps to avoid clicks on note start/stop. Tight enough
// to feel instantaneous; long enough to smooth the discontinuity.
const ATTACK_SEC: f32 = 0.004;
const RELEASE_SEC: f32 = 0.012;

impl Oscillator {
    pub fn new(sample_rate: f32, waveform: Waveform) -> Self {
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
        let attack_inc = 1.0 / (ATTACK_SEC * self.sample_rate);
        let release_inc = 1.0 / (RELEASE_SEC * self.sample_rate);

        for frame in 0..block_size {
            // Process MIDI events at this frame
            while event_idx < midi_events.len() && midi_events[event_idx].0 as usize <= frame {
                let [status, note, velocity] = midi_events[event_idx].1;
                let msg_type = status & 0xF0;
                match msg_type {
                    0x90 if velocity > 0 => {
                        // Note-on: attack from current amp (0 if new voice, or
                        // the partially-released level on retrigger — avoids a
                        // click when re-hitting a still-decaying note).
                        self.voices
                            .entry(note)
                            .and_modify(|v| v.state = VoiceState::Attack)
                            .or_insert(Voice {
                                phase: 0.0,
                                amp: 0.0,
                                state: VoiceState::Attack,
                            });
                    }
                    0x80 | 0x90 => {
                        if let Some(v) = self.voices.get_mut(&note) {
                            v.state = VoiceState::Release;
                        }
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
            for (&note, voice) in self.voices.iter_mut() {
                match voice.state {
                    VoiceState::Attack => {
                        voice.amp += attack_inc;
                        if voice.amp >= 1.0 {
                            voice.amp = 1.0;
                            voice.state = VoiceState::Sustain;
                        }
                    }
                    VoiceState::Sustain => {}
                    VoiceState::Release => {
                        voice.amp -= release_inc;
                        if voice.amp < 0.0 {
                            voice.amp = 0.0;
                        }
                    }
                }

                let freq = Self::note_to_freq(note, self.detune);
                sample += self.waveform.sample(voice.phase) * voice.amp * VOICE_GAIN;
                voice.phase += freq / self.sample_rate;
                if voice.phase >= 1.0 {
                    voice.phase -= 1.0;
                }
            }

            // Drop fully-released voices.
            self.voices
                .retain(|_, v| !(matches!(v.state, VoiceState::Release) && v.amp <= 0.0));

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
