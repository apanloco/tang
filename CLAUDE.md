# Overview

This is Tang, a terminal-based audio plugin host.

- Terminal user interface (TUI)
- Supports LV2, CLAP, and VST3 plugins
- Supports MIDI keyboard
- Includes a Virtual Piano (tracker-style)

## Usage

```
tang                             # launch TUI with default session
tang session.toml                # launch TUI with specified session
tang play session.toml           # play session with virtual piano (no TUI)
tang enumerate plugins           # list installed LV2, CLAP, and VST3 plugins
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

## Config file

**Path:** `~/.config/tang/config.toml`

Optional application-level config. Currently supports custom plugin search paths.
Missing file or missing keys = no extra paths.

```toml
[plugin_paths]
clap = ["/home/user/my-plugins/clap"]
vst3 = ["/home/user/my-plugins/vst3", "/mnt/external/vst3"]
lv2 = ["/home/user/my-plugins/lv2"]
```

All fields optional (default: empty). Extra paths are appended after platform
defaults. Loaded once at startup via a `OnceLock<Config>` global in `src/config.rs`.

For LV2, extra paths are injected into the `LV2_PATH` environment variable before
`livi::World::new()` is called. For CLAP and VST3, the respective plugin modules
read the extra paths directly.

## Session config

Sessions are TOML files. The TUI (when implemented) will use `~/.config/tang/default.toml` by default.
Sessions are saved explicitly only (no auto-save on parameter change).
TODO: Session saving not yet implemented.
A plugin can be specified using:
- `./path/to/Plugin.lv2`: LV2 bundle by path
- `./path/to/Plugin.clap`: CLAP bundle by path
- `./path/to/Plugin.vst3`: VST3 bundle by path
- `lv2:<uri>`: LV2 lookup by URI (lv2: prefix OPTIONAL)
- `clap:<id>`: CLAP lookup by plugin ID (clap: prefix OPTIONAL)
- `vst3:<name>`: VST3 lookup by name (case-insensitive)
- `builtin:<name>`: built-in plugin (e.g. `builtin:sine`)

### Keyboard/split format

Sessions are organized into keyboards and splits. Each keyboard represents a
MIDI input context. Each split within a keyboard maps a key range to an
instrument with its own effect chain.

```toml
[[keyboard]]
name = "Main"

[[keyboard.split]]
range = "C0-B3"

[keyboard.split.instrument]
plugin = "lv2:http://tytel.org/helm"
preset = "Pad"
volume = 0.8

[keyboard.split.instrument.params]
"reverb_on" = 1.0

[[keyboard.split.effect]]
plugin = "./reverb.lv2"
mix = 0.5

[[keyboard.split]]
range = "C4-C8"

[keyboard.split.instrument]
plugin = "builtin:sine"
```

Simple case (one keyboard, one full-range split, no effects):
```toml
[[keyboard]]
[[keyboard.split]]
[keyboard.split.instrument]
plugin = "builtin:sine"
```

- `[[keyboard]]` — one or more keyboards
  - `name` — display name (optional, defaults to "Keyboard N")
  - `[[keyboard.split]]` — one or more splits per keyboard
    - `range` — note range like `"C0-B3"` (optional, omit for full range)
    - `[keyboard.split.instrument]` — the instrument for this split (required)
    - `[[keyboard.split.effect]]` — zero or more effects in series
    - `[[keyboard.split.modulator]]` — zero or more modulators (LFO or ADSR envelope)

### Legacy format

The legacy format (`[instrument]` + `[[effect]]`) is auto-detected and wrapped
into a single keyboard with one full-range split:

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
```

### Plugin slot fields

Each plugin slot has:
- `plugin` — plugin path or URI (required)
- `preset` — preset name to load (optional)
- `params` — parameter overrides applied after preset (optional)

The instrument additionally has:
- `volume` — host-side output gain, applied before effects (default: 1.0, uncapped)

Effects additionally have:
- `mix` — host-side dry/wet blend, 0.0=dry 1.0=wet (default: 1.0)

Load order per plugin: load → preset → params.

### Modulator fields

Modulators are block-rate sources that modulate plugin parameters or sibling
modulator parameters within the same split. Two types are supported:

**LFO modulator** (default):

```toml
[[keyboard.split.instrument.modulator]]
type = "lfo"            # optional, "lfo" is the default
waveform = "sine"       # sine, triangle, saw, square (default: sine)
rate = 0.5              # Hz (default: 1.0)

[[keyboard.split.instrument.modulator.target]]
param = "cutoff"        # parameter name
depth = 0.5             # fraction of param range, 0.0–1.0 (default: 0.5)
```

**ADSR envelope modulator** (triggered by note-on/off):

```toml
[[keyboard.split.instrument.modulator]]
type = "envelope"
attack = 0.01           # seconds (default: 0.01)
decay = 0.3             # seconds (default: 0.3)
sustain = 0.7           # level 0.0–1.0 (default: 0.7)
release = 0.5           # seconds (default: 0.5)

[[keyboard.split.instrument.modulator.target]]
param = "cutoff"
depth = 0.5
```

Each modulator applies `base_value + depth * output * range` to its targets
once per audio buffer. LFO output is bipolar (-1..1), envelope output is
unipolar (0..1). The base value tracks the user's set value automatically.

Envelope behavior: note-on retriggers from Attack phase. Release begins when
all held notes are released. Linear ramps for A/D/R phases.

**Cross-modulation** — a modulator can target sibling modulators' parameters
instead of (or in addition to) plugin parameters:

```toml
# Target a sibling modulator's LFO rate:
[[keyboard.split.instrument.modulator.target]]
mod_rate = 0            # index of sibling modulator
depth = 0.3

# Target a sibling modulator's envelope parameters:
[[keyboard.split.instrument.modulator.target]]
mod_attack = 1          # index of sibling modulator
depth = 0.5

# Target a sibling modulator's target depth:
[[keyboard.split.instrument.modulator.target]]
mod_depth = [0, 0]      # [mod_index, target_index]
depth = 0.2
```

Cross-mod target fields (`mod_rate`, `mod_depth`, `mod_attack`, `mod_decay`,
`mod_sustain`, `mod_release`) are mutually exclusive with `param`. Self-
modulation (targeting own index) is prevented.

### MIDI routing

- Note-on/note-off: filtered by split's key range (inclusive)
- CC, pitch bend, channel pressure: duplicated to all splits within the keyboard
- Split with no range: receives all notes (full range)
- Overlapping ranges: notes go to all matching splits

### Pattern recorder/player

Each split can have a recorded MIDI pattern. Recording captures note events for
a fixed number of beats at the current BPM. Playback transposes the pattern to
match any held key and loops while the key is held.

**Session config:**

```toml
[[keyboard.split]]
[keyboard.split.instrument]
plugin = "lv2:http://tytel.org/helm"

[keyboard.split.pattern]
bpm = 120.0
length_beats = 4.0
base_note = "C4"
enabled = true

[[keyboard.split.pattern.events]]
frame = 0
status = "on"
note = "C4"
velocity = 100

[[keyboard.split.pattern.events]]
frame = 24000
status = "off"
note = "C4"
velocity = 0
```

- `bpm` — tempo used for pattern timing (default: 120)
- `length_beats` — pattern length in beats (default: 4 = 1 bar in 4/4)
- `base_note` — reference note for transposition (set to first recorded note)
- `events` — recorded MIDI events with `frame` (sample offset), `status`
  ("on"/"off"), `note` (e.g. "C4"), and `velocity`
- `enabled` — whether pattern playback is active

**Behavior:**
- Press `r` on the Session tab to start recording. Play notes on the virtual
  piano or MIDI keyboard. Recording auto-stops after `length_beats` at current
  BPM.
- Hold any key to play back the pattern transposed (relative to `base_note`).
  The pattern loops while the key is held and stops on release.
- Press `r` again to overwrite with a new recording.
- Press `Ctrl+R` to clear the pattern.
- Press `b` to set the global BPM.
- BPM is displayed in the status bar. Pattern indicators show in the chain tree:
  `▶` = pattern exists, `⏺` = recording.

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
The global BPM is displayed on the right side of the status bar (e.g. `120 BPM`).
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

The chain is rendered as a tree. Keyboards are top-level nodes, splits are
children with their instruments, and effects are nested under each split.

```
⌨ Keyboard 1
├─ ♪ Helm [LV2]            C0-B3
│  ├─ fx Reverb [LV2]
│  ├─ fx Compressor [LV2]
│  └─ ~ LFO 0.5Hz sine → cutoff (50%), resonance (25%)
└─ ♪ Sine [Built-in]       C4-C8
⌨ Keyboard 2
└─ ♪ Lead Synth [CLAP]
   └─ fx Delay [CLAP]
```

Navigation is a single cursor through the flattened tree. Keyboard rows show
no parameters. Actions (`i`/`a`/`d`/`m`) operate on the split containing the
selected node. Modulators appear in magenta after effects.

The selected entry is highlighted. Unselected entries are dimmed.

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
| `m` | Add modulator to current split |
| `d` | Delete selected plugin/modulator (no confirmation) |
| `p` | Browse presets for selected plugin (opens preset selector popup) |
| `t` | Add modulation target (when modulator selected, opens target selector) |
| `Enter` | Focus parameter list for selected plugin |
| `Esc` | Back to chain focus |
| `Left` / `Right` | Decrease / increase selected parameter |
| `Shift+Left` / `Shift+Right` | Fine decrease / increase selected parameter |
| `Ctrl+Left` / `Ctrl+Right` | Coarse decrease / increase selected parameter |
| `e` | Edit parameter value (opens value entry popup) |
| `r` | Record/stop pattern for current split |
| `Ctrl+R` | Clear pattern for current split |
| `b` | Set global BPM (opens value entry popup) |
| `Ctrl+S` | Save session |
| `Ctrl+Shift+S` | Save session as (prompts for filename, saves to same directory as current session) |

### Plugin selector popup

Opened by `i` (instruments only) or `a` (effects only). Same layout for both,
filtered by plugin type.

- **Top**: text input for filtering (matches against any column: name, type, etc.)
- **Below**: table with columns — Name, Format (LV2/CLAP/VST3), Params, Presets
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
  → For each keyboard:
      → For each split (filtered by note range):
          → Modulators apply (set_parameter on targets)
          → Instrument → volume gain → N Effects (in series, each with dry/wet mix)
  → Sum all splits
  → Audio output → clip detection
```

Each keyboard contains one or more splits. Each split has its own instrument
and effect chain. MIDI note events are filtered by split range; CC/pitch bend
messages are duplicated to all splits. All split outputs are summed together.

Effects can be reordered within a split in the Session tab. Instrument volume
is applied after the instrument's output and before the first effect in that
split.

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

Plugin loading is behind a trait. Three formats supported:

- **LV2** — via livi.
- **CLAP** — via clack-host.
- **VST3** — via vst3-rs (coupler-rs/vst3-rs) with libloading.

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

VST3 support is behind the `vst3` Cargo feature (enabled by default). It uses
pre-generated bindings (no build-time SDK dependency) and works on all platforms.

CI runs clippy + tests on all three platforms (Linux with LV2+VST3,
macOS/Windows with VST3 only).

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
