use std::collections::HashMap;
use std::mem::MaybeUninit;

use crossbeam_channel::{Receiver, Sender};

use super::Plugin;
use crate::session::{self, RemapTarget};

/// Maximum number of audio channels supported (for stack-allocated reference arrays).
const MAX_CHANNELS: usize = 16;

/// Pre-computed remap entry for a single note.
#[derive(Debug, Clone)]
struct RemapEntry {
    target_note: u8,
    channel: u8,
    pitch_bend_lsb: u8,
    pitch_bend_msb: u8,
}

/// Remaps specific MIDI notes to different notes on separate channels with pitch bend.
///
/// Normal notes pass through on channel 1 (status nibble 0x00).
/// Remapped notes are rewritten to a target note on channels 2-16, with a pitch bend
/// message inserted before each note-on to shift the pitch to the correct frequency.
#[derive(Debug, Clone)]
pub struct NoteRemapper {
    table: HashMap<u8, RemapEntry>,
}

impl NoteRemapper {
    /// Build a remapper from the session config.
    ///
    /// Groups entries by detune value and assigns MIDI channels 1-15 (status nibble 0x01-0x0F,
    /// i.e. MIDI channels 2-16). Returns an error if detune exceeds pitch_bend_range or if
    /// there are more than 15 distinct detune values.
    pub fn from_config(
        remap: &HashMap<String, RemapTarget>,
        pitch_bend_range: f64,
    ) -> anyhow::Result<Self> {
        if remap.is_empty() {
            return Ok(NoteRemapper {
                table: HashMap::new(),
            });
        }

        // Group by detune value to assign channels. Use ordered floats as keys.
        // We use a Vec to maintain insertion order and dedup by approximate equality.
        let mut detune_channels: Vec<(f64, u8)> = Vec::new();

        let mut table = HashMap::new();

        // First pass: collect all distinct detune values
        for target in remap.values() {
            if target.detune.abs() > pitch_bend_range {
                anyhow::bail!(
                    "detune {:.1} exceeds pitch_bend_range ±{:.1}",
                    target.detune,
                    pitch_bend_range
                );
            }
            let existing = detune_channels
                .iter()
                .find(|(d, _)| (*d - target.detune).abs() < 1e-9);
            if existing.is_none() {
                if detune_channels.len() >= 15 {
                    anyhow::bail!(
                        "too many distinct detune values (max 15, MIDI channels 2-16)"
                    );
                }
                // Channel status nibble: 0x01 for ch2, 0x02 for ch3, etc.
                let ch = detune_channels.len() as u8 + 1;
                detune_channels.push((target.detune, ch));
            }
        }

        // Second pass: build the lookup table
        for (source_name, target) in remap {
            let source_note = session::parse_note_name(source_name)?;
            let target_note = session::parse_note_name(&target.note)?;

            let &(_, channel) = detune_channels
                .iter()
                .find(|(d, _)| (*d - target.detune).abs() < 1e-9)
                .unwrap();

            // Pre-compute pitch bend: center is 8192, range maps to ±pitch_bend_range semitones
            let bend_value =
                (8192.0 + (target.detune / pitch_bend_range) * 8191.0).round() as i32;
            let bend_clamped = bend_value.clamp(0, 16383) as u16;
            let lsb = (bend_clamped & 0x7F) as u8;
            let msb = ((bend_clamped >> 7) & 0x7F) as u8;

            table.insert(
                source_note,
                RemapEntry {
                    target_note,
                    channel,
                    pitch_bend_lsb: lsb,
                    pitch_bend_msb: msb,
                },
            );
        }

        Ok(NoteRemapper { table })
    }

    /// Remap MIDI events, writing results into the provided buffer.
    /// For remapped note-on events, a pitch bend message is inserted before the note-on.
    /// For remapped note-off events, the note and channel are rewritten.
    /// All other events pass through unchanged.
    pub fn remap_events(
        &self,
        input: &[(u64, [u8; 3])],
        output: &mut Vec<(u64, [u8; 3])>,
    ) {
        output.clear();
        for &(frame, bytes) in input {
            let status_type = bytes[0] & 0xF0;
            let note = bytes[1];

            match status_type {
                0x90 if bytes[2] > 0 => {
                    // Note-on
                    if let Some(entry) = self.table.get(&note) {
                        log::info!(
                            "Remap: NoteOn {} → {} ch={} bend=({},{})",
                            note, entry.target_note, entry.channel + 1,
                            entry.pitch_bend_lsb, entry.pitch_bend_msb,
                        );
                        // Rewritten note-on on remapped channel
                        output.push((
                            frame,
                            [0x90 | entry.channel, entry.target_note, bytes[2]],
                        ));
                        // Pitch bend after note-on
                        output.push((
                            frame,
                            [0xE0 | entry.channel, entry.pitch_bend_lsb, entry.pitch_bend_msb],
                        ));
                    } else {
                        output.push((frame, bytes));
                    }
                }
                0x80 | 0x90 => {
                    // Note-off (0x80 or 0x90 with velocity 0)
                    if let Some(entry) = self.table.get(&note) {
                        log::info!(
                            "Remap: NoteOff {} → {} ch={}",
                            note, entry.target_note, entry.channel + 1,
                        );
                        output.push((
                            frame,
                            [(status_type) | entry.channel, entry.target_note, bytes[2]],
                        ));
                    } else {
                        output.push((frame, bytes));
                    }
                }
                _ => {
                    output.push((frame, bytes));
                }
            }
        }
    }
}

/// Build `&mut [&mut [f32]]` on the stack from `&mut [Vec<f32>]`.
///
/// # Panics
/// Panics if `bufs.len() > MAX_CHANNELS`.
fn mut_slices<'a>(
    bufs: &'a mut [Vec<f32>],
    storage: &'a mut [MaybeUninit<&'a mut [f32]>; MAX_CHANNELS],
) -> &'a mut [&'a mut [f32]] {
    let n = bufs.len();
    assert!(n <= MAX_CHANNELS);
    for (i, buf) in bufs.iter_mut().enumerate() {
        storage[i].write(buf.as_mut_slice());
    }
    // SAFETY: first `n` elements are initialized. MaybeUninit<T> is #[repr(transparent)].
    unsafe { std::slice::from_raw_parts_mut(storage.as_mut_ptr().cast(), n) }
}

/// Build `&[&[f32]]` on the stack from `&[Vec<f32>]`.
///
/// # Panics
/// Panics if `bufs.len() > MAX_CHANNELS`.
fn shared_slices<'a>(
    bufs: &'a [Vec<f32>],
    storage: &'a mut [MaybeUninit<&'a [f32]>; MAX_CHANNELS],
) -> &'a [&'a [f32]] {
    let n = bufs.len();
    assert!(n <= MAX_CHANNELS);
    for (i, buf) in bufs.iter().enumerate() {
        storage[i].write(buf.as_slice());
    }
    // SAFETY: first `n` elements are initialized. MaybeUninit<T> is #[repr(transparent)].
    unsafe { std::slice::from_raw_parts(storage.as_ptr().cast(), n) }
}

/// Commands sent from the main thread to mutate the plugin chain on the audio thread.
pub enum ChainCommand {
    SwapInstrument {
        instrument: Box<dyn Plugin>,
        /// Pre-allocated inst_buf (built on main thread to avoid audio-thread allocation).
        inst_buf: Vec<Vec<f32>>,
        /// Optional note remapper for pitch-bend-based note substitution.
        remapper: Option<NoteRemapper>,
    },
    InsertEffect {
        index: usize,
        effect: Box<dyn Plugin>,
        mix: f64,
    },
    #[allow(dead_code)]
    RemoveEffect { index: usize },
    #[allow(dead_code)]
    ReorderEffect { from: usize, to: usize },
    /// Set a parameter on a plugin. slot 0 = instrument, 1..N = effects.
    SetParameter {
        slot: usize,
        param_index: u32,
        value: f32,
    },
    /// Set the host-side dry/wet mix on an effect. slot 1..N = effects.
    #[expect(dead_code)]
    SetMix { slot: usize, value: f32 },
}

/// A plugin chain: one instrument followed by zero or more effects in series.
///
/// Uses ping-pong buffering between effects to avoid allocations during process().
/// Commands are drained at the top of every audio callback via try_recv loop.
pub struct PluginChain {
    instrument: Option<Box<dyn Plugin>>,
    effects: Vec<Box<dyn Plugin>>,
    mix_values: Vec<f64>,
    /// Full instrument output buffer (may be >2 channels for multi-output instruments)
    inst_buf: Vec<Vec<f32>>,
    buf_a: Vec<Vec<f32>>,
    buf_b: Vec<Vec<f32>>,
    num_channels: usize,
    command_rx: Receiver<ChainCommand>,
    return_tx: Sender<Box<dyn Plugin>>,
    note_remapper: Option<NoteRemapper>,
    remapped_events: Vec<(u64, [u8; 3])>,
}

impl PluginChain {
    /// Create an empty plugin chain. Outputs silence until an instrument is swapped in.
    pub fn new(
        num_channels: usize,
        command_rx: Receiver<ChainCommand>,
        return_tx: Sender<Box<dyn Plugin>>,
    ) -> Self {
        let buf_a = (0..num_channels).map(|_| Vec::new()).collect();
        let buf_b = (0..num_channels).map(|_| Vec::new()).collect();

        PluginChain {
            instrument: None,
            effects: Vec::new(),
            mix_values: Vec::new(),
            inst_buf: Vec::new(),
            buf_a,
            buf_b,
            num_channels,
            command_rx,
            return_tx,
            note_remapper: None,
            remapped_events: Vec::with_capacity(128),
        }
    }

    pub fn num_channels(&self) -> usize {
        self.num_channels
    }

    /// Drain all pending commands from the command channel (lock-free).
    pub fn drain_commands(&mut self) {
        while let Ok(cmd) = self.command_rx.try_recv() {
            match cmd {
                ChainCommand::SwapInstrument {
                    instrument: new_inst,
                    inst_buf,
                    remapper,
                } => {
                    self.inst_buf = inst_buf;
                    self.note_remapper = remapper;

                    if let Some(old) = self.instrument.replace(new_inst) {
                        let _ = self.return_tx.try_send(old);
                    }
                }
                ChainCommand::InsertEffect { index, effect, mix } => {
                    if effect.audio_output_count() != self.num_channels {
                        log::warn!(
                            "Rejecting effect '{}': output channels {} != chain channels {}",
                            effect.name(),
                            effect.audio_output_count(),
                            self.num_channels,
                        );
                        let _ = self.return_tx.try_send(effect);
                    } else {
                        let idx = index.min(self.effects.len());
                        self.effects.insert(idx, effect);
                        self.mix_values.insert(idx, mix);
                    }
                }
                ChainCommand::RemoveEffect { index } => {
                    if index < self.effects.len() {
                        let old = self.effects.remove(index);
                        self.mix_values.remove(index);
                        let _ = self.return_tx.try_send(old);
                    }
                }
                ChainCommand::ReorderEffect { from, to } => {
                    if from < self.effects.len() && to < self.effects.len() && from != to {
                        let effect = self.effects.remove(from);
                        let mix = self.mix_values.remove(from);
                        self.effects.insert(to, effect);
                        self.mix_values.insert(to, mix);
                    }
                }
                ChainCommand::SetParameter {
                    slot,
                    param_index,
                    value,
                } => {
                    let plugin: Option<&mut Box<dyn Plugin>> = if slot == 0 {
                        self.instrument.as_mut()
                    } else {
                        self.effects.get_mut(slot - 1)
                    };
                    if let Some(p) = plugin {
                        if let Err(e) = p.set_parameter(param_index, value) {
                            log::warn!("SetParameter slot={slot} index={param_index}: {e}");
                        }
                    }
                }
                ChainCommand::SetMix { slot, value } => {
                    if slot > 0 {
                        if let Some(mix) = self.mix_values.get_mut(slot - 1) {
                            *mix = value as f64;
                        }
                    }
                }
            }
        }
    }

    /// Process audio: drain commands, run instrument + effects, write to output.
    /// Outputs silence if no instrument is loaded.
    pub fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        audio_out: &mut [Vec<f32>],
    ) -> anyhow::Result<()> {
        self.drain_commands();

        // Apply note remapping if configured
        let effective_events: &[(u64, [u8; 3])] = if let Some(ref remapper) = self.note_remapper {
            remapper.remap_events(midi_events, &mut self.remapped_events);
            &self.remapped_events
        } else {
            midi_events
        };

        let instrument = match self.instrument.as_mut() {
            Some(inst) => inst,
            None => {
                // No instrument — output silence
                for ch in audio_out.iter_mut() {
                    ch.fill(0.0);
                }
                return Ok(());
            }
        };

        let frames = audio_out.first().map(|b| b.len()).unwrap_or(0);
        let inst_outputs = self.inst_buf.len();

        if inst_outputs <= self.num_channels && self.effects.is_empty() {
            // Fast path: instrument output fits in audio_out, no effects
            let mut storage = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
            let out_refs = mut_slices(audio_out, &mut storage);
            return instrument.process(effective_events, &[], out_refs);
        }

        // Resize inst_buf (may have more channels than the chain for multi-output instruments)
        for buf in self.inst_buf.iter_mut() {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }

        // Instrument → inst_buf
        {
            let mut storage = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
            let refs = mut_slices(&mut self.inst_buf, &mut storage);
            instrument.process(effective_events, &[], refs)?;
        }

        if self.effects.is_empty() {
            // No effects — copy first num_channels from inst_buf to output
            for (ch, out) in audio_out.iter_mut().enumerate() {
                out.copy_from_slice(&self.inst_buf[ch]);
            }
            return Ok(());
        }

        // Resize effect ping-pong buffers
        for buf in self.buf_a.iter_mut().chain(self.buf_b.iter_mut()) {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }

        // Copy first num_channels from inst_buf → buf_a
        for ch in 0..self.num_channels {
            self.buf_a[ch].copy_from_slice(&self.inst_buf[ch]);
        }

        // Effects: alternate between buf_a and buf_b
        let mut src_is_a = true;

        for (effect, &mix) in self.effects.iter_mut().zip(self.mix_values.iter()) {
            let mix = mix as f32;

            if src_is_a {
                {
                    let mut in_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let mut out_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let in_refs = shared_slices(&self.buf_a, &mut in_s);
                    let out_refs = mut_slices(&mut self.buf_b, &mut out_s);
                    effect.process(&[], in_refs, out_refs)?;
                }

                if mix < 1.0 {
                    let dry = 1.0 - mix;
                    for ch in 0..self.num_channels {
                        for i in 0..frames {
                            self.buf_b[ch][i] = self.buf_a[ch][i] * dry + self.buf_b[ch][i] * mix;
                        }
                    }
                }
            } else {
                {
                    let mut in_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let mut out_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let in_refs = shared_slices(&self.buf_b, &mut in_s);
                    let out_refs = mut_slices(&mut self.buf_a, &mut out_s);
                    effect.process(&[], in_refs, out_refs)?;
                }

                if mix < 1.0 {
                    let dry = 1.0 - mix;
                    for ch in 0..self.num_channels {
                        for i in 0..frames {
                            self.buf_a[ch][i] = self.buf_b[ch][i] * dry + self.buf_a[ch][i] * mix;
                        }
                    }
                }
            }
            src_is_a = !src_is_a;
        }

        // Copy final result to audio_out
        let final_buf = if src_is_a { &self.buf_a } else { &self.buf_b };
        for (ch, out) in audio_out.iter_mut().enumerate() {
            if ch < final_buf.len() {
                let copy_len = out.len().min(final_buf[ch].len());
                out[..copy_len].copy_from_slice(&final_buf[ch][..copy_len]);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{ParameterInfo, Preset};

    const FRAMES: usize = 64;

    macro_rules! mock_plugin_boilerplate {
        () => {
            fn sample_rate(&self) -> f32 {
                48000.0
            }
            fn parameters(&self) -> Vec<ParameterInfo> {
                Vec::new()
            }
            fn get_parameter(&mut self, _: u32) -> Option<f32> {
                None
            }
            fn set_parameter(&mut self, i: u32, _: f32) -> anyhow::Result<()> {
                anyhow::bail!("no parameter {i}")
            }
            fn presets(&self) -> Vec<Preset> {
                Vec::new()
            }
            fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
                anyhow::bail!("no preset {id}")
            }
        };
    }

    /// Test instrument: outputs a constant value on all channels when a note is held.
    struct ConstInstrument {
        value: f32,
        num_outputs: usize,
        has_note: bool,
    }

    impl ConstInstrument {
        fn new(value: f32) -> Box<dyn Plugin> {
            Box::new(Self {
                value,
                num_outputs: 2,
                has_note: false,
            })
        }
        fn with_outputs(value: f32, num_outputs: usize) -> Box<dyn Plugin> {
            Box::new(Self {
                value,
                num_outputs,
                has_note: false,
            })
        }
    }

    impl Plugin for ConstInstrument {
        fn name(&self) -> &str {
            "ConstInstrument"
        }
        fn is_instrument(&self) -> bool {
            true
        }
        fn audio_output_count(&self) -> usize {
            self.num_outputs
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
            for &(_, [status, _, velocity]) in midi_events {
                match status & 0xF0 {
                    0x90 if velocity > 0 => self.has_note = true,
                    0x80 | 0x90 => self.has_note = false,
                    _ => {}
                }
            }
            let v = if self.has_note { self.value } else { 0.0 };
            for ch in audio_out.iter_mut() {
                ch.fill(v);
            }
            Ok(())
        }

        mock_plugin_boilerplate!();
    }

    /// Effect that copies input to output unchanged.
    struct PassthroughEffect;

    impl Plugin for PassthroughEffect {
        fn name(&self) -> &str {
            "Passthrough"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }

        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                out.copy_from_slice(inp);
            }
            Ok(())
        }

        mock_plugin_boilerplate!();
    }

    /// Effect that multiplies input by a constant gain.
    struct ScaleEffect(f32);

    impl Plugin for ScaleEffect {
        fn name(&self) -> &str {
            "Scale"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }

        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                for (o, &i) in out.iter_mut().zip(inp.iter()) {
                    *o = i * self.0;
                }
            }
            Ok(())
        }

        mock_plugin_boilerplate!();
    }

    /// Effect that adds a constant offset to input.
    struct OffsetEffect(f32);

    impl Plugin for OffsetEffect {
        fn name(&self) -> &str {
            "Offset"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }

        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                for (o, &i) in out.iter_mut().zip(inp.iter()) {
                    *o = i + self.0;
                }
            }
            Ok(())
        }

        mock_plugin_boilerplate!();
    }

    // -- helpers --

    fn make_chain(
        num_channels: usize,
    ) -> (
        PluginChain,
        crossbeam_channel::Sender<ChainCommand>,
        crossbeam_channel::Receiver<Box<dyn Plugin>>,
    ) {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        (
            PluginChain::new(num_channels, cmd_rx, return_tx),
            cmd_tx,
            return_rx,
        )
    }

    fn make_output() -> Vec<Vec<f32>> {
        vec![vec![0.0; FRAMES]; 2]
    }

    fn note_on(note: u8) -> (u64, [u8; 3]) {
        (0, [0x90, note, 100])
    }

    fn note_off(note: u8) -> (u64, [u8; 3]) {
        (0, [0x80, note, 0])
    }

    fn swap_instrument(cmd_tx: &crossbeam_channel::Sender<ChainCommand>, inst: Box<dyn Plugin>) {
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(ChainCommand::SwapInstrument {
                instrument: inst,
                inst_buf,
                remapper: None,
            })
            .unwrap();
    }

    fn insert_effect(
        cmd_tx: &crossbeam_channel::Sender<ChainCommand>,
        index: usize,
        effect: Box<dyn Plugin>,
        mix: f64,
    ) {
        cmd_tx
            .send(ChainCommand::InsertEffect { index, effect, mix })
            .unwrap();
    }

    // -- tests --

    #[test]
    fn silence_when_no_instrument() {
        let (mut chain, _, _) = make_chain(2);
        let mut out = make_output();
        out[0].fill(999.0);
        out[1].fill(999.0);

        chain.process(&[], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.0));
        assert!(out[1].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn instrument_direct_output() {
        let (mut chain, cmd_tx, _) = make_chain(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.75));

        let mut out = make_output();
        chain.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.75));
        assert!(out[1].iter().all(|&s| s == 0.75));
    }

    #[test]
    fn instrument_silence_without_note() {
        let (mut chain, cmd_tx, _) = make_chain(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.75));

        let mut out = make_output();
        chain.process(&[], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn note_off_silences_instrument() {
        let (mut chain, cmd_tx, _) = make_chain(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.75));

        let mut out = make_output();
        chain.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 0.75));

        let mut out = make_output();
        chain.process(&[note_off(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn single_passthrough_effect() {
        let (mut chain, cmd_tx, _) = make_chain(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.5));
        insert_effect(&cmd_tx, 0, Box::new(PassthroughEffect), 1.0);

        let mut out = make_output();
        chain.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.5));
        assert!(out[1].iter().all(|&s| s == 0.5));
    }

    #[test]
    fn dry_wet_mix() {
        let (mut chain, cmd_tx, _) = make_chain(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        // ScaleEffect(0.0) outputs silence; mix=0.5 → 0.5*dry + 0.5*wet = 0.5
        insert_effect(&cmd_tx, 0, Box::new(ScaleEffect(0.0)), 0.5);

        let mut out = make_output();
        chain.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| (s - 0.5).abs() < 1e-6));
        assert!(out[1].iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn multiple_effects_chain() {
        let (mut chain, cmd_tx, _) = make_chain(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        insert_effect(&cmd_tx, 0, Box::new(ScaleEffect(0.5)), 1.0);
        insert_effect(&cmd_tx, 1, Box::new(ScaleEffect(0.5)), 1.0);

        let mut out = make_output();
        chain.process(&[note_on(60)], &mut out).unwrap();

        // 1.0 * 0.5 * 0.5 = 0.25
        assert!(out[0].iter().all(|&s| (s - 0.25).abs() < 1e-6));
    }

    #[test]
    fn multi_output_instrument_truncation() {
        let (mut chain, cmd_tx, _) = make_chain(2);
        swap_instrument(&cmd_tx, ConstInstrument::with_outputs(0.8, 4));
        insert_effect(&cmd_tx, 0, Box::new(PassthroughEffect), 1.0);

        let mut out = make_output();
        chain.process(&[note_on(60)], &mut out).unwrap();

        // Only first 2 of 4 channels reach the output
        assert!(out[0].iter().all(|&s| s == 0.8));
        assert!(out[1].iter().all(|&s| s == 0.8));
    }

    #[test]
    fn multi_output_instrument_no_effects() {
        let (mut chain, cmd_tx, _) = make_chain(2);
        // 16-output instrument with no effects (the Pianoteq scenario)
        swap_instrument(&cmd_tx, ConstInstrument::with_outputs(0.6, 16));

        let mut out = make_output();
        chain.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.6));
        assert!(out[1].iter().all(|&s| s == 0.6));
    }

    #[test]
    fn swap_instrument_returns_old() {
        let (mut chain, cmd_tx, return_rx) = make_chain(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));

        let mut out = make_output();
        chain.process(&[], &mut out).unwrap();

        // Swap in a new instrument
        swap_instrument(&cmd_tx, ConstInstrument::new(0.5));
        chain.process(&[], &mut out).unwrap();

        // Old instrument should have been returned via the channel
        let old = return_rx.try_recv();
        assert!(old.is_ok());
        assert_eq!(old.unwrap().name(), "ConstInstrument");
    }

    #[test]
    fn remove_effect() {
        let (mut chain, cmd_tx, _) = make_chain(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        insert_effect(&cmd_tx, 0, Box::new(ScaleEffect(0.5)), 1.0);

        let mut out = make_output();
        chain.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 0.5).abs() < 1e-6));

        // Remove the effect — should go back to direct instrument output
        cmd_tx
            .send(ChainCommand::RemoveEffect { index: 0 })
            .unwrap();

        let mut out = make_output();
        chain.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 1.0));
    }

    #[test]
    fn reorder_effects() {
        let (mut chain, cmd_tx, _) = make_chain(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        // [Scale(2.0), Offset(0.5)] → 1.0 * 2.0 + 0.5 = 2.5
        insert_effect(&cmd_tx, 0, Box::new(ScaleEffect(2.0)), 1.0);
        insert_effect(&cmd_tx, 1, Box::new(OffsetEffect(0.5)), 1.0);

        let mut out = make_output();
        chain.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 2.5).abs() < 1e-6));

        // Move Scale from index 0 to index 1 → [Offset(0.5), Scale(2.0)]
        // (1.0 + 0.5) * 2.0 = 3.0
        cmd_tx
            .send(ChainCommand::ReorderEffect { from: 0, to: 1 })
            .unwrap();

        let mut out = make_output();
        chain.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 3.0).abs() < 1e-6));
    }

    #[test]
    fn reject_effect_with_wrong_channel_count() {
        /// Mono effect (1 output) — incompatible with a stereo chain.
        struct MonoEffect;

        impl Plugin for MonoEffect {
            fn name(&self) -> &str {
                "MonoEffect"
            }
            fn is_instrument(&self) -> bool {
                false
            }
            fn audio_output_count(&self) -> usize {
                1
            }
            fn audio_input_count(&self) -> usize {
                1
            }

            fn process(
                &mut self,
                _midi_events: &[(u64, [u8; 3])],
                audio_in: &[&[f32]],
                audio_out: &mut [&mut [f32]],
            ) -> anyhow::Result<()> {
                for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                    out.copy_from_slice(inp);
                }
                Ok(())
            }

            mock_plugin_boilerplate!();
        }

        let (mut chain, cmd_tx, return_rx) = make_chain(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        insert_effect(&cmd_tx, 0, Box::new(MonoEffect), 1.0);

        let mut out = make_output();
        chain.process(&[note_on(60)], &mut out).unwrap();

        // Effect was rejected — instrument output passes through directly
        assert!(out[0].iter().all(|&s| s == 1.0));

        // Rejected effect was returned via the return channel
        let returned = return_rx.try_recv();
        assert!(returned.is_ok());
        assert_eq!(returned.unwrap().name(), "MonoEffect");
    }

    // -- NoteRemapper tests --

    fn make_remap(
        entries: &[(&str, &str, f64)],
    ) -> HashMap<String, crate::session::RemapTarget> {
        entries
            .iter()
            .map(|(src, dst, detune)| {
                (
                    src.to_string(),
                    crate::session::RemapTarget {
                        note: dst.to_string(),
                        detune: *detune,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn remapper_from_config_valid() {
        let remap = make_remap(&[("G#4", "G4", 1.0), ("C#2", "D2", -0.5)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        // G#4 = 68, C#2 = 37 — both should be in the table
        assert!(remapper.table.contains_key(&68));
        assert!(remapper.table.contains_key(&37));
    }

    #[test]
    fn remapper_remap_note_on() {
        // Remap G#4 (68) → G4 (67) with +1 semitone detune
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        let input = vec![(0u64, [0x90u8, 68, 100])]; // note-on G#4
        let mut output = Vec::new();
        remapper.remap_events(&input, &mut output);

        // Should produce: remapped note-on + pitch bend
        assert_eq!(output.len(), 2);
        // First event: note-on for G4 (67) on channel 2
        assert_eq!(output[0].1[0], 0x91); // note-on ch2
        assert_eq!(output[0].1[1], 67); // G4
        assert_eq!(output[0].1[2], 100); // velocity preserved
        // Second event: pitch bend on channel 2 (status 0xE1)
        assert_eq!(output[1].1[0] & 0xF0, 0xE0);
        assert_eq!(output[1].1[0] & 0x0F, 1); // channel 2 = nibble 0x01
    }

    #[test]
    fn remapper_remap_note_off() {
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        let input = vec![(0u64, [0x80u8, 68, 0])]; // note-off G#4
        let mut output = Vec::new();
        remapper.remap_events(&input, &mut output);

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].1[0], 0x81); // note-off ch2
        assert_eq!(output[0].1[1], 67); // G4
    }

    #[test]
    fn remapper_passthrough_non_remapped() {
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        // C4 (60) is NOT remapped — should pass through unchanged
        let input = vec![(0u64, [0x90u8, 60, 100])];
        let mut output = Vec::new();
        remapper.remap_events(&input, &mut output);

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].1, [0x90, 60, 100]);
    }

    #[test]
    fn remapper_passthrough_non_note_events() {
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        // CC message — should pass through unchanged
        let input = vec![(0u64, [0xB0u8, 64, 127])];
        let mut output = Vec::new();
        remapper.remap_events(&input, &mut output);

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].1, [0xB0, 64, 127]);
    }

    #[test]
    fn remapper_pitch_bend_bytes_center() {
        // Detune 0.0 → pitch bend = 8192 (center)
        let remap = make_remap(&[("C4", "C4", 0.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        let entry = &remapper.table[&60];
        // 8192 = 0b10_0000_0000000 → LSB = 0, MSB = 64
        assert_eq!(entry.pitch_bend_lsb, 0);
        assert_eq!(entry.pitch_bend_msb, 64);
    }

    #[test]
    fn remapper_pitch_bend_bytes_max() {
        // Detune +2.0 with range 2.0 → pitch bend = 8192 + 8191 = 16383
        let remap = make_remap(&[("C4", "C4", 2.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        let entry = &remapper.table[&60];
        // 16383 = 0x3FFF → LSB = 127, MSB = 127
        assert_eq!(entry.pitch_bend_lsb, 127);
        assert_eq!(entry.pitch_bend_msb, 127);
    }

    #[test]
    fn remapper_pitch_bend_bytes_min() {
        // Detune -2.0 with range 2.0 → pitch bend = 8192 - 8191 = 1
        let remap = make_remap(&[("C4", "C4", -2.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        let entry = &remapper.table[&60];
        // 1 = 0b00_0000_0000001 → LSB = 1, MSB = 0
        assert_eq!(entry.pitch_bend_lsb, 1);
        assert_eq!(entry.pitch_bend_msb, 0);
    }

    #[test]
    fn remapper_shared_detune_shares_channel() {
        // Two notes with the same detune should share a MIDI channel
        let remap = make_remap(&[("C4", "B3", 1.0), ("D4", "C#4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        assert_eq!(remapper.table[&60].channel, remapper.table[&62].channel);
    }

    #[test]
    fn remapper_different_detune_different_channels() {
        let remap = make_remap(&[("C4", "B3", 1.0), ("D4", "C#4", -0.5)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        assert_ne!(remapper.table[&60].channel, remapper.table[&62].channel);
    }

    #[test]
    fn remapper_error_detune_exceeds_range() {
        let remap = make_remap(&[("C4", "B3", 3.0)]);
        let result = NoteRemapper::from_config(&remap, 2.0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exceeds"));
    }

    #[test]
    fn remapper_error_too_many_detune_values() {
        // 16 distinct detune values should fail (max 15)
        let notes = [
            "C2", "C#2", "D2", "D#2", "E2", "F2", "F#2", "G2", "G#2", "A2", "A#2", "B2", "C3",
            "C#3", "D3", "D#3",
        ];
        let entries: Vec<(&str, &str, f64)> = notes
            .iter()
            .enumerate()
            .map(|(i, &n)| (n, n, (i as f64 + 1.0) * 0.1))
            .collect();
        let remap = make_remap(&entries);
        let result = NoteRemapper::from_config(&remap, 10.0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too many"));
    }

    #[test]
    fn remapper_integration_with_chain() {
        // Integration test: remap G#4→G4 with pitch bend, verify instrument sees remapped events
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        let (mut chain, cmd_tx, _) = make_chain(2);
        let inst = ConstInstrument::new(0.75);
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(ChainCommand::SwapInstrument {
                instrument: inst,
                inst_buf,
                remapper: Some(remapper),
            })
            .unwrap();

        let mut out = make_output();
        // Send note-on for G#4 (68) — should be remapped to G4 (67) on ch2
        // ConstInstrument responds to any note-on, so we just verify it produces output
        chain.process(&[note_on(68)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 0.75));
    }
}
