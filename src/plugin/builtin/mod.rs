mod oscillator;
mod reverb;

use super::{Plugin, PluginInfo};

use oscillator::{Oscillator, Waveform};
use reverb::Reverb;

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
        "reverb" => Ok(Box::new(Reverb::new(sample_rate))),
        _ => anyhow::bail!(
            "Unknown built-in plugin: {name:?}\n\
             Available built-ins: sine, triangle, square, reverb\n\
             Usage: builtin:sine"
        ),
    }
}

/// Return enumeration info for all built-in plugins.
pub fn enumerate_plugins() -> Vec<PluginInfo> {
    let mut out = Vec::new();
    for w in [Waveform::Sine, Waveform::Triangle, Waveform::Square] {
        out.push(PluginInfo {
            name: w.name().into(),
            id: w.id().into(),
            is_instrument: true,
            param_count: 1,
            preset_count: 0,
            path: "(built-in)".into(),
            scan_ms: 0,
        });
    }
    out.push(PluginInfo {
        name: reverb::NAME.into(),
        id: reverb::ID.into(),
        is_instrument: false,
        param_count: reverb::PARAM_COUNT,
        preset_count: reverb::PRESETS.len(),
        path: "(built-in)".into(),
        scan_ms: 0,
    });
    out
}
