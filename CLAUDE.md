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

These flags apply to both the TUI and the `play` subcommand:

| Flag | Default | Effect |
|------|---------|--------|
| `--midi-device <name>` | all inputs | Open only the named MIDI input |
| `--audio-device <name>` | system default | Use the named audio output |
| `--buffer-size <frames>` | 512 | Audio buffer size in frames |
| `--sample-rate <hz>` | 48000 | Sample rate |

The `play` subcommand takes the session path as a required positional argument.
The TUI uses `~/.config/tang/default.toml` as the default session. If the file
does not exist, Tang creates it with `builtin:sine` as the instrument and no
effects.

## Subcommands

**enumerate** `<target>` — lists one category of available resources. Target is
one of `plugins`, `builtins`, `midi`, or `audio`.

**describe** `<plugin>` — prints plugin info (type, parameters, presets) and exits.

**play** `<session.toml>` — loads a session and plays it in the terminal.
No editing, no TUI.
Keyboard acts as a virtual piano (Amiga tracker layout).
Logs MIDI events and audio info to the screen.
TODO: Remove once the TUI is functional.

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
volume = 0.8

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

The instrument additionally has:
- `volume` — host-side output gain, applied before effects (default: 1.0, uncapped)

Effects additionally have:
- `mix` — host-side dry/wet blend, 0.0=dry 1.0=wet (default: 1.0)

Load order per plugin: load → preset → params.

## Note remapping

Some instrument plugins have bad samples on certain notes. Note remapping lets
you substitute a specific key with a nearby note on a separate MIDI channel,
using pitch bend to shift it to the correct pitch.

### Config syntax

```toml
[instrument]
plugin = "lv2:http://tytel.org/helm"
pitch_bend_range = 2  # optional, default ±2 semitones

[instrument.remap]
"G#4" = { note = "G4", detune = 1.0 }
"C#2" = { note = "D2", detune = -0.5 }
```

- `pitch_bend_range` — the plugin's pitch bend range in semitones (default: 2.0).
  Must match the plugin's own pitch bend range setting.
- `[instrument.remap]` — a table of note substitutions. Keys are source note
  names (`[A-G][#b]?[0-9]`, C4 = middle C = MIDI 60). Each value has:
  - `note` — the target note name the plugin will actually play
  - `detune` — pitch bend offset in semitones (e.g. 1.0 = one semitone up)

### How it works

- Normal (non-remapped) notes play on MIDI channel 1 as usual.
- Each unique detune value is assigned its own MIDI channel (2–16). Notes that
  share the same detune value share a channel.
- When a remapped note-on is received, Tang sends the rewritten note-on followed
  by a pitch bend message on that channel (bend after note-on — some plugins
  only apply pitch bend to already-sounding notes).
- Most plugins respond to all MIDI channels by default (omni mode), so no
  plugin-side configuration is needed beyond setting the pitch bend range.

### Limits

- Maximum 15 distinct detune values (MIDI channels 2–16).
- Detune must not exceed `pitch_bend_range` (error at load time).
- Sustain pedal (CC64) only affects channel 1 (non-remapped notes).

## TUI

TODO: The TUI is not yet implemented. The design below is planned.

The interface is tab-based.
The status bar at the top shows all tabs with the active one highlighted.
A clip indicator (`CLIP` in red) appears on the right side of the status bar
when any audio sample exceeds 1.0. It holds for ~2 seconds after the last
clipped sample, then disappears. Detection is via an `AtomicBool` set by the
audio thread and read by the render loop.

### Tabs

| # | Tab | Description |
|---|-----|-------------|
| 1 | Session | Instrument and effects chain editor with parameter control |
| 2 | Piano | Virtual piano using computer keyboard |
| 3 | Oscilloscope | Real-time waveform of audio output |
| 4 | Help | Keybindings and usage reference (static, scrollable) |

TODO: Oscilloscope tab is a placeholder — not implemented yet.

### Global keybindings

These work from any tab (except where noted for the Piano tab):

| Key | Action |
|-----|--------|
| `1` `2` `3` `4` | Switch to tab by number |
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

#### Chain (left pane)

Each plugin is rendered as a card. The vertical order follows the signal chain
(instrument at top, effects below). Each card shows:

- Type prefix and name: `♪ Helm` (instrument) or `fx ACE Reverb` (effect)
- Format tag: `[LV2]` or `[CLAP]`
- Preset name below the plugin name (dimmed, if loaded)
- Instrument: volume bar with value
- Effects: mix bar with value

The selected card is highlighted. Unselected cards are dimmed.

#### Parameters (right pane)

Shows all parameters for the selected plugin as horizontal bars:

```
  cutoff          ▓▓▓▓▓▓▓▓░░░░ 0.75
  resonance       ▓▓▓░░░░░░░░░ 0.25
▸ attack          ▓░░░░░░░░░░░ 0.05
  decay           ▓▓▓▓░░░░░░░░ 0.30
```

The selected parameter is marked with `▸`. The bar shows the parameter's
position within its min–max range. The numeric value is shown on the right.

| Key | Action |
|-----|--------|
| `Down` | Move selection down in focused pane |
| `Up` | Move selection up in focused pane |
| `Shift+Down` | Move selected effect down (reorder, focus follows) |
| `Shift+Up` | Move selected effect up (reorder, focus follows) |
| `i` | Replace instrument (opens instrument selector popup) |
| `a` | Add effect (opens effect selector popup) |
| `d` | Delete selected plugin (no confirmation) |
| `p` | Browse presets for selected plugin (opens preset selector popup) |
| `Enter` | Focus parameter list for selected plugin |
| `Esc` | Back to chain focus |
| `Left` / `Right` | Decrease / increase selected parameter |
| `Shift+Left` / `Shift+Right` | Fine decrease / increase selected parameter |
| `Ctrl+Left` / `Ctrl+Right` | Coarse decrease / increase selected parameter |
| `e` | Edit parameter value (opens value entry popup) |
| `Ctrl+S` | Save session |
| `Ctrl+Shift+S` | Save session as (prompts for filename, saves to same directory as current session) |

### Plugin selector popup

Opened by `i` (instruments only) or `a` (effects only). Same layout for both,
filtered by plugin type.

- **Top**: text input for filtering (matches against any column: name, type, etc.)
- **Below**: table with columns — Name, Format (LV2/CLAP), Params, Presets
- `Up` / `Down` — navigate rows
- `Enter` — select plugin and close popup
- `Escape` — cancel and close popup
- Typing updates the filter immediately

### Preset selector popup

Opened by `p` on the Session tab.

- **Top**: text input for filtering by name
- **Below**: single-column list of preset names
- `Up` / `Down` — navigate rows
- `Enter` — load preset and close popup
- `Escape` — cancel and close popup

### Value entry popup

Opened by `e` on a selected parameter.

- Shows parameter name, current value, and valid range
- Text input for entering a numeric value
- `Enter` — accept value and close popup
- `Escape` — cancel and close popup

### Piano tab

Captures keyboard for note input. Shows current octave and active notes.

| Key | Action |
|-----|--------|
| `[` | Octave down |
| `]` | Octave up |

### Help tab

Static text showing all keybindings and usage reference. Scrollable with
`Up`/`Down`.

### Dirty indicator

When the session has unsaved changes (parameter tweaks, plugin adds/removes,
preset loads, effect reordering), the Session tab label shows an asterisk:
`Session *`. The asterisk clears on save.

### Session state

The main thread maintains an in-memory session model that tracks all user
changes: plugin selections, preset names, parameter values, mix values, volume,
and effect order. This model is the source of truth for `Ctrl+S` serialization.
Every user action (preset load, parameter tweak, plugin add/remove, reorder)
updates both the session model (for saving) and sends a command to the audio
thread (for playback). The dirty indicator is driven by this model.

Loading a preset clears all parameter overrides for that slot. The preset
sets every parameter to its own values, so previous overrides are discarded.
Any parameter tweaks made after the preset load are recorded as new overrides.

### Logging

All logging goes to stderr via `RawModeLogger`, same as in `play` mode. Use
`tang 2> debug.log` to capture logs to a file. No in-app log viewer.

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
  → 1 Instrument → volume gain
  → N Effects (in series, each with dry/wet mix)
  → Audio output → clip detection
```

Maximum one instrument. Zero or more effects, processed in order. Effects can be
reordered in the Session tab. Instrument volume is applied after the instrument's
output and before the first effect.

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
