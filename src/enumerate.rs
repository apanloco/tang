use crate::plugin::builtin;
use crate::plugin::clap;
#[cfg(feature = "lv2")]
use crate::plugin::lv2;

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
    let stderr_guard = suppress_stderr();

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

    let _ = stderr_guard;

    println!("=== Audio Output Devices ===");
    if devices.is_empty() {
        println!("  (none found)");
        return Ok(());
    }
    for (name, config) in &devices {
        let is_default = default_name.as_deref() == Some(name.as_str());
        let marker = if is_default { " *" } else { "" };
        println!(
            "  {name}{marker}  ({ch}ch, {rate}Hz, {fmt})",
            ch = config.channels(),
            rate = config.sample_rate().0,
            fmt = format_sample_fmt(config.sample_format()),
        );
    }
    Ok(())
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

/// Temporarily redirect stderr to /dev/null to suppress ALSA/JACK library spam.
/// Returns a guard that restores stderr on drop. No-op on non-Unix platforms.
#[cfg(unix)]
fn suppress_stderr() -> Option<StderrGuard> {
    use std::os::unix::io::AsRawFd;
    let devnull = std::fs::File::open("/dev/null").ok()?;
    let stderr_fd = std::io::stderr().as_raw_fd();
    let saved = unsafe { libc::dup(stderr_fd) };
    if saved < 0 {
        return None;
    }
    unsafe { libc::dup2(devnull.as_raw_fd(), stderr_fd) };
    Some(StderrGuard { saved_fd: saved })
}

#[cfg(unix)]
struct StderrGuard {
    saved_fd: i32,
}

#[cfg(unix)]
impl Drop for StderrGuard {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        let stderr_fd = std::io::stderr().as_raw_fd();
        unsafe {
            libc::dup2(self.saved_fd, stderr_fd);
            libc::close(self.saved_fd);
        }
    }
}

#[cfg(not(unix))]
fn suppress_stderr() -> Option<()> {
    None
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
        println!("  [{kind}] {}", p.name);
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
            println!("  [{kind}] {}", p.name);
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
        println!("  [{kind}] {}", p.name);
        println!("          ID:      {}", p.id);
        println!("          Path:    {}", p.path);
        println!("          Params:  {}", p.param_count);
        println!("          Presets: {}", p.preset_count);
    }
    Ok(())
}
