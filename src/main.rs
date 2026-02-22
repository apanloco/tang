#![allow(clippy::collapsible_if)]

mod audio;
mod cli;
mod enumerate;
mod midi;
mod piano;
mod plugin;
mod session;
mod tui;

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use clap::Parser;
use cli::{Cli, Command, EnumerateTarget, PlayArgs};
use crossterm::event::{
    self, Event, KeyCode, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            let session = cli.session;
            todo!("TUI not yet implemented (session: {session:?})");
        }
        Some(Command::Enumerate(target)) => {
            env_logger::init();
            match target {
                EnumerateTarget::Midi => enumerate::midi(),
                EnumerateTarget::Audio => enumerate::audio(),
                EnumerateTarget::Plugins => enumerate::plugins(),
                EnumerateTarget::Builtins => enumerate::builtins(),
            }
        }
        Some(Command::Describe { plugin: source }) => {
            env_logger::init();
            let p = plugin::load(&source, 48000.0, 512, &plugin::Runtime::default())?;
            println!("{}", p.name());
            println!(
                "  Type:          {}",
                if p.is_instrument() {
                    "instrument"
                } else {
                    "effect"
                }
            );
            println!("  Audio outputs: {}", p.audio_output_count());
            let params = p.parameters();
            println!("  Parameters:    {}", params.len());
            for param in &params {
                println!(
                    "    [{}] {} (min={}, max={}, default={})",
                    param.index, param.name, param.min, param.max, param.default
                );
            }
            let presets = p.presets();
            if presets.is_empty() {
                println!("  Presets:       (none)");
            } else {
                println!("  Presets:       {}", presets.len());
                for preset in &presets {
                    println!("    {} ({})", preset.name, preset.id);
                }
            }
            Ok(())
        }
        Some(Command::Play(args)) => play(args),
    }
}

/// Custom logger that writes to stderr with \r\n line endings for raw mode.
struct RawModeLogger;

impl log::Log for RawModeLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            let secs = now.as_secs() % 86400; // time of day
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            let s = secs % 60;
            let ms = now.subsec_millis();
            let _ = write!(
                std::io::stderr(),
                "[{h:02}:{m:02}:{s:02}.{ms:03} {}] {}\r\n",
                record.level(),
                record.args()
            );
        }
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

static RAW_MODE_LOGGER: RawModeLogger = RawModeLogger;

fn play(args: PlayArgs) -> anyhow::Result<()> {
    // Set up raw mode logger early so plugin loading messages are visible
    log::set_logger(&RAW_MODE_LOGGER).ok();
    log::set_max_level(
        std::env::var("RUST_LOG")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(log::LevelFilter::Info),
    );

    let source = &args.session;
    let sample_rate = args.sample_rate as f32;
    let max_block_size = args.buffer_size as usize;

    // Load session config
    let config = session::load(source)?;

    let session_dir = Path::new(source).parent().unwrap_or_else(|| Path::new("."));

    // Create shared LV2 world (scans system plugins once, reused for all LV2 loads)
    #[cfg(feature = "lv2")]
    let runtime = plugin::Runtime::with_lv2(max_block_size);
    #[cfg(not(feature = "lv2"))]
    let runtime = plugin::Runtime::default();

    // Create channels
    let (midi_tx, midi_rx) = crossbeam_channel::bounded::<audio::MidiEvent>(1024);
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<plugin::chain::ChainCommand>(64);
    let (return_tx, return_rx) = crossbeam_channel::bounded::<Box<dyn plugin::Plugin>>(16);

    // Create empty plugin chain (outputs silence until instrument is swapped in)
    let num_channels = 2; // stereo — see CLAUDE.md design decision
    let chain = plugin::chain::PluginChain::new(num_channels, cmd_rx, return_tx);

    // Start MIDI input
    let mut midi_mgr = midi::MidiManager::new(midi_tx.clone(), args.midi_device.clone());
    midi_mgr.open_ports()?;
    log::info!("MIDI inputs connected: {}", midi_mgr.connection_count());

    // Start audio engine (silent — no instrument yet)
    let engine = audio::AudioEngine::start(
        chain,
        midi_rx,
        args.audio_device.as_deref(),
        args.sample_rate,
        args.buffer_size,
    )?;

    // Load instrument on main thread, apply preset, then send to audio thread
    let instrument_source = session::resolve_plugin_path(&config.instrument.plugin, session_dir);
    let mut instrument = plugin::load(&instrument_source, sample_rate, max_block_size, &runtime)?;
    log::info!("Loaded instrument: {}", instrument.name());

    if let Some(ref preset_name) = config.instrument.preset {
        session::apply_preset(&mut instrument, preset_name);
    }

    // Query parameter info for name→index mapping before sending to audio thread
    let inst_params = instrument.parameters();
    let inst_buf = (0..instrument.audio_output_count())
        .map(|_| Vec::new())
        .collect();
    cmd_tx
        .send(plugin::chain::ChainCommand::SwapInstrument {
            instrument,
            inst_buf,
        })
        .map_err(|_| anyhow::anyhow!("command channel closed"))?;

    // Send instrument parameter overrides as commands
    for (name, &value) in &config.instrument.params {
        if let Some(info) = inst_params.iter().find(|p| p.name == *name) {
            cmd_tx
                .send(plugin::chain::ChainCommand::SetParameter {
                    slot: 0,
                    param_index: info.index,
                    value: value as f32,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;
            log::info!("Set instrument '{}' = {}", name, value);
        } else {
            log::warn!(
                "Unknown instrument parameter '{}' (available: {})",
                name,
                inst_params
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    // Load each effect on main thread, apply preset, send InsertEffect + SetParameter commands
    for (i, effect_config) in config.effects.iter().enumerate() {
        let effect_source = session::resolve_plugin_path(&effect_config.plugin, session_dir);
        let mut effect = plugin::load(&effect_source, sample_rate, max_block_size, &runtime)?;
        log::info!("Loaded effect {}: {}", i, effect.name());

        if let Some(ref preset_name) = effect_config.preset {
            session::apply_preset(&mut effect, preset_name);
        }

        let effect_params = effect.parameters();

        cmd_tx
            .send(plugin::chain::ChainCommand::InsertEffect {
                index: i,
                effect,
                mix: effect_config.mix,
            })
            .map_err(|_| anyhow::anyhow!("command channel closed"))?;

        // Send parameter overrides for this effect (slot = i + 1)
        for (name, &value) in &effect_config.params {
            if let Some(info) = effect_params.iter().find(|p| p.name == *name) {
                cmd_tx
                    .send(plugin::chain::ChainCommand::SetParameter {
                        slot: i + 1,
                        param_index: info.index,
                        value: value as f32,
                    })
                    .map_err(|_| anyhow::anyhow!("command channel closed"))?;
                log::info!("Set effect {} '{}' = {}", i, name, value);
            } else {
                log::warn!(
                    "Unknown parameter '{}' for effect {} (available: {})",
                    name,
                    i,
                    effect_params
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }

    // Probe keyboard enhancement support (must be done before entering raw mode)
    let kitty_supported = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);

    // Enter raw mode
    crossterm::terminal::enable_raw_mode()?;

    // Push Kitty keyboard flags if supported
    if kitty_supported {
        crossterm::execute!(
            std::io::stderr(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            )
        )?;
        log::info!("Kitty keyboard protocol enabled (press/release detection active)");
    } else {
        log::warn!(
            "Terminal does not support Kitty keyboard protocol — virtual piano disabled (hardware MIDI still works)"
        );
    }

    // Create virtual piano
    let mut virt_piano = piano::VirtualPiano::new(midi_tx, kitty_supported);

    log::info!("Playing. Ctrl+Q or Ctrl+C to quit.");

    let mut last_poll = Instant::now();

    loop {
        // Poll crossterm events with 10ms timeout
        if event::poll(Duration::from_millis(10))? {
            if let Event::Key(key_event) = event::read()? {
                // Ctrl+C or Ctrl+Q → quit
                if key_event
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL)
                {
                    match key_event.code {
                        KeyCode::Char('c') | KeyCode::Char('q') => break,
                        _ => {}
                    }
                }
                // Pass to virtual piano
                virt_piano.handle_key_event(key_event);
            }
        }

        // Drain returned plugins so they are dropped on the main thread
        while return_rx.try_recv().is_ok() {}

        // Poll for new MIDI devices every ~1s
        if last_poll.elapsed() >= Duration::from_secs(1) {
            midi_mgr.poll_new_devices();
            last_poll = Instant::now();
        }
    }

    // Cleanup
    virt_piano.all_notes_off();

    if kitty_supported {
        crossterm::execute!(std::io::stderr(), PopKeyboardEnhancementFlags).ok();
    }
    crossterm::terminal::disable_raw_mode()?;

    log::info!("Stopping...");

    // Shutdown order matters: stop audio first (so callback can't call plugin),
    // then drop MIDI connections, then returned plugins are dropped last.
    engine.stop();
    drop(midi_mgr);

    // Drain any remaining returned plugins
    while return_rx.try_recv().is_ok() {}

    Ok(())
}
