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

## Flags

These flags apply to both the TUI and the `play` subcommand:

| Flag | Default | Effect |
|------|---------|--------|
| `--midi-device <name>` | all inputs | Open only the named MIDI input |
| `--audio-device <name>` | system default | Use the named audio output |
| `--buffer-size <frames>` | 1024 (Linux), 256 (macOS/Windows) | Audio buffer size in frames |
| `--sample-rate <hz>` | 48000 | Sample rate |

The `play` subcommand takes the session path as a required positional argument.
When launched without a session path, the TUI starts an in-memory default
session (`builtin:sine` as the instrument, no effects) targeting a fresh
timestamped file `~/.config/tang/sessions/session-<id>.toml`. Nothing is
written to disk until the first `Ctrl+S`.

## Subcommands

**enumerate** `<target>` — lists one category of available resources. Target is
one of `plugins`, `builtins`, `midi`, or `audio`.

**describe** `<plugin>` — prints plugin info (type, parameters, presets) and exits.

**play** `<session.toml>` — loads a session and plays it in the terminal.
No editing, no TUI. Accepts input from any connected MIDI device, and the
computer keyboard doubles as a virtual piano (Amiga tracker layout).
Logs MIDI events and audio info to the screen.

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

Sessions are TOML files. Without a session argument the TUI creates a new
in-memory session saved to `~/.config/tang/sessions/session-<id>.toml` on
first save. Sessions are saved explicitly only (no auto-save on parameter
change).
A plugin can be specified using:
- `./path/to/Plugin.lv2`: LV2 bundle by path
- `./path/to/Plugin.clap`: CLAP bundle by path
- `./path/to/Plugin.vst3`: VST3 bundle by path
- `lv2:<uri>`: LV2 lookup by URI (lv2: prefix OPTIONAL)
- `clap:<id>`: CLAP lookup by plugin ID (clap: prefix OPTIONAL)
- `vst3:<name>`: VST3 lookup by name (case-insensitive)
- `builtin:<name>`: built-in plugin (e.g. `builtin:sine`)

### Instrument format

Sessions are a flat list of instruments. Each instrument has a plugin, an
optional key range, and its own effect chain.

```toml
[[instrument]]
plugin = "lv2:http://tytel.org/helm"
range = "C0-B3"
preset = "Pad"
volume = 0.8

[instrument.params]
"reverb_on" = 1.0

[[instrument.effect]]
plugin = "./reverb.lv2"
mix = 0.5

[[instrument]]
plugin = "builtin:sine"
range = "C4-C8"
```

Simple case (one instrument, full range, no effects):
```toml
[[instrument]]
plugin = "builtin:sine"
```

- `[[instrument]]` — one or more instruments
  - `plugin` — plugin path or URI (required)
  - `range` — note range like `"C0-B3"` (optional, omit for full range).
    Either bound may be omitted for an open-ended range: `"C4-"` means C4 and
    up, `"-B3"` means everything up to and including B3.
  - `preset` — preset name to load (optional)
  - `volume` — host-side output gain, applied before effects (default: 1.0,
    uncapped in the TOML; the in-TUI editor (`v`) caps at 4.0)
  - `params` — parameter overrides applied after preset (optional)
  - `[[instrument.effect]]` — zero or more effects in series
  - `[[instrument.modulator]]` — zero or more modulators (LFO or ADSR envelope)

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
modulator parameters within the same instrument. Two types are supported:

**LFO modulator** (default):

```toml
[[instrument.modulator]]
type = "lfo"            # optional, "lfo" is the default
waveform = "sine"       # sine, triangle, saw, square (default: sine)
rate = 0.5              # Hz (default: 1.0)

[[instrument.modulator.target]]
param = "cutoff"        # parameter name
depth = 0.5             # fraction of param range, 0.0–1.0 (default: 0.5)
```

**ADSR envelope modulator** (triggered by note-on/off):

```toml
[[instrument.modulator]]
type = "envelope"
attack = 0.01           # seconds (default: 0.01)
decay = 0.3             # seconds (default: 0.3)
sustain = 0.7           # level 0.0–1.0 (default: 0.7)
release = 0.5           # seconds (default: 0.5)

[[instrument.modulator.target]]
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
[[instrument.modulator.target]]
mod_rate = 0            # index of sibling modulator
depth = 0.3

# Target a sibling modulator's envelope parameters:
[[instrument.modulator.target]]
mod_attack = 1          # index of sibling modulator
depth = 0.5

# Target a sibling modulator's target depth:
[[instrument.modulator.target]]
mod_depth = [0, 0]      # [mod_index, target_index]
depth = 0.2
```

Cross-mod target fields (`mod_rate`, `mod_depth`, `mod_attack`, `mod_decay`,
`mod_sustain`, `mod_release`) are mutually exclusive with `param`. Self-
modulation (targeting own index) is prevented.

### MIDI routing

- Note-on/note-off: filtered by instrument's key range (inclusive)
- CC, pitch bend, channel pressure: duplicated to all instruments
- Instrument with no range: receives all notes (full range)
- Overlapping ranges: notes go to all matching instruments

### Pattern recorder/player

Each instrument can have a recorded MIDI pattern. Recording captures note events
for a fixed number of beats at the current BPM. Playback transposes the pattern
to match any held key and loops while the key is held.

**Session config:**

```toml
[[instrument]]
plugin = "lv2:http://tytel.org/helm"

[instrument.pattern]
bpm = 120.0
length_beats = 4.0
base_note = "C4"
enabled = true

[[instrument.pattern.events]]
frame = 0
status = "on"
note = "C4"
velocity = 100

[[instrument.pattern.events]]
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
[[instrument]]
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

### Splash screen

While session plugins load (which can take seconds for heavyweight
instruments like Pianoteq), the TUI shows a splash screen: TANG logo,
animated spinner, the name of the plugin currently loading, and a progress
bar over plugin slots (instruments + effects). Implemented in
`src/tui/splash.rs` — a transient render thread owns the terminal and
animates at ~12 fps while the main thread loads plugins and reports progress
over a channel. Dropping the `Splash` handle restores the terminal, so load
errors clean up automatically before the error prints. TUI mode only; `play`
mode keeps plain log output. While the splash is up, logging to a terminal
stderr is suppressed (redirected stderr still gets logs).

The interface is tab-based.
The status bar at the top shows all tabs with the active one highlighted.
The global BPM is displayed on the right side of the status bar (e.g. `120 BPM`).

TODO: A clip indicator (`CLIP` in red) on the right side of the status bar
when any audio sample exceeds 1.0, holding for ~2 seconds after the last
clipped sample. Detection via an `AtomicBool` set by the audio thread and
read by the render loop. Not yet wired up.

### Tabs

| # | Tab | Description |
|---|-----|-------------|
| 1 | Session | Instrument and effects chain editor with parameter control |
| 2 | Piano | Virtual piano using computer keyboard |
| 3 | Oscilloscope | Real-time waveform of audio output |
| 4 | Help | Keybindings and usage reference (static, scrollable) |

TODO: Piano and Oscilloscope tabs are placeholders — only Session and Help
are functional today. The Piano tab will host the virtual piano UI; the
Oscilloscope tab will draw the live waveform.

### Global keybindings

These work from any tab (except where noted for the Piano tab):

| Key | Action |
|-----|--------|
| `1` `2` `3` `4` | Switch to tab by number |
| `Tab` | Next tab |
| `Shift+Tab` | Previous tab |
| `Ctrl+Q` | Quit |
| `Ctrl+C` | Quit |

TODO: `?` shortcut to jump to the Help tab.

On the Piano tab, alphanumeric keys are captured for note input. Tab switching
uses `Tab`/`Shift+Tab` only.

### Session tab

The session tab has two panes: the chain (left) and parameters (right).
The focused pane is visually distinct so it's always clear which pane is active.
`Enter` moves focus to the parameter pane, `Esc` moves it back to the chain.
`Up`/`Down` navigate within whichever pane has focus.

#### Chain (left pane)

The chain is rendered as a tree. Instruments are top-level nodes, with effects
and modulators nested under each instrument.

```
♪ Helm [LV2]            C0-B3
├─ fx Reverb [LV2]
├─ fx Compressor [LV2]
└─ ~ LFO 0.5Hz sine → cutoff (50%), resonance (25%)
♪ Sine [Built-in]       C4-C8
└─ fx Delay [CLAP]
```

Navigation is a single cursor through the flattened tree. Actions (`i`/`a`/`d`/`m`)
operate on the instrument containing the selected node. Modulators appear in
magenta after effects.

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
| `Down` / `Up` | Move selection in focused pane |
| `PageDown` / `PageUp` | Jump selection by a page |
| `Shift+Down` / `Shift+Up` | Move selected effect down/up (reorder, focus follows) |
| `n` | Add a new instrument (defaults to `builtin:sine`, opens the plugin selector to replace it; full range until set with `R`) |
| `i` | Replace instrument (opens instrument selector popup) |
| `R` | Set the selected instrument's key range (opens range popup, prefilled) |
| `v` | Set the selected instrument's output volume (host-side gain, opens value popup) |
| `a` | Add effect (opens effect selector popup). New effects default to 0.5 mix (half-wet) |
| `m` | Add modulator to current instrument |
| `x` | Set the selected effect's dry/wet mix (host-side blend, opens value popup) |
| `d` | Delete selected instrument/effect/modulator/pattern (no confirmation) |
| `t` | Add modulation target (when modulator selected, opens target selector) |
| `Enter` | Focus parameter list / open value editor on selected parameter |
| `Esc` | Back to chain focus / close popup |
| `Left` / `Right` | Decrease / increase selected parameter |
| `Shift+Left` / `Shift+Right` | Fine decrease / increase selected parameter |
| `Ctrl+Left` / `Ctrl+Right` | Coarse decrease / increase selected parameter |
| `/` | Filter parameter list by name |
| `r` | Record/stop pattern for current instrument |
| `b` | Set global BPM (opens value entry popup) |
| `Ctrl+S` | Save session (to the file it was loaded from) |
| `Ctrl+Shift+S` | Save session as (prompts for filename, saves in the current session's directory) |

Mouse: click to select, drag a parameter bar to set its value, drag the
parameter scrollbar to scroll. Mouse wheel scrolls the parameter list and the
Help tab.

TODO: `p` to browse presets for the selected plugin (preset selector popup).
TODO: `e` shortcut to open the value entry popup directly (currently use
`Enter` from chain focus or land on a param and press `Enter`).
TODO: `Ctrl+R` shortcut to clear a pattern (currently select the pattern
node and press `d`).

### Plugin selector popup

Opened by `i` (instruments only) or `a` (effects only). Same layout for both,
filtered by plugin type.

- **Top**: text input for filtering (matches against any column: name, type, etc.)
- **Below**: table with columns — Name, Format (LV2/CLAP/VST3), Params, Presets
- `Up` / `Down` — navigate rows
- `Enter` — select plugin and close popup
- `Escape` — cancel and close popup
- Typing updates the filter immediately

The plugin catalog is scanned on a background thread started when the TUI
launches (a full scan instantiates every installed plugin and can take
seconds, so it must not block the first frame). Results arrive per format
(built-ins, LV2, CLAP, VST3) and the popup fills in progressively; while the
scan is still running the popup title shows "(scanning…)".

### Preset selector popup

TODO: Not yet implemented. Planned design:

Opened by `p` on the Session tab.

- **Top**: text input for filtering by name
- **Below**: single-column list of preset names
- `Up` / `Down` — navigate rows
- `Enter` — load preset and close popup
- `Escape` — cancel and close popup

### Value entry popup

Opened by `Enter` on a selected parameter (and also used for `b` BPM entry,
`x` effect mix entry, and `v` instrument volume entry).

- Shows parameter name, current value, and valid range
- Text input for entering a numeric value
- `Enter` — accept value and close popup
- `Escape` — cancel and close popup

### Range entry popup

Opened by `R` to set the selected instrument's key range. Titled "Set Range"
and prefilled with the current range.

- Text input for a note range like `C0-B3`. Either bound may be omitted:
  `C4-` (C4 and up), `-B3` (up to B3), or empty (full range)
- `Enter` — accept and close popup (kept open on parse error)
- `Escape` — cancel and close popup

To build a split: press `n` to add a new instrument (this adds a full-range
lane defaulting to `builtin:sine` and opens the plugin selector to replace it —
cancelling the selector leaves the sine in place), then press `R` to set that
lane's key range. Repeat for each split zone.

### Save As popup

Opened by `Ctrl+Shift+S`. Titled "Save As" and prefilled with the current
session's filename.

- Text input for a filename. `.toml` is appended if missing. The file is saved
  in the current session's directory (cwd if there is no current session).
- `Enter` — save and close; `session_path` is updated so subsequent `Ctrl+S`
  targets the new file. On failure (or an empty field) the popup stays open —
  the error is shown in the popup and `session_path` keeps its old value.
- `Escape` — cancel and close popup

Distinguishing `Ctrl+Shift+S` from `Ctrl+S` requires the kitty keyboard
protocol (the TUI pushes `DISAMBIGUATE_ESCAPE_CODES` when the terminal
supports it). On terminals without it, the SHIFT modifier is not reported
for Ctrl+letter chords and `Ctrl+Shift+S` performs a plain save instead.

### Modulation target selector popup

Opened by `t` when a modulator is selected. Lists candidate targets for the
modulator (plugin parameters and sibling modulator parameters).

- `Up` / `Down` — navigate rows
- `Enter` — bind target and close popup
- `Escape` — cancel and close popup

### Piano tab

Visual piano keyboard for learning scales and seeing what's playing in
real time.

Layout: status line on top (scale + view octave + currently-held notes
with degree labels), the keyboard itself in the middle, a "scale strip"
underneath showing the notes of the scale, and a hint line at the bottom.

The keyboard auto-sizes to the terminal width — it tries 4 octaves and
the widest white-key width that fits, falling back to narrower or fewer
octaves as needed. The view is centered on `piano_view_octave`.

Key colors (in order of priority):
- **Held** → bright red (magenta if also root)
- **Root** of current scale → amber/gold
- **Scale tone** (non-root) → teal
- **Off-scale** → plain white/black

| Key | Action |
|-----|--------|
| `k` | Open scale picker (root + scale type, filterable) |
| `[` | Shift view octave down |
| `]` | Shift view octave up |

The scale persists in the session file under a top-level `[piano]`
section:

```toml
[piano]
scale = "C major"
```

Accepted forms include sharps (`C#`), flats (`Bb`), short names
(`major`, `minor`, `dorian`, `maj pent`, `min pent`, `blues`,
`chromatic`, `whole tone`, `diminished`), and long names
(`Natural Minor`, `Harmonic Minor`, `Melodic Minor`, etc.).

Currently-held notes are tracked in a lock-free `Arc<HeldNotes>` (a
128-bit atomic bitset). Both the hardware MIDI thread and the virtual
piano set/clear bits on NoteOn/NoteOff; the TUI reads them every render
frame (~10 fps).

TODO: Capture letter keys on the Piano tab for note input via the
virtual piano (currently the Piano tab is read-only — notes still play
from the global virtual piano which is only wired up in `play` mode).

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
  → For each instrument (filtered by note range):
      → Modulators apply (set_parameter on targets)
      → Instrument → volume gain → N Effects (in series, each with dry/wet mix)
  → Sum all instrument outputs
  → Audio output → clip detection
```

Each instrument has its own effect chain. MIDI note events are filtered by the
instrument's key range; CC/pitch bend messages are duplicated to all instruments.
All instrument outputs are summed together.

Effects can be reordered within an instrument in the Session tab. Instrument
volume is applied after the instrument's output and before the first effect in
that instrument's chain.

## Architecture

N+2 threads, no async:

- **Audio thread** (cpal, JACK or ALSA on Linux) — processes the instrument
  chains, fills output buffers. Promoted to SCHED_FIFO real-time scheduling on
  first callback. The instrument list is owned (moved into) the audio callback
  closure — no mutex needed. A plugin swap mechanism exists via bounded crossbeam
  channels (send new plugin in, receive old plugin back for main-thread drop).
  Used by the `play` subcommand to load the session's plugins into the audio
  thread.
- **MIDI thread(s)** (midir) — one per input device, all push into the MIDI channel.
- **Main thread** — runs the crossterm event loop, handles keyboard input. The virtual
  piano lives here and pushes into the same MIDI channel as hardware devices.
- **Catalog scan thread** (TUI only, transient) — enumerates installed plugins
  for the selector popup at TUI startup and exits when done. Sends per-format
  batches over an unbounded channel drained by the TUI event loop.
- **Splash render thread** (TUI only, transient) — owns the terminal during
  session plugin loading, animating the startup splash while the main thread
  loads plugins. Exits before `tui::run` takes over the terminal.

The shared LV2 world (`plugin::Runtime`) is created lazily on the first LV2
plugin load, not at startup — building the world scans every installed LV2
bundle, which sessions without LV2 plugins shouldn't pay for.

MIDI-to-audio communication via bounded MPSC channel (crossbeam-channel, capacity 1024).

Audio thread logging uses `log::debug!()` for per-event messages (filtered out at default
Info level) and `log::info!()` only once on first callback. No real-time safety issue at
default log levels.

TODO: Mirror audio output into a lock-free ring buffer for the oscilloscope display.

MIDI devices are hot-pluggable — main thread polls for new devices every ~1s.

## Audio host selection

Tang uses cpal for audio output. The host backend is selected at startup with
automatic fallback:

| Platform | Preferred | Fallback | Notes |
|----------|-----------|----------|-------|
| Linux | JACK (via PipeWire) | ALSA | JACK avoids PipeWire's ALSA compatibility layer, which can destabilize the global audio quantum at small buffer sizes |
| macOS | CoreAudio | — | Native, no fallback needed |
| Windows | WASAPI | — | Native, no fallback needed |

On Linux, the JACK host is only used if it can find an output device. This
requires PipeWire's JACK bridge — either via `pw-jack tang` or by installing
the system-wide `ld.so.conf` redirect:

```bash
sudo cp /usr/share/doc/pipewire/examples/ld.so.conf.d/pipewire-jack-x86_64-linux-gnu.conf /etc/ld.so.conf.d/
sudo ldconfig
```

Without this, Tang falls back to ALSA transparently.

On Linux, the audio callback thread is promoted to `SCHED_FIFO` real-time
scheduling (priority 50) on its first invocation. This requires the user to
have `rtprio` privileges (typically via the `audio` group). Fails silently
if privileges are missing. macOS and Windows handle real-time audio thread
scheduling natively.

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
dependency chain (livi → lilv → lilv-sys) requires the `lilv` system C
library:

- **Linux**: `apt install liblilv-dev` (or distro equivalent)
- **macOS**: `brew install lilv`
- **Windows**: not supported

The upstream `lv2-sys` crate (RustAudio/rust-lv2) doesn't compile on macOS
because its `unsupported.rs` is a `compile_error!`. PR #117 fixes this with
a one-line `cfg_attr` aliasing macOS to the Linux bindings. Until that PR
merges, `Cargo.toml` patches `lv2-sys` to akx's fork (1 commit ahead, 0
behind upstream develop). Drop the `[patch.crates-io]` entry once #117 lands.

VST3 support is behind the `vst3` Cargo feature (enabled by default). It uses
pre-generated bindings (no build-time SDK dependency) and works on all platforms.

CI runs clippy + tests on all three platforms:

- **Linux** — full default features (LV2 + VST3 + CLAP), `apt install liblilv-dev libasound2-dev libjack-dev`
- **macOS** — full default features (LV2 + VST3 + CLAP), `brew install lilv`
- **Windows** — `--no-default-features --features vst3` (CLAP is not feature-gated, so it's always built; LV2 is unsupported)

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
