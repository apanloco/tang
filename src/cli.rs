use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tang", about = "Minimal CLI LV2/CLAP instrument host")]
pub struct Cli {
    /// Optional session file (launches TUI)
    pub session: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// List available MIDI inputs, audio outputs, and plugins
    #[command(subcommand)]
    Enumerate(EnumerateTarget),
    /// Describe a plugin (parameters, presets, I/O)
    Describe {
        /// Plugin source (lv2:<URI>, clap:<ID>, or path)
        plugin: String,
    },
    /// Load a session and play via MIDI input with virtual piano
    Play(PlayArgs),
}

#[derive(Subcommand)]
pub enum EnumerateTarget {
    /// List available MIDI input devices
    Midi,
    /// List available audio output devices
    Audio,
    /// List available LV2 and CLAP plugins
    Plugins,
    /// List built-in plugins
    Builtins,
}

#[derive(clap::Args)]
pub struct PlayArgs {
    /// Path to session file (.toml)
    pub session: String,

    /// Audio output device name (default: system default)
    #[arg(long)]
    pub audio_device: Option<String>,

    /// MIDI input device name filter (default: open all)
    #[arg(long)]
    pub midi_device: Option<String>,

    /// Audio buffer size in frames
    #[arg(long, default_value = "512")]
    pub buffer_size: u32,

    /// Sample rate in Hz
    #[arg(long, default_value = "48000")]
    pub sample_rate: u32,
}
