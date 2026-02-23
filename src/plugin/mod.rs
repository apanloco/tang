pub mod autodetect;
pub mod builtin;
pub mod chain;
pub mod clap;
#[cfg(feature = "lv2")]
pub mod lv2;
#[cfg(feature = "vst3")]
pub mod vst3;

#[derive(Clone)]
pub struct ParameterInfo {
    pub index: u32,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

#[derive(Clone)]
pub struct Preset {
    pub name: String,
    pub id: String,
}

/// A loaded plugin instance ready to process audio.
pub trait Plugin: Send {
    fn name(&self) -> &str;
    fn is_instrument(&self) -> bool;
    #[expect(dead_code)]
    fn sample_rate(&self) -> f32;
    fn audio_output_count(&self) -> usize;
    #[expect(dead_code)]
    fn audio_input_count(&self) -> usize;
    fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()>;

    fn parameters(&self) -> Vec<ParameterInfo>;
    #[expect(dead_code)]
    fn get_parameter(&mut self, index: u32) -> Option<f32>;
    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()>;

    fn presets(&self) -> Vec<Preset>;
    fn load_preset(&mut self, id: &str) -> anyhow::Result<()>;
}

/// Summary info returned by plugin enumeration.
pub struct PluginInfo {
    pub name: String,
    pub id: String,
    pub is_instrument: bool,
    pub param_count: usize,
    pub preset_count: usize,
    pub path: String,
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
    #[cfg(feature = "lv2")]
    pub lv2: Option<lv2::Lv2Runtime>,
}

impl Runtime {
    #[cfg(feature = "lv2")]
    pub fn with_lv2(max_block_size: usize) -> Self {
        Self {
            lv2: Some(lv2::Lv2Runtime::new(max_block_size)),
        }
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
            _runtime.lv2.as_ref(),
        ),
        PluginType::Clap => clap::load(&resolved, sample_rate, max_block_size),
        #[cfg(feature = "vst3")]
        PluginType::Vst3 => vst3::load(&resolved, sample_rate, max_block_size),
    }
}
