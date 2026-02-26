#![allow(clippy::collapsible_if)]

mod audio;
mod cli;
mod config;
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

/// Convert a MIDI note number to a human-readable name (e.g. 60 → "C4").
pub fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let octave = (note / 12) as i8 - 1;
    let name = NAMES[(note % 12) as usize];
    format!("{name}{octave}")
}
use crossterm::event::{
    self, Event, KeyCode, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Load application config (extra plugin paths, etc.)
    if let Ok(config_dir) = dirs_config() {
        let config_path = config_dir.join("config.toml");
        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(text) => match toml::from_str::<config::Config>(&text) {
                    Ok(cfg) => config::init(cfg),
                    Err(e) => eprintln!("Warning: failed to parse {}: {e}", config_path.display()),
                },
                Err(e) => eprintln!("Warning: failed to read {}: {e}", config_path.display()),
            }
        }
    }

    // Set LV2_PATH for extra LV2 search directories (before any LV2 world is created)
    let extra_lv2 = config::extra_lv2_paths();
    if !extra_lv2.is_empty() {
        let current = std::env::var("LV2_PATH").unwrap_or_default();
        let extra: String = extra_lv2
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(":");
        let new = if current.is_empty() {
            extra
        } else {
            format!("{extra}:{current}")
        };
        // Safety: called before any threads are spawned.
        unsafe {
            std::env::set_var("LV2_PATH", new);
        }
    }

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

/// Create a default in-memory session config and a path for saving later.
/// The file is NOT written to disk — only created on Ctrl+S.
fn default_session() -> anyhow::Result<(session::SessionConfig, std::path::PathBuf)> {
    let dir = dirs_config_sessions()?;

    // Generate a short unique ID from the current timestamp.
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let id = format!("{:x}", ts.as_millis());
    let path = dir.join(format!("session-{id}.toml"));

    let config = session::SessionConfig {
        keyboards: vec![session::KeyboardConfig {
            name: None,
            splits: vec![session::SplitConfig {
                range: None,
                transpose: 0,
                instrument: Some(session::PluginConfig {
                    plugin: "builtin:sine".into(),
                    preset: None,
                    volume: 1.0,
                    pitch_bend_range: 2.0,
                    remap: Default::default(),
                    params: Default::default(),
                    modulators: vec![],
                }),
                effects: vec![],
                pattern: None,
            }],
        }],
    };

    log::info!("New session (will save to {} on Ctrl+S)", path.display());
    Ok((config, path))
}

fn dirs_config_sessions() -> anyhow::Result<std::path::PathBuf> {
    let config = dirs_config()?;
    Ok(config.join("sessions"))
}

fn dirs_config() -> anyhow::Result<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(Path::new(&home).join(".config").join("tang"));
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
            return Ok(Path::new(&config).join("tang"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(Path::new(&home).join(".config").join("tang"));
        }
    }
    anyhow::bail!("could not determine config directory")
}

/// Load modulators from a plugin's config and send the commands to the audio thread.
/// Returns the loaded modulators for the TUI model.
fn load_modulators(
    mod_configs: &[session::ModulatorConfig],
    parent_slot: usize,
    parent_params: &[plugin::ParameterInfo],
    kb_idx: usize,
    sp_idx: usize,
    cmd_tx: &crossbeam_channel::Sender<plugin::chain::GraphCommand>,
) -> anyhow::Result<Vec<tui::LoadedModulator>> {
    let mut loaded = Vec::new();
    for (mod_idx, mod_config) in mod_configs.iter().enumerate() {
        let (source, loaded_source, desc) = match mod_config.mod_type.as_str() {
            "envelope" => {
                let source = plugin::chain::ModSource::Envelope {
                    attack: mod_config.attack as f32,
                    decay: mod_config.decay as f32,
                    sustain: mod_config.sustain as f32,
                    release: mod_config.release as f32,
                    state: plugin::chain::EnvState::Idle,
                    level: 0.0,
                    notes_held: 0,
                };
                let loaded_source = tui::LoadedModSource::Envelope {
                    attack: mod_config.attack as f32,
                    decay: mod_config.decay as f32,
                    sustain: mod_config.sustain as f32,
                    release: mod_config.release as f32,
                };
                (source, loaded_source, "ADSR envelope".to_string())
            }
            _ => {
                // Default: LFO.
                let waveform = plugin::chain::LfoWaveform::from_str(&mod_config.waveform)
                    .unwrap_or_else(|| {
                        log::warn!(
                            "Unknown waveform '{}', defaulting to sine",
                            mod_config.waveform
                        );
                        plugin::chain::LfoWaveform::Sine
                    });
                let source = plugin::chain::ModSource::Lfo {
                    waveform,
                    rate: mod_config.rate as f32,
                    phase: 0.0,
                };
                let loaded_source = tui::LoadedModSource::Lfo {
                    waveform,
                    rate: mod_config.rate as f32,
                };
                let desc = format!("{} {:.1}Hz", waveform.name(), mod_config.rate);
                (source, loaded_source, desc)
            }
        };

        cmd_tx
            .send(plugin::chain::GraphCommand::InsertModulator {
                kb: kb_idx,
                split: sp_idx,
                parent_slot,
                index: mod_idx,
                source,
            })
            .map_err(|_| anyhow::anyhow!("command channel closed"))?;

        let mut loaded_targets: Vec<tui::LoadedModTarget> = Vec::new();
        for target_config in &mod_config.targets {
            // Determine the target kind and associated metadata.
            let (kind, label, param_min, param_max, base_value) =
                if let Some(ref param_name) = target_config.param {
                    // Plugin parameter target.
                    let param_info = parent_params.iter().find(|p| p.name == *param_name);
                    let param_info = match param_info {
                        Some(p) => p,
                        None => {
                            log::warn!(
                                "Modulator target param '{}' not found in parent slot {}",
                                param_name,
                                parent_slot,
                            );
                            continue;
                        }
                    };
                    (
                        plugin::chain::ModTargetKind::PluginParam { param_index: param_info.index },
                        param_info.name.clone(),
                        param_info.min,
                        param_info.max,
                        param_info.default,
                    )
                } else if let Some(mi) = target_config.mod_rate {
                    (plugin::chain::ModTargetKind::ModulatorRate { mod_index: mi },
                     format!("Mod {} rate", mi), 0.01, 50.0, 1.0)
                } else if let Some(ref pair) = target_config.mod_depth {
                    let (mi, ti) = (pair.first().copied().unwrap_or(0), pair.get(1).copied().unwrap_or(0));
                    (plugin::chain::ModTargetKind::ModulatorDepth { mod_index: mi, target_index: ti },
                     format!("Mod {} depth {}", mi, ti), 0.0, 1.0, 0.5)
                } else if let Some(mi) = target_config.mod_attack {
                    (plugin::chain::ModTargetKind::ModulatorAttack { mod_index: mi },
                     format!("Mod {} attack", mi), 0.001, 10.0, 0.01)
                } else if let Some(mi) = target_config.mod_decay {
                    (plugin::chain::ModTargetKind::ModulatorDecay { mod_index: mi },
                     format!("Mod {} decay", mi), 0.001, 10.0, 0.3)
                } else if let Some(mi) = target_config.mod_sustain {
                    (plugin::chain::ModTargetKind::ModulatorSustain { mod_index: mi },
                     format!("Mod {} sustain", mi), 0.0, 1.0, 0.7)
                } else if let Some(mi) = target_config.mod_release {
                    (plugin::chain::ModTargetKind::ModulatorRelease { mod_index: mi },
                     format!("Mod {} release", mi), 0.001, 10.0, 0.5)
                } else {
                    log::warn!("Modulator target has no param or mod_* field, skipping");
                    continue;
                };

            let target = plugin::chain::ModTarget {
                kind: kind.clone(),
                depth: target_config.depth as f32,
                base_value,
                param_min,
                param_max,
            };

            cmd_tx
                .send(plugin::chain::GraphCommand::AddModTarget {
                    kb: kb_idx,
                    split: sp_idx,
                    parent_slot,
                    mod_index: mod_idx,
                    target,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;

            loaded_targets.push(tui::LoadedModTarget {
                param_name: label.clone(),
                param_index: match &kind {
                    plugin::chain::ModTargetKind::PluginParam { param_index } => *param_index,
                    _ => 0,
                },
                depth: target_config.depth as f32,
                param_min,
                param_max,
            });

            log::info!(
                "Modulator {} target: '{}' depth={}",
                mod_idx,
                label,
                target_config.depth,
            );
        }

        loaded.push(tui::LoadedModulator {
            source: loaded_source,
            targets: loaded_targets,
        });

        log::info!(
            "Loaded modulator {} for kb={} split={} slot={}: {}",
            mod_idx,
            kb_idx,
            sp_idx,
            parent_slot,
            desc,
        );
    }
    Ok(loaded)
}

fn play(args: PlayArgs) -> anyhow::Result<()> {
    // Set up raw mode logger early so plugin loading messages are visible
    log::set_logger(&RAW_MODE_LOGGER).ok();
    log::set_max_level(
        std::env::var("RUST_LOG")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(log::LevelFilter::Info),
    );

    let sample_rate = args.sample_rate as f32;
    let max_block_size = args.buffer_size as usize;

    // Load or create session config.
    let (config, source) = match args.session {
        Some(s) => {
            let config = session::load(&s)?;
            (config, s)
        }
        None => {
            let (config, path) = default_session()?;
            (config, path.to_string_lossy().to_string())
        }
    };

    let session_dir = Path::new(&source).parent().unwrap_or_else(|| Path::new("."));

    // Create shared LV2 world (scans system plugins once, reused for all LV2 loads)
    #[cfg(feature = "lv2")]
    let runtime = plugin::Runtime::with_lv2(max_block_size);
    #[cfg(not(feature = "lv2"))]
    let runtime = plugin::Runtime::default();

    // Create channels
    let (midi_tx, midi_rx) = crossbeam_channel::bounded::<audio::MidiEvent>(1024);
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<plugin::chain::GraphCommand>(64);
    let (return_tx, return_rx) = crossbeam_channel::bounded::<Box<dyn plugin::Plugin>>(16);

    // Create empty audio graph (outputs silence until instruments are added)
    let num_channels = 2; // stereo — see CLAUDE.md design decision
    let mut graph = plugin::chain::AudioGraph::new(num_channels, cmd_rx, return_tx);

    // Pattern recording completion channel
    let (pattern_tx, pattern_rx) = crossbeam_channel::bounded::<plugin::chain::PatternNotification>(64);
    graph.set_pattern_tx(pattern_tx.clone());

    // Start MIDI input
    let mut midi_mgr = midi::MidiManager::new(midi_tx.clone(), args.midi_device.clone());
    midi_mgr.open_ports()?;
    log::info!("MIDI inputs connected: {}", midi_mgr.connection_count());

    // Start audio engine (silent — no instruments yet)
    let engine = audio::AudioEngine::start(
        graph,
        midi_rx,
        args.audio_device.as_deref(),
        args.sample_rate,
        args.buffer_size,
    )?;

    // Build TUI metadata while loading plugins into the graph.
    let mut loaded_keyboards: Vec<tui::LoadedKeyboard> = Vec::new();

    // Set up the graph structure first: add keyboards and splits
    for (kb_idx, kb_config) in config.keyboards.iter().enumerate() {
        cmd_tx
            .send(plugin::chain::GraphCommand::AddKeyboard)
            .map_err(|_| anyhow::anyhow!("command channel closed"))?;

        let mut loaded_splits: Vec<tui::LoadedSplit> = Vec::new();

        for (sp_idx, sp_config) in kb_config.splits.iter().enumerate() {
            cmd_tx
                .send(plugin::chain::GraphCommand::AddSplit {
                    kb: kb_idx,
                    range: sp_config.range,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;

            // Load instrument (if present)
            let loaded_instrument = if let Some(inst_config) = &sp_config.instrument {
                let instrument_source =
                    session::resolve_plugin_path(&inst_config.plugin, session_dir);
                let mut instrument =
                    plugin::load(&instrument_source, sample_rate, max_block_size, &runtime)?;
                log::info!(
                    "Loaded instrument for kb={} split={}: {}",
                    kb_idx,
                    sp_idx,
                    instrument.name()
                );

                if let Some(ref preset_name) = inst_config.preset {
                    session::apply_preset(&mut instrument, preset_name);
                }

                // Build note remapper if configured
                let remapper = if inst_config.remap.is_empty() {
                    None
                } else {
                    let r = plugin::chain::NoteRemapper::from_config(
                        &inst_config.remap,
                        inst_config.pitch_bend_range,
                    )?;
                    log::info!(
                        "Note remapper: {} entries, pitch_bend_range=±{}",
                        inst_config.remap.len(),
                        inst_config.pitch_bend_range,
                    );
                    Some(r)
                };

                let inst_params = instrument.parameters();
                let inst_name = instrument.name().to_string();
                let inst_buf = (0..instrument.audio_output_count())
                    .map(|_| Vec::new())
                    .collect();
                cmd_tx
                    .send(plugin::chain::GraphCommand::SwapInstrument {
                        kb: kb_idx,
                        split: sp_idx,
                        instrument,
                        inst_buf,
                        remapper,
                    })
                    .map_err(|_| anyhow::anyhow!("command channel closed"))?;

                // Set volume if not default
                if (inst_config.volume - 1.0).abs() > f64::EPSILON {
                    cmd_tx
                        .send(plugin::chain::GraphCommand::SetVolume {
                            kb: kb_idx,
                            split: sp_idx,
                            value: inst_config.volume as f32,
                        })
                        .map_err(|_| anyhow::anyhow!("command channel closed"))?;
                }

                // Send instrument parameter overrides
                let mut inst_values: Vec<f32> = inst_params.iter().map(|p| p.default).collect();
                for (name, &value) in &inst_config.params {
                    if let Some(info) = inst_params.iter().find(|p| p.name == *name) {
                        cmd_tx
                            .send(plugin::chain::GraphCommand::SetParameter {
                                kb: kb_idx,
                                split: sp_idx,
                                slot: 0,
                                param_index: info.index,
                                value: value as f32,
                            })
                            .map_err(|_| anyhow::anyhow!("command channel closed"))?;
                        if let Some(v) = inst_values.get_mut(info.index as usize) {
                            *v = value as f32;
                        }
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

                // Load instrument modulators
                let inst_mods = load_modulators(
                    &inst_config.modulators,
                    0, // parent_slot = instrument
                    &inst_params,
                    kb_idx,
                    sp_idx,
                    &cmd_tx,
                )?;

                Some(tui::LoadedPlugin {
                    name: inst_name,
                    id: instrument_source,
                    is_instrument: true,
                    params: inst_params,
                    param_values: inst_values,
                    modulators: inst_mods,
                })
            } else {
                None
            };

            // Load effects
            let mut loaded_effects: Vec<tui::LoadedPlugin> = Vec::new();
            for (fx_idx, effect_config) in sp_config.effects.iter().enumerate() {
                let effect_source =
                    session::resolve_plugin_path(&effect_config.plugin, session_dir);
                let mut effect =
                    plugin::load(&effect_source, sample_rate, max_block_size, &runtime)?;
                log::info!(
                    "Loaded effect for kb={} split={} fx={}: {}",
                    kb_idx,
                    sp_idx,
                    fx_idx,
                    effect.name()
                );

                if let Some(ref preset_name) = effect_config.preset {
                    session::apply_preset(&mut effect, preset_name);
                }

                let effect_params = effect.parameters();
                let effect_name = effect.name().to_string();

                cmd_tx
                    .send(plugin::chain::GraphCommand::InsertEffect {
                        kb: kb_idx,
                        split: sp_idx,
                        index: fx_idx,
                        effect,
                        mix: effect_config.mix,
                    })
                    .map_err(|_| anyhow::anyhow!("command channel closed"))?;

                // Send parameter overrides for this effect (slot = fx_idx + 1)
                let mut fx_values: Vec<f32> = effect_params.iter().map(|p| p.default).collect();
                for (name, &value) in &effect_config.params {
                    if let Some(info) = effect_params.iter().find(|p| p.name == *name) {
                        cmd_tx
                            .send(plugin::chain::GraphCommand::SetParameter {
                                kb: kb_idx,
                                split: sp_idx,
                                slot: fx_idx + 1,
                                param_index: info.index,
                                value: value as f32,
                            })
                            .map_err(|_| anyhow::anyhow!("command channel closed"))?;
                        if let Some(v) = fx_values.get_mut(info.index as usize) {
                            *v = value as f32;
                        }
                        log::info!("Set effect {} '{}' = {}", fx_idx, name, value);
                    } else {
                        log::warn!(
                            "Unknown parameter '{}' for effect {} (available: {})",
                            name,
                            fx_idx,
                            effect_params
                                .iter()
                                .map(|p| p.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                }

                // Load effect modulators
                let fx_mods = load_modulators(
                    &effect_config.modulators,
                    fx_idx + 1, // parent_slot for effects
                    &effect_params,
                    kb_idx,
                    sp_idx,
                    &cmd_tx,
                )?;

                loaded_effects.push(tui::LoadedPlugin {
                    name: effect_name,
                    id: effect_source,
                    is_instrument: false,
                    params: effect_params,
                    param_values: fx_values,
                    modulators: fx_mods,
                });
            }

            // Load pattern if configured.
            let loaded_pattern = sp_config.pattern.as_ref().map(|p| {
                // Build Pattern and send to audio graph.
                let pattern_events: Vec<crate::plugin::chain::PatternEvent> = p.events.iter().map(|&(frame, status, note, vel)| {
                    crate::plugin::chain::PatternEvent {
                        frame,
                        status,
                        note,
                        velocity: vel,
                    }
                }).collect();
                let beats_per_sec = p.bpm / 60.0;
                let length_samples = (p.length_beats / beats_per_sec * sample_rate) as u64;
                let pattern = crate::plugin::chain::Pattern {
                    events: pattern_events,
                    length_samples,
                };
                let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPattern {
                    kb: kb_idx,
                    split: sp_idx,
                    pattern,
                    base_note: p.base_note,
                });
                let _ = cmd_tx.send(plugin::chain::GraphCommand::SetGlobalBpm { bpm: p.bpm });
                let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPatternLength {
                    kb: kb_idx,
                    split: sp_idx,
                    beats: p.length_beats,
                });
                if p.enabled {
                    let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPatternEnabled {
                        kb: kb_idx,
                        split: sp_idx,
                        enabled: true,
                    });
                }
                if !p.looping {
                    let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPatternLooping {
                        kb: kb_idx,
                        split: sp_idx,
                        looping: false,
                    });
                }
                tui::LoadedPattern {
                    bpm: p.bpm,
                    length_beats: p.length_beats,
                    looping: p.looping,
                    base_note: p.base_note,
                    events: p.events.clone(),
                    enabled: p.enabled,
                }
            });

            loaded_splits.push(tui::LoadedSplit {
                range: sp_config.range,
                transpose: sp_config.transpose,
                instrument: loaded_instrument,
                effects: loaded_effects,
                pattern: loaded_pattern,
            });
        }

        loaded_keyboards.push(tui::LoadedKeyboard {
            name: kb_config
                .name
                .clone()
                .unwrap_or_else(|| format!("Keyboard {}", kb_idx + 1)),
            splits: loaded_splits,
        });
    }

    // --- Branch: TUI view vs plain play mode ---
    if args.view {
        let session_path = Some(std::path::PathBuf::from(source));
        tui::run(loaded_keyboards, cmd_tx, midi_tx, runtime, sample_rate, max_block_size, session_path, pattern_rx)?;
    } else {
        // --- Plain play mode (original) ---

        // Probe keyboard enhancement support (must be done before entering raw mode)
        let kitty_supported =
            crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);

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
    }

    log::info!("Stopping...");

    // Shutdown order matters: stop audio first (so callback can't call plugin),
    // then drop MIDI connections, then returned plugins are dropped last.
    engine.stop();
    drop(midi_mgr);

    // Drain any remaining returned plugins
    while return_rx.try_recv().is_ok() {}

    Ok(())
}
