mod filter;
mod oscillator;
mod reverb;

use super::{Plugin, PluginInfo};

use filter::Filter;
use oscillator::{Oscillator, Waveform};
use reverb::Reverb;

/// Load a built-in plugin by source string (e.g. `"builtin:osc"`).
///
/// `osc` is the unified oscillator (pick the waveform via its `waveform`
/// parameter). `sine`/`triangle`/`square` are retained as load-only aliases
/// that start the oscillator on that waveform, so older sessions keep working.
pub fn load(
    source: &str,
    sample_rate: f32,
    _max_block_size: usize,
) -> anyhow::Result<Box<dyn Plugin>> {
    let name = source.strip_prefix("builtin:").unwrap_or(source);
    match name {
        "osc" | "oscillator" | "sine" => {
            Ok(Box::new(Oscillator::new(sample_rate, Waveform::Sine)))
        }
        "triangle" => Ok(Box::new(Oscillator::new(sample_rate, Waveform::Triangle))),
        "square" => Ok(Box::new(Oscillator::new(sample_rate, Waveform::Square))),
        "reverb" => Ok(Box::new(Reverb::new(sample_rate))),
        "filter" => Ok(Box::new(Filter::new(sample_rate))),
        _ => anyhow::bail!(
            "Unknown built-in plugin: {name:?}\n\
             Available built-ins: osc, reverb, filter\n\
             Usage: builtin:osc"
        ),
    }
}

/// Return enumeration info for all built-in plugins.
pub fn enumerate_plugins() -> Vec<PluginInfo> {
    vec![
        PluginInfo {
            name: "Oscillator".into(),
            id: "builtin:osc".into(),
            is_instrument: true,
            param_count: oscillator::PARAM_COUNT,
            preset_count: 0,
            path: "(built-in)".into(),
            scan_ms: 0,
        },
        PluginInfo {
            name: reverb::NAME.into(),
            id: reverb::ID.into(),
            is_instrument: false,
            param_count: reverb::PARAM_COUNT,
            preset_count: reverb::PRESETS.len(),
            path: "(built-in)".into(),
            scan_ms: 0,
        },
        PluginInfo {
            name: filter::NAME.into(),
            id: filter::ID.into(),
            is_instrument: false,
            param_count: filter::PARAM_COUNT,
            preset_count: 0,
            path: "(built-in)".into(),
            scan_ms: 0,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unified osc and the legacy waveform aliases all load, and the
    /// aliases start the oscillator on the matching waveform (the `waveform`
    /// parameter, index 0: sine=0, triangle=1, square=2).
    #[test]
    fn osc_and_aliases_load() {
        for (id, expected_waveform) in [
            ("builtin:osc", 0.0),
            ("builtin:sine", 0.0),
            ("builtin:triangle", 1.0),
            ("builtin:square", 2.0),
        ] {
            let mut p = load(id, 48000.0, 512).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(p.is_instrument(), "{id} should be an instrument");
            assert_eq!(
                p.get_parameter(0),
                Some(expected_waveform),
                "{id} should start on waveform {expected_waveform}"
            );
        }
    }

    #[test]
    fn enumerate_lists_one_oscillator() {
        let plugins = enumerate_plugins();
        let oscs: Vec<_> = plugins.iter().filter(|p| p.is_instrument).collect();
        assert_eq!(oscs.len(), 1, "should enumerate a single oscillator");
        assert_eq!(oscs[0].id, "builtin:osc");
    }
}
