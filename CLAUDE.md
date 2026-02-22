# Overview

This is Tang, a terminal-based audio plugin host.

- Terminal user interface (TUI)
- Supports LV2 and CLAP plugins
- Supports MIDI keyboard
- Includes a Virtual Piano (tracker-style)

## Usage

```
tang                             # launch TUI with default session
tang session.toml                # launch TUI with specified session
tang play session.toml           # play session with virtual piano (no TUI)
tang enumerate plugins           # list installed LV2 and CLAP plugins
tang enumerate builtins          # list built-in plugins
tang enumerate midi              # list available MIDI input devices
tang enumerate audio             # list available audio output devices
tang describe <plugin>           # show plugin details
```

TODO: TUI not yet implemented.

## Flags

These flags apply to the `play` subcommand:

| Flag | Default | Effect |
|------|---------|--------|
| `--midi-device <name>` | all inputs | Open only the named MIDI input |
| `--audio-device <name>` | system default | Use the named audio output |
| `--buffer-size <frames>` | 512 | Audio buffer size in frames |
| `--sample-rate <hz>` | 48000 | Sample rate |

The `play` subcommand takes the session path as a required positional argument.
The TUI (when implemented) will use `~/.config/tang/default.toml` as the default
session, creating an empty one if it does not exist.

## Subcommands

**enumerate** `<target>` — lists one category of available resources. Target is
one of `plugins`, `builtins`, `midi`, or `audio`.

**describe** `<plugin>` — prints plugin info (type, parameters, presets) and exits.

**play** `<session.toml>` — loads a session and plays it in the terminal.
No editing, no TUI.
Keyboard acts as a virtual piano (Amiga tracker layout).
Logs MIDI events and audio info to the screen.

## Session config

Sessions are TOML files. The TUI (when implemented) will use `~/.config/tang/default.toml` by default.
Sessions are saved explicitly only (no auto-save on parameter change).
TODO: Session saving not yet implemented.
A plugin can be specified using:
- `./path/to/Plugin.lv2`: LV2 bundle by path
- `./path/to/Plugin.clap`: CLAP bundle by path
- `lv2:<uri>`: LV2 lookup by URI (lv2: prefix OPTIONAL)
- `clap:<id>`: CLAP lookup by plugin ID (clap: prefix OPTIONAL)
- `builtin:<name>`: built-in plugin (e.g. `builtin:sine`)

Example session config:

```toml
[instrument]
plugin = "lv2:http://tytel.org/helm"
preset = "COA Crispy Pad Organ"

[instrument.params]
"reverb_on" = 1.0

[[effect]]
plugin = "./reverb.lv2"
preset = "Large Empty Hall"
mix = 0.5

[effect.params]
"Decay time" = 2.5

[[effect]]
plugin = "./delay.clap"
mix = 1.0

[effect.params]
"time" = 0.25
"feedback" = 0.4
```

Each plugin slot has:
- `plugin` — plugin path or URI (required)
- `preset` — preset name to load (optional)
- `params` — parameter overrides applied after preset (optional)

Effects additionally have:
- `mix` — host-side dry/wet blend, 0.0=dry 1.0=wet (default: 1.0)

Load order per plugin: load → preset → params.

## TUI

TODO: The TUI is not yet implemented. The design below is planned.

The interface is tab-based.
The status bar at the top shows all tabs with the active one highlighted.

### Tabs

| # | Tab | Description |
|---|-----|-------------|
| 1 | Session | Instrument and effects chain editor with parameter control |
| 2 | Piano | Built-in virtual piano using computer keyboard |
| 3 | Oscilloscope | Real-time waveform of audio output |
| 4 | Debug Log | Scrolling log of MIDI events, audio info, plugin messages |
| 5 | Help | Keybindings and usage reference |

### Global keybindings

These work from any tab (except where noted for the Piano tab):

| Key | Action |
|-----|--------|
| `1` `2` `3` `4` `5` | Switch to tab by number |
| `?` | Jump to Help tab |
| `Tab` | Next tab |
| `Shift+Tab` | Previous tab |
| `Ctrl+Q` | Quit |
| `Ctrl+C` | Quit |

On the Piano tab, alphanumeric keys are captured for note input. Tab switching
uses `Tab`/`Shift+Tab` only.

### Session tab

The session tab has two panes: the chain (left) and parameters (right).
The focused pane is visually distinct so it's always clear which pane is active.
`Enter` moves focus to the parameter pane, `Esc` moves it back to the chain.
`Up`/`Down` navigate within whichever pane has focus.

| Key | Action |
|-----|--------|
| `Down` | Move selection down in focused pane |
| `Up` | Move selection up in focused pane |
| `Shift+Down` | Move selected effect down (reorder, focus follows) |
| `Shift+Up` | Move selected effect up (reorder, focus follows) |
| `a` | Add plugin (opens selector popup) |
| `d` | Delete selected plugin |
| `Enter` | Focus parameter list for selected plugin |
| `Esc` | Back to chain focus |
| `Left` / `Right` | Decrease / increase selected parameter |
| `Ctrl+S` | Save session |
| `Ctrl+Shift+S` | Save session as (prompts for filename, saves to same directory as current session) |

### Debug Log tab keybindings

| Key | Action |
|-----|--------|
| `Up` / `Down` | Scroll through log |
| `End` | Jump to bottom (resume auto-scroll) |

### Piano tab keybindings

| Key | Action |
|-----|--------|
| `[` | Octave down |
| `]` | Octave up |

## Virtual piano

The Piano tab turns the computer keyboard into a MIDI controller using Amiga tracker key layout.
Default base octave is 4 (lower row starts at C3, MIDI note 48). Fixed velocity: 100.

Lower row (base octave):
```
Key:  Z  S  X  D  C  V  G  B  H  N  J  M  ,  L  .  ;  /
Note: C  C# D  D# E  F  F# G  G# A  A# B  C  C# D  D# E
```

Upper row (base octave + 1):
```
Key:  Q  2  W  3  E  R  5  T  6  Y  7  U  I  9  O  0  P
Note: C  C# D  D# E  F  F# G  G# A  A# B  C  C# D  D# E
```

Notes sound on key press and stop on key release.

## Signal chain

```
MIDI sources (hardware keyboards + virtual piano)
  → 1 Instrument
  → N Effects (in series)
  → Audio output
```

Maximum one instrument. Zero or more effects, processed in order. Effects can be
reordered in the Session tab.

## Architecture

N+2 threads, no async:

- **Audio thread** (cpal) — processes the plugin chain, fills output buffers. The plugin
  chain is owned (moved into) the audio callback closure — no mutex needed. A plugin swap
  mechanism exists via bounded crossbeam channels (send new plugin in, receive old plugin
  back for main-thread drop). Used by the `play` subcommand to load the session's plugins
  into the audio thread.
- **MIDI thread(s)** (midir) — one per input device, all push into the MIDI channel.
- **Main thread** — runs the crossterm event loop, handles keyboard input. The virtual
  piano lives here and pushes into the same MIDI channel as hardware devices.

MIDI-to-audio communication via bounded MPSC channel (crossbeam-channel, capacity 1024).

Audio thread logging uses `log::debug!()` for per-event messages (filtered out at default
Info level) and `log::info!()` only once on first callback. No real-time safety issue at
default log levels.

TODO: ratatui TUI event loop on the main thread.

TODO: Mirror audio output into a lock-free ring buffer for the oscilloscope display.

MIDI devices are hot-pluggable — main thread polls for new devices every ~1s.

## Plugin compatibility

Plugin loading is behind a trait. Both formats supported:

- **LV2** — via livi.
- **CLAP** — via clack-host.

## Plugin I/O handling

How Tang handles common plugin I/O mismatches:

- **Multi-output instruments** (e.g. Pianoteq with 16 outputs): the chain uses
  the first stereo pair (channels 1-2) and discards the rest. This is the standard
  main mix output for all known multi-output instruments.

- **Sidechain inputs**: effects with more audio inputs than the chain provides
  (e.g. Calf Reverb with 2 audio + 1 sidechain = 3 inputs) get silence on the
  extra ports. Sidechaining is not supported.

- **Atom sequence ports (LV2)**: only passed to plugins that declare atom
  sequence input ports. Effects without them (e.g. ACE Reverb) get no event
  buffer — avoids livi's AtomSequenceInputsSizeMismatch error.

- **Channel count validation**: all effects in a chain must have the same output
  channel count. The instrument may have more outputs (they get truncated), but
  effects cannot exceed the instrument's count.

## TUI framework

The TUI is built on ratatui 0.30 with the crossterm backend. We target the
latest ratatui release and adopt new APIs as they become available. When
upgrading ratatui, update this version note.

## Platform

Cross-platform: Linux, macOS, Windows.

LV2 support is behind the `lv2` Cargo feature (enabled by default). The LV2
dependency chain (livi → lilv → lilv-sys) requires system C libraries that are
only readily available on Linux. macOS and Windows builds use
`cargo build --no-default-features` for CLAP-only mode.

CI runs clippy + tests on all three platforms (Linux with LV2, macOS/Windows
without).

## Future ideas

- Recording audio output to WAV file
- Auto-reconnect on audio device disconnect
- Background/daemon mode with system tray icon
- In-app volume control
- MIDI device selection tab
- Audio device selection tab

## Notes for Claude

- This document serves as a design document for us.
- We use TODO:s (on new lines) to clarify what is yet to be implemented.
- Notify me when you find refactorings we should do before implementing new things.
- Notify me when you find discrepancies in this document vs how it actually works.
