pub mod autodetect;
pub mod builtin;
pub mod chain;
pub mod clap;
#[cfg(feature = "lv2")]
pub mod lv2;
#[cfg(feature = "vst3")]
pub mod vst3;

#[derive(Clone, Default)]
pub struct ParameterInfo {
    pub index: u32,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    /// Names for discrete parameter values (e.g. filter types). When set,
    /// the parameter is an enum: the value is an index into this list (min
    /// 0, max len-1), and the UI shows names and steps by 1 instead of
    /// rendering a continuous bar.
    pub labels: Option<Vec<String>>,
    /// What the value is measured in (`"Hz"`, `"s"`, `"st"`, ...), for showing it.
    /// `None` for a plain number, or when the plugin does not say.
    pub unit: Option<String>,
    /// The sections the plugin files the parameter under, outermost first
    /// (`["Envelope"]`); empty for one at the top level. A client lays a big
    /// plugin's parameters out by it.
    pub group: Vec<String>,
    /// How many steps there are between `min` and `max`: 0 for a continuous
    /// parameter, 3 for one with four values (an enum with four labels).
    pub step_count: u32,
}

#[derive(Debug, Clone)]
pub struct Preset {
    pub name: String,
    pub id: String,
}

/// What an effect that takes a second audio input calls its two inputs (P3g of `docs/patch-graph.md` §11.6: the small half of "the ports that plugins have"): the main one, which is
/// what its chain and the cables that end at its first input give it, and the *side* one, which the cables that end at its second input bring. A page names the two handles by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SideInput {
    pub main: &'static str,
    pub side: &'static str,
}

/// A loaded plugin instance ready to process audio.
pub trait Plugin: Send {
    fn name(&self) -> &str;
    fn is_instrument(&self) -> bool;
    fn sample_rate(&self) -> f32;
    fn audio_output_count(&self) -> usize;
    #[allow(dead_code)]
    fn audio_input_count(&self) -> usize;
    fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()>;

    fn parameters(&self) -> Vec<ParameterInfo>;
    #[allow(dead_code)]
    fn get_parameter(&mut self, index: u32) -> Option<f32>;
    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()>;

    fn presets(&self) -> Vec<Preset>;
    fn load_preset(&mut self, id: &str) -> anyhow::Result<()>;

    /// How many frames later what comes out is than what went in (the look-ahead of a limiter, the filters of an oversampler), or, for an instrument, than the notes that made it.
    /// The host puts the paths that are added up in step by it (P3f of `docs/patch-graph.md` §11.6: where two paths of different latency meet, the shorter is delayed; the dry part of an
    /// effect that is blended with its output is as late as the output is), so it has to be what the plugin does: too little leaves a comb filter where the dry sound and the
    /// plugin's meet, and too much puts them out of step the other way. It is asked once a block, so an adapter answers from what it already has and does not call into the plugin.
    /// A plugin whose latency changes while it runs may say the new one: the host makes its plan again for it. The default is none.
    fn latency(&self) -> u32 {
        0
    }

    /// Whether the effect has a second audio input, and what its two inputs are called (the default is none: an effect has the one that the chain feeds, and a plugin of another format
    /// that has a sidechain gets silence there, as it did). The host gives an effect that has one a second port in its plan, which cables can end at, and calls
    /// [`Plugin::process_side`] for it.
    fn side_input(&self) -> Option<SideInput> {
        None
    }

    /// `process` for an effect that has a side input: `side` is the sound that the cables at its second input bring (a buffer for each channel, as `audio_in` has), or `None` while no
    /// cable ends there, so that the plugin can have a sound of its own in its place. The default is a plugin that has no side input, and leaves it.
    fn process_side(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        audio_in: &[&[f32]],
        side: Option<&[&[f32]]>,
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()> {
        let _ = side;
        self.process(midi_events, audio_in, audio_out)
    }
}

/// Summary info returned by plugin enumeration.
pub struct PluginInfo {
    pub name: String,
    pub id: String,
    pub is_instrument: bool,
    pub param_count: usize,
    pub preset_count: usize,
    pub path: String,
    pub scan_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginType {
    #[cfg(feature = "lv2")]
    Lv2,
    Clap,
    #[cfg(feature = "vst3")]
    Vst3,
}

#[derive(Default)]
pub struct Runtime {
    /// Lazily created on the first LV2 load — building the world scans every
    /// installed LV2 bundle (~hundreds of ms), which sessions without LV2
    /// plugins shouldn't pay at startup.
    #[cfg(feature = "lv2")]
    lv2: Option<(std::sync::OnceLock<lv2::Lv2Runtime>, usize)>,
}

impl Runtime {
    #[cfg(feature = "lv2")]
    pub fn with_lv2(max_block_size: usize) -> Self {
        Self {
            lv2: Some((std::sync::OnceLock::new(), max_block_size)),
        }
    }

    #[cfg(feature = "lv2")]
    fn lv2(&self) -> Option<&lv2::Lv2Runtime> {
        self.lv2
            .as_ref()
            .map(|(cell, max_block_size)| {
                cell.get_or_init(|| lv2::Lv2Runtime::new(*max_block_size))
            })
    }
}

/// Load a plugin from the given source, returning a boxed Plugin trait object.
pub fn load(
    source: &str,
    sample_rate: f32,
    max_block_size: usize,
    _runtime: &Runtime,
) -> anyhow::Result<Box<dyn Plugin>> {
    if source.starts_with("builtin:") {
        return builtin::load(source, sample_rate, max_block_size);
    }

    let (plugin_type, resolved) = autodetect::resolve(source)?;
    match plugin_type {
        #[cfg(feature = "lv2")]
        PluginType::Lv2 => lv2::load(
            &resolved,
            sample_rate,
            max_block_size,
            _runtime.lv2(),
        ),
        PluginType::Clap => clap::load(&resolved, sample_rate, max_block_size),
        #[cfg(feature = "vst3")]
        PluginType::Vst3 => vst3::load(&resolved, sample_rate, max_block_size),
    }
}
