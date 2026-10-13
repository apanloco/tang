use clap::{Parser, Subcommand};

/// Default audio buffer size in frames.
///
/// On Linux, audio goes through PipeWire's JACK bridge, which runs every client
/// on one driver graph at a single global quantum equal to the *smallest* latency
/// any active client requests. A small buffer here would drag the whole system's
/// quantum down and make other apps (YouTube, etc.) chop. 1024 matches PipeWire's
/// typical default global quantum, so Tang doesn't disturb anything else; pass
/// `--buffer-size` to opt into lower latency. macOS (CoreAudio) and Windows
/// (WASAPI) don't share a global quantum, so they keep a low-latency default.
#[cfg(target_os = "linux")]
pub const DEFAULT_BUFFER_SIZE: &str = "1024";
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_BUFFER_SIZE: &str = "256";

// The flags are `global`: they are the same wherever they are written, before the
// subcommand or after it (`tang play x.toml --audio-device none`).
#[derive(Parser)]
#[command(name = "tang", about = "Minimal CLI LV2/CLAP instrument host")]
pub struct Cli {
    /// Session file path (.toml). Defaults to a new session.
    pub session: Option<String>,

    /// Audio output device name (default: system default). `none` runs the
    /// engine on a timer without a sound card, for CI and headless use.
    #[arg(long, global = true)]
    pub audio_device: Option<String>,

    /// MIDI input device name filter (default: open all)
    #[arg(long, global = true)]
    pub midi_device: Option<String>,

    /// Audio buffer size in frames
    #[arg(long, default_value = DEFAULT_BUFFER_SIZE, global = true)]
    pub buffer_size: u32,

    /// Sample rate in Hz
    #[arg(long, default_value = "48000", global = true)]
    pub sample_rate: u32,

    /// Show a live incoming-MIDI monitor at the bottom of the TUI (for
    /// diagnosing what a controller actually sends).
    #[arg(long, global = true)]
    pub debug: bool,

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
    /// Load a session and play via MIDI input with virtual piano (no TUI)
    Play {
        /// Path to session file (.toml). If omitted, creates a new session.
        session: Option<String>,
    },
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every way of writing the same thing: the flags before the subcommand (where
    /// they always worked) and after it, or among its arguments.
    fn placements(subcommand: &[&str], flags: &[&str]) -> Vec<Vec<String>> {
        let owned = |parts: &[&[&str]]| parts.iter().flat_map(|p| p.iter()).map(|s| s.to_string()).collect::<Vec<String>>();
        vec![
            owned(&[&["tang"], flags, subcommand]),
            owned(&[&["tang"], subcommand, flags]),
            owned(&[&["tang"], &subcommand[..1], flags, &subcommand[1..]]),
        ]
    }

    #[test]
    fn the_flags_of_the_session_commands_work_wherever_they_are_written() {
        let flags = ["--audio-device", "none", "--midi-device", "keys", "--buffer-size", "128", "--sample-rate", "44100", "--debug"];
        for subcommand in [["play", "x.toml"]] {
            for args in placements(&subcommand, &flags) {
                let cli = Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
                assert_eq!(cli.audio_device.as_deref(), Some("none"), "{args:?}");
                assert_eq!(cli.midi_device.as_deref(), Some("keys"), "{args:?}");
                assert_eq!((cli.buffer_size, cli.sample_rate, cli.debug), (128, 44100, true), "{args:?}");
            }
        }
    }

    #[test]
    fn the_flags_are_as_before_when_they_are_not_given() {
        let cli = Cli::try_parse_from(["tang", "play", "x.toml"]).unwrap();
        assert_eq!((cli.audio_device, cli.midi_device, cli.sample_rate, cli.debug), (None, None, 48000, false));
        assert_eq!(cli.buffer_size.to_string(), DEFAULT_BUFFER_SIZE);
        assert!(Cli::try_parse_from(["tang", "x.toml", "--audio-device", "none"]).is_ok(), "and without a subcommand");
    }
}
