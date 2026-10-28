use crate::plugin::builtin;
use crate::plugin::clap;
#[cfg(feature = "lv2")]
use crate::plugin::lv2;
#[cfg(feature = "vst3")]
use crate::plugin::vst3;

pub fn midi() -> anyhow::Result<()> {
    println!("=== MIDI Input Devices ===");
    let midi_in = midir::MidiInput::new("tang-enumerate")?;
    let ports = midi_in.ports();
    if ports.is_empty() {
        println!("  (none found)");
    }
    for port in &ports {
        let name = midi_in.port_name(port).unwrap_or_else(|_| "Unknown".into());
        println!("  {name}");
    }
    Ok(())
}

pub fn audio() -> anyhow::Result<()> {
    // Suppress ALSA/JACK noise on stderr during device enumeration
    let stderr_guard = crate::tty::StderrSilencer::new();

    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let default_name = host.default_output_device().and_then(|d| d.name().ok());
    let devices: Vec<_> = host
        .devices()?
        .filter_map(|device| {
            let name = device.name().ok()?;
            let config = device.default_output_config().ok()?;
            Some((name, config))
        })
        .collect();

    // The audio input (`builtin:input`) opens its stream on the host that `audio::input_host` says, which is not always the output's.
    let input_host = crate::audio::input_host();
    let default_input = input_host.default_input_device().and_then(|d| d.name().ok());
    let inputs: Vec<_> = input_host
        .input_devices()
        .map(|found| {
            found
                .filter_map(|device| {
                    let name = device.name().ok()?;
                    let config = device.default_input_config().ok()?;
                    Some((name, config))
                })
                .collect()
        })
        .unwrap_or_default();

    let _ = stderr_guard;

    print_devices("Audio Output Devices", &devices, default_name.as_deref());
    println!();
    print_devices("Audio Input Devices", &inputs, default_input.as_deref());
    Ok(())
}

/// One list of audio devices as `tang enumerate audio` prints it: the default marked, and each with its channels, rate and sample format.
fn print_devices(title: &str, devices: &[(String, cpal::SupportedStreamConfig)], default: Option<&str>) {
    println!("=== {title} ===");
    if devices.is_empty() {
        println!("  (none found)");
        return;
    }
    for (name, config) in devices {
        let is_default = default == Some(name.as_str());
        let marker = if is_default { " *" } else { "" };
        println!(
            "  {name}{marker}  ({ch}ch, {rate}Hz, {fmt})",
            ch = config.channels(),
            rate = config.sample_rate().0,
            fmt = format_sample_fmt(config.sample_format()),
        );
    }
}

fn format_sample_fmt(fmt: cpal::SampleFormat) -> &'static str {
    match fmt {
        cpal::SampleFormat::I8 => "i8",
        cpal::SampleFormat::I16 => "i16",
        cpal::SampleFormat::I32 => "i32",
        cpal::SampleFormat::I64 => "i64",
        cpal::SampleFormat::U8 => "u8",
        cpal::SampleFormat::U16 => "u16",
        cpal::SampleFormat::U32 => "u32",
        cpal::SampleFormat::U64 => "u64",
        cpal::SampleFormat::F32 => "f32",
        cpal::SampleFormat::F64 => "f64",
        _ => "?",
    }
}

pub fn builtins() -> anyhow::Result<()> {
    println!("=== Built-in Plugins ===");
    let plugins = builtin::enumerate_plugins();
    if plugins.is_empty() {
        println!("  (none)");
    }
    for p in &plugins {
        let kind = if p.is_instrument {
            "instrument"
        } else {
            "effect"
        };
        println!("  [{kind}] {} ({} ms)", p.name, p.scan_ms);
        println!("          ID:      {}", p.id);
        println!("          Params:  {}", p.param_count);
        println!("          Presets: {}", p.preset_count);
    }
    Ok(())
}

pub fn plugins() -> anyhow::Result<()> {
    #[cfg(feature = "lv2")]
    {
        println!("=== LV2 Plugins ===");
        let plugins = lv2::enumerate_plugins();
        if plugins.is_empty() {
            println!("  (none found)");
        }
        for p in &plugins {
            let kind = if p.is_instrument {
                "instrument"
            } else {
                "effect"
            };
            println!("  [{kind}] {} ({} ms)", p.name, p.scan_ms);
            println!("          URI:     {}", p.id);
            println!("          Path:    {}", p.path);
            println!("          Params:  {}", p.param_count);
            println!("          Presets: {}", p.preset_count);
        }
        println!();
    }
    #[cfg(not(feature = "lv2"))]
    {
        println!("=== LV2 Plugins ===");
        println!("  (LV2 support not enabled)");
        println!();
    }

    println!("=== CLAP Plugins ===");
    let claps = clap::enumerate_plugins();
    if claps.is_empty() {
        println!("  (none found)");
    }
    for p in &claps {
        let kind = if p.is_instrument {
            "instrument"
        } else {
            "effect"
        };
        println!("  [{kind}] {} ({} ms)", p.name, p.scan_ms);
        println!("          ID:      {}", p.id);
        println!("          Path:    {}", p.path);
        println!("          Params:  {}", p.param_count);
        println!("          Presets: {}", p.preset_count);
    }
    println!();

    #[cfg(feature = "vst3")]
    {
        let vst3s = vst3::enumerate_plugins();
        println!("=== VST3 Plugins ===");
        if vst3s.is_empty() {
            println!("  (none found)");
        }
        for p in &vst3s {
            let kind = if p.is_instrument {
                "instrument"
            } else {
                "effect"
            };
            println!("  [{kind}] {} ({} ms)", p.name, p.scan_ms);
            println!("          ID:      {}", p.id);
            println!("          Path:    {}", p.path);
            println!("          Params:  {}", p.param_count);
            println!("          Presets: {}", p.preset_count);
        }
    }
    #[cfg(not(feature = "vst3"))]
    {
        println!("=== VST3 Plugins ===");
        println!("  (VST3 support not enabled)");
    }

    Ok(())
}
