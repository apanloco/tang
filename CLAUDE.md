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
tang --web session.toml          # TUI, and serve the session to a browser too (needs --features web)
tang serve session.toml          # serve the session to a browser (no TUI; needs --features web)
tang enumerate plugins           # list installed LV2, CLAP, and VST3 plugins
tang enumerate builtins          # list built-in plugins
tang enumerate midi              # list available MIDI input devices
tang enumerate audio             # list available audio output devices
tang describe <plugin>           # show plugin details
```

## Flags

These flags apply to the TUI and to the `play` and `serve` subcommands, and
can be written before or after the subcommand (`tang play x.toml --audio-device
none`):

| Flag | Default | Effect |
|------|---------|--------|
| `--midi-device <name>` | all inputs | Open only the named MIDI input |
| `--audio-device <name>` | system default | Use the named audio output; `none` opens no device and renders the engine on a timer (for CI, headless runs and tests: the audio thread's work happens, nothing is heard) |
| `--buffer-size <frames>` | 1024 (Linux), 256 (macOS/Windows) | Audio buffer size in frames |
| `--sample-rate <hz>` | 48000 | Sample rate |
| `--debug` | off | Show a live incoming-MIDI monitor at the bottom of the TUI (diagnoses what a controller actually sends) |

`--debug` adds a strip at the bottom of the TUI tallying incoming MIDI by kind
(note / pitch bend / channel pressure / poly aftertouch / each CC number), each
row showing the latest raw bytes and a hit count. Useful for confirming what an
expressive controller emits — e.g. whether a hard key press sends Channel
Pressure (learnable) or is routed to a CC. Implemented as a lock-free-ish
`Arc<MidiMonitor>` (`src/midi_monitor.rs`) written by the MIDI input thread and
read by the render loop; a no-op when the flag is off.

**`--web[=<addr>]`** (the TUI only; needs a build with `--features web`, without
which it is refused before the session is loaded) serves the session to browsers
while the TUI runs, so that what is changed in either shows in the other. The
address, an IP and a port, comes only after an `=` (`tang --web=0.0.0.0:9000`; so
`tang --web x.toml` is a session) and is `127.0.0.1:8765` without one;
`--web-origin <origin>` (repeatable, needs `--web`) lets another page connect, a
dev server's, say. As for `serve`, beyond loopback a token is made and put in the
address's fragment. The address (with the same warnings `serve` logs, for an address
beyond this machine) is printed on stdout before the TUI takes the screen, and shown
while it runs: in the tab bar (`web 127.0.0.1:8765 (2)`, where it listens and how
many browsers are on it, shortened or left out on a narrow terminal) and on the Help
tab. A port that is taken is an error at once, before the session is loaded and the
terminal is taken over (the address is tried, and let go again). `play` and
`serve` do not take it (`serve` has `--web-bind`). See "Web UI" and
`docs/web-ui.md` §3.4.

The `play` and `serve` subcommands take the session path as an optional
positional argument. When launched without a session path, the TUI (or the
subcommand) starts an in-memory default
session (`builtin:osc` as the instrument, no effects) targeting a fresh
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

**serve** `[session.toml] [--web-bind <ip:port>] [--web-origin <origin>]...` —
loads a session like every mode and serves it to browsers, with no terminal
interface (needs a build with `--features web`; without it the subcommand says
so). The first line on stdout is the address to open (`Web interface:
http://127.0.0.1:8765/`), the log is plain text on stderr, and Ctrl+C / SIGTERM /
SIGHUP closes the browsers and stops the program. `--web-bind` (default
`127.0.0.1:8765`, an IP and a port) beyond loopback makes a token for the run and
puts it in the address's fragment (`http://host:port/#token=…`); `--web-origin`
(repeatable) lets another page connect, a dev server's, say; an address that is
taken, or beyond loopback without a token that can be made, is an error before the
session is loaded. Works with `--audio-device none`, which is how CI and the
end-to-end tests run it. Without a
MIDI subsystem (a container) it warns and carries on. The page at `/` is the web
interface (`web/`, read from the disk in a debug build and embedded in a release
build; a placeholder if it was not built).

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
change). The session's file is known by its absolute path from the start
(`session::absolute_path`): some plugins change the working directory of the
whole process while they are scanned or loaded (a ROLI one does), and a path
relative to it would then lead somewhere else.
A plugin can be specified using:
- `./path/to/Plugin.lv2`: LV2 bundle by path
- `./path/to/Plugin.clap`: CLAP bundle by path
- `./path/to/Plugin.vst3`: VST3 bundle by path
- `lv2:<uri>`: LV2 lookup by URI (lv2: prefix OPTIONAL)
- `clap:<id>`: CLAP lookup by plugin ID (clap: prefix OPTIONAL)
- `vst3:<name>`: VST3 lookup by name (case-insensitive)
- `builtin:<name>`: built-in plugin (e.g. `builtin:osc`)

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
plugin = "builtin:osc"
range = "C4-C8"
```

Simple case (one instrument, full range, no effects):
```toml
[[instrument]]
plugin = "builtin:osc"
```

- `[[instrument]]` — one or more instruments
  - `plugin` — plugin path or URI (required)
  - `range` — note range like `"C0-B3"` (optional, omit for full range).
    Either bound may be omitted for an open-ended range: `"C4-"` means C4 and
    up, `"-B3"` means everything up to and including B3.
  - `preset` — preset name to load (optional)
  - `volume` — host-side output gain, applied before effects (default: 1.0,
    uncapped in the TOML; the in-TUI editor (`v`) caps at 4.0)
  - `transpose` — shift incoming notes by this many semitones (default: 0,
    config-file only — shown in the chain tree but not editable in the TUI)
  - `params` — parameter overrides applied after preset (optional)
  - `group` — submix group membership: index into the top-level `[[group]]`
    list (optional, omit for ungrouped)
  - `[[instrument.effect]]` — zero or more effects in series
  - `[[instrument.modulator]]` — zero or more modulators (LFO or ADSR envelope)

### Group format

Optional submix groups. A group sums its member instruments (those whose
`group` index points at it), runs its own end-of-chain effect chain and output
volume on that sum, then adds the result to the master mix.

```toml
[[group]]
name = "Pad"            # optional display name (default: "Group N")
volume = 0.8            # group bus output gain (default: 1.0)
pan = -0.3             # group bus stereo balance (default: 0.0 = center)

[[group.effect]]        # zero or more bus effects, in series
plugin = "builtin:reverb"
mix = 1.0

[[instrument]]
plugin = "builtin:osc"
group = 0               # this instrument feeds group 0

[[instrument]]
plugin = "builtin:osc"
group = 0               # ...and so does this one
```

- `name` — display name (optional; editable in the TUI with `R` on the group
  header, default "Group N"; an empty or blank name is no name)
- `volume` — group bus output gain, applied to the member sum before the group
  effects (default: 1.0)
- `pan` — group bus stereo balance, −1 left … 0 center … +1 right (default:
  0.0; balance-style, applied to the bus output after the effect chain). In
  the TUI a group's parameter pane shows Volume + Pan as editable rows
  (`Enter` to focus, `Left`/`Right`/`Enter` to edit); `v` still edits volume.
- `[[group.effect]]` — zero or more bus effects in series (each with `mix`)
- `[[group.modulator]]` — zero or more group-scoped modulators (see below)
- Membership is set on the instruments via their `group` index, not listed on
  the group. Groups are one level deep (no nested groups).

#### Group modulators

A group can own modulators that drive parameters anywhere in the group's
world: any **member instrument's** chain, the group's own **bus effects**,
**sibling group modulators**, or a **member instrument's modulators** (a group
LFO can, e.g., sweep a member synth's own LFO rate). They are the group-scoped
analogue of the lane-scoped instrument modulators (same LFO/envelope sources
and cross-mod; see "Modulator fields" below) — they just address a wider
target set.

```toml
[[group.modulator]]
type = "lfo"            # or "envelope" — same fields as instrument modulators
rate = 0.5

# Target a member instrument's parameter. `member` is the member's ordinal
# within the group (0 = the first member in instrument-list order); `slot` is
# 0 = that instrument (default), 1..N = its effects.
[[group.modulator.target]]
member = 0
slot = 0
param = "cutoff"
depth = 0.5

# Target one of the group's own bus effects (`bus` = bus-effect index).
[[group.modulator.target]]
bus = 0
param = "mix"
depth = 0.25

# Cross-mod a sibling group modulator (mod_rate / mod_depth / mod_attack /
# mod_decay / mod_sustain / mod_release — same as lane cross-mod).
[[group.modulator.target]]
mod_rate = 1
depth = 0.3

# Cross-mod a MEMBER instrument's modulator: `member` + a `mod_*` field, where
# the mod_* value is the member's lane-modulator index. Here: member 0's
# modulator 2's LFO rate.
[[group.modulator.target]]
member = 0
mod_rate = 2
depth = 0.3
```

- `member` + `param` (+ optional `slot`) → a member instrument's parameter.
- `bus` + `param` → a group bus-effect parameter.
- `member` + `mod_*` → a member instrument's modulator field (the `mod_*` value
  is that member's lane-modulator index). Applied before the member renders, so
  the member modulator ticks with the modulated field.
- `mod_*` alone (no `member`) → a sibling group modulator.
- Member ordinals are maintained as membership changes: a member leaving the
  group drops targets that pointed at it (higher ordinals shift down); a
  member joining bumps ordinals at/after its slot. The file stores ordinals,
  not global lane indices, so it stays readable.
- **Envelope trigger:** a group envelope modulator is triggered by the union
  of its members' note ranges — it opens whenever any member would sound a
  note.
- **Application order:** group modulators are applied *before* the member
  lanes render (so members read the modulated values) and before the bus
  effects run. If a member's own lane modulator targets the same parameter, the
  lane modulator is applied later (inside the lane) and therefore wins.

### Plugin slot fields

Each plugin slot has:
- `plugin` — plugin path or URI (required)
- `preset` — preset name to load (optional)
- `params` — parameter overrides applied after preset (optional)

The instrument additionally has:
- `volume` — host-side output gain, applied before effects (default: 1.0, uncapped)
- `transpose` — note shift in semitones (default: 0)

Effects additionally have:
- `mix` — host-side dry/wet blend, 0.0=dry 1.0=wet (default: 1.0)

Load order per plugin: load → preset → params.

A number in a session file must be finite, and small enough for the 32-bit floats
the model and the audio thread hold: TOML has `nan` and `inf`, and 64-bit floats, so
`1e39` is finite to TOML and infinite once narrowed. A session that has one is refused
when it is loaded, with the place in the error
(`instrument[0].volume is not a finite number (NaN)`,
`instrument[0].volume is too big for the 32-bit numbers the audio engine holds (1e39)`).

### Modulator fields

Modulators are **lane-scoped** block-rate sources: one modulator belongs to an
instrument and can drive parameters anywhere in that instrument's chain — the
instrument itself or any of its effects — plus sibling modulators. Each target
names the chain `slot` it lands on: `0` (default) = the instrument, `1..N` =
the effects in order. In the TUI, modulators appear as the instrument's "mod
rack" (magenta), and the target selector lists every parameter in the chain
labelled by plugin (e.g. `Filter: cutoff`). Three source types are supported
(LFO, ADSR envelope, MIDI learn):

(Groups can also own modulators — same source types and cross-mod, but their
targets span the whole group: any member instrument, the group's bus effects,
or sibling group modulators. See "Group modulators" above.)

**LFO modulator** (default):

```toml
[[instrument.modulator]]
type = "lfo"            # optional, "lfo" is the default
waveform = "sine"       # sine, triangle, saw, square (default: sine)
rate = 0.5              # Hz (default: 1.0)

[[instrument.modulator.target]]
param = "cutoff"        # parameter name (resolved within `slot`'s plugin)
slot = 1               # chain slot: 0 = instrument (default), 1..N = effect
depth = 0.5             # fraction of param range, −1.0–1.0 (default: 0.5).
                        # Negative depth inverts the direction (LFO in
                        # anti-phase; a control that lowers instead of raises).
```

Modulators always serialize under `[[instrument.modulator]]` (the instrument
level), never under an effect. Older sessions that nested `[[...effect.
modulator]]` still load: those modulators migrate into the lane with their
targets pinned to that effect's slot. They belong to the lane, not to its
instrument plugin: a lane with no `plugin` (what `d` on the instrument of a lane that
has effects leaves) keeps its effects and its modulators through a save and a load. A
target on slot 0 of such a lane has no parameter to land on, and a load skips it with a
log line.

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

**MIDI-learn modulator** (a hardware CC, the pitch-bend wheel, or key
pressure / aftertouch drives the target):

```toml
[[instrument.modulator]]
type = "midi"
midi_cc = 74            # bound CC number …
# midi_pitch_bend = true  # … or the pitch-bend wheel …
# midi_aftertouch = true  # … or key pressure (channel pressure / poly
                        # aftertouch — the "press harder" dimension on MPE
                        # controllers). The three are mutually exclusive;
                        # omit all = unbound (re-arms for learning on load).

[[instrument.modulator.target]]
param = "cutoff"
depth = 1.0             # full deflection = ±depth·range around your set value
```

Create one from the TUI with **`l` (learn)** on a selected parameter (in the
param pane): it adds an armed MIDI modulator bound to that parameter, and the
next incoming CC / pitch-bend / aftertouch on any channel is captured and bound
(the audio thread notifies the main thread, which fills in and persists the
binding). It targets anything the param pane can select — plugin params
(instrument, effect, group-bus effect) and modulator fields (bind a knob to an
LFO's rate, an envelope stage, or a target's depth). In the tree it shows as
`CC 74 → cutoff` / `Pitch Bend → …` / `Aftertouch → …` (or `MIDI (learning…)`
while armed). `t` and `d` work on it like any modulator. A learned control
*combines* with your set value (see the apply math below) rather than
overwriting it.

Modulators are applied once per audio buffer; the base value tracks the
user's set value automatically. Target `depth` runs **−1.0 … 1.0**: negative
depth flips the sign of the offset below, inverting the direction (an LFO in
anti-phase, a control that lowers the parameter instead of raising it, an
envelope that dips below the base). The source types apply differently:

- **LFO** (bipolar output -1..1): `base + output * depth * range` — wobbles
  ±depth around the user's value.
- **Envelope** (unipolar output 0..1): `base - depth * (1 - output) *
  (base - min)` — the user's value is the envelope's **peak**. The parameter
  rises from below up to the base as the envelope opens; with depth 1.0 it
  spans the full min→base distance. This is what makes an amp envelope work:
  volume at 1.0 with an envelope at depth 1.0 gives silence→full→sustain→
  silence. (A purely additive envelope could only push values *above* the
  base, which on an already-maxed parameter clamps to no effect at all.)
- **MIDI learn** (additive around the base, like the LFO): the control
  *offsets* the parameter around the user's set value rather than overwriting
  it, so it combines with the base and any other modulators (an unbound /
  still-learning control contributes nothing — the base shows through). Two
  rest-aware laws by source:
  - **Pitch bend** (bipolar): `base + (output − 0.5) * 2 * depth * range` —
    centre = no change, full bend = ±depth·range around the base. This makes
    the wheel behave like a pitch/mod wheel and is the natural mapping for
    expressive (MPE) controllers.
  - **CC** (unipolar): `base + output * depth * range` — rest (0) = no change,
    full = +depth·range above the base.

Envelope behavior: note-on retriggers from Attack phase (continuing from the
current level — a quick retrigger does not restart from silence). Release
begins when all held notes are released. Linear ramps for A/D/R phases.

**Taking a cable or a modulator out** (TUI `d`, the browser's "×"; both through
`Controller::remove_edge` / `remove_modulator`) gives what it drove the value the
user set: a cable writes into its target on every buffer, and nothing else would
write the user's value back, so the parameter (or a modulator's rate, a stage of
its envelope, the depth of a cable) would stay where the cable last put it. The
values are the model's, sent as ordinary `SetParameter` / `SetModulatorRate` /
`SetModTargetDepth`… commands after the removal; a target that another cable still
drives is left to that cable, and one that went with the modulator is given
nothing. What drives what is named by node and edge ids (`Driven`), which survive
the removals around it, not by places in a rack.

**Taking an effect, a lane or a group out** (TUI `d`, through `Controller::remove_effect` /
`remove_bus_effect` / `remove_lane` / `remove_group`) takes the cables that point at what
goes out first, each by the same path as above (with the cables that drive their depths),
and then the plugins, which the audio thread hands back. For an effect or a bus effect that
is the cables that drive its parameters, a lane's own and a group's; for a lane, its group's
cables to it (its own modulators' cables go with it: a lane's modulator reaches only its
own chain), and the group's cables to the members after it are told their new ordinals; for
a group, all of its cables. What they drove goes with the plugin or the lane, so nothing is
given back — except what a group's cables drove in the lanes that stay (a member's
parameters, its modulators' rates and stages, the depths of their cables), which has the
value the user set again, unless a cable of the lane's own still drives it. The cables to the
effects after a removed one are told their new slots (a group's, their new bus indices). A
session keeps at least one lane: the last one is not taken out. `d` on an instrument that has
a plugin empties it (`clear_instrument`) and a second `d` takes the lane out.

**Limitation:** a modulator envelope shapes *parameters*, not voices — it
cannot make a note ring past the instrument's own release. Once the plugin
stops its voice (the built-in oscillators hold notes ~12 ms past note-off by
default), modulating a volume parameter has nothing left to attenuate. For
amplitude envelopes on the built-in synths use their native
`attack`/`decay`/`sustain`/`release` parameters; envelope modulators are for
sweeping parameters (cutoff etc.) while notes are sounding.

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
- Instrument with no range: receives all notes (full range)
- Overlapping ranges: notes go to all matching instruments
- **Per-note expression** (CC, pitch bend, channel pressure, poly aftertouch):
  routed by MIDI channel following the MPE convention. **Channel 1** (the MPE
  master / non-MPE global channel) is duplicated to all instruments, so an
  ordinary mod wheel / sustain / pitch wheel affects everything as before.
  **Member channels 2–16** carry per-note MPE expression and reach only the
  lane currently sounding a note on that channel — so on an MPE controller
  (e.g. a ROLI Seaboard) the pressure/glide of a note played in one
  instrument's zone can't bleed into another instrument's modulators. Each lane
  tracks its active note channels (`InstrumentLane.active_note_channels`) from
  range-filtered note on/off; the tally resets when the lane's range changes.
  **Limitation:** modulators are block-rate and lane-global, not per-voice, so
  a *chord* on one MPE instrument still collapses its notes' per-note expression
  to a single value (last channel wins) — true per-voice expression would need
  voice-level modulation.

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
transpose = "in_key"

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
- `base_note` — reference note for transposition (set to the lowest recorded
  note)
- `events` — recorded MIDI events with `frame` (sample offset), `status`
  ("on"/"off"), `note` (e.g. "C4"), and `velocity`
- `enabled` — whether pattern playback is active
- `looping` — whether playback loops while the key is held or plays once
  (default: true)
- `transpose` — `"chromatic"` (default) or `"in_key"`. Chromatic shifts every
  note by the semitone distance from `base_note` to the trigger key,
  preserving exact intervals. In-key shifts by **scale degrees** of the piano
  scale (`[piano] scale`, live-updated from the scale picker), so a C-E-G
  pattern triggered on D in C major plays D-F-A (the diatonic D minor triad)
  instead of D-F#-A. Off-scale recorded notes keep their chromatic offset from
  the scale tone below (passing tones survive); off-scale trigger keys snap to
  the scale tone below. With the Chromatic scale selected, in-key degenerates
  to plain semitone shifting.

**Behavior:**
- Press `r` on the Session tab to start recording. A 4-beat metronome count-in
  ticks first (notes played during count-in snap to frame 0), then recording
  begins. Play notes on the virtual piano or MIDI keyboard. Recording
  auto-stops after `length_beats` at current BPM.
- Hold any key to play back the pattern transposed (relative to `base_note`).
  The pattern loops while the key is held and stops on release.
- Press `r` again to overwrite with a new recording. Re-recording keeps the
  pattern's transpose mode.
- Press `Ctrl+R` to clear the pattern.
- Press `b` to set the global BPM.
- BPM is displayed in the status bar. Pattern indicators show in the chain tree:
  `▶` = pattern exists, `⏺` = recording. In-key patterns show `in-key` in the
  tree label.
- Selecting the pattern node shows its settings in the parameter pane:
  Length (beats), Enabled, Loop, and Transpose (Chromatic/In Key), toggled
  with `Left`/`Right`.

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
With `--web`, left of the CLIP light and the BPM, it says where the web interface
listens and how many browsers are connected (`web 127.0.0.1:8765 (2)`; `web (2)`
on a narrow terminal, nothing if even that would touch the tabs).

A `CLIP` light (bold red) sits on the right side of the status bar, left of the
BPM, while the output has clipped lately. The audio thread counts every buffer
whose mixed output has a sample above full scale (`|s| > 1.0`; exactly 1.0 is
not a clip) in a telemetry slot (`telemetry::MASTER.clips`, `src/telemetry.rs`);
the TUI lights `CLIP` when that count has moved since it last looked (once per
frame, `src/tui/clip.rs`) and keeps it on for 2 seconds after the last move.
The count only goes up, so any number of readers (the web runtime later) can
watch it without resetting it. Clips from before the TUI started do not light it.

### Tabs

| # | Tab | Description |
|---|-----|-------------|
| 1 | Session | Instrument and effects chain editor with parameter control |
| 2 | Piano | Virtual piano using computer keyboard |
| 3 | Oscilloscope | Real-time waveform of audio output |
| 4 | Help | Keybindings and usage reference (static, scrollable) |

TODO: the Oscilloscope tab is a placeholder and will draw the live waveform. The
Piano tab draws the keyboard, the scale and the notes being held (see "Piano
tab"), but takes no note input yet.

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
▦ Pad  vol 0.80
  ♪ Helm [LV2]            C0-B3
  ├─ fx Reverb [LV2]
  └─ ~ LFO 0.5Hz sine → Helm: cutoff (50%)
  ♪ Oscillator [Built-in]  C4-C8
  fx Compressor [LV2]  (bus)
  ~ LFO 0.2Hz sine → M0 Helm: cutoff (40%), Bus: Compressor: threshold (25%)  (bus)
♪ Bass [Built-in]
└─ fx Delay [CLAP]
```

Navigation is a single cursor through the flattened tree. Actions (`i`/`a`/`d`/`m`)
operate on the instrument containing the selected node. Modulators are the
instrument's lane "mod rack" — they appear in magenta after the effects, and a
single modulator's targets can span the whole chain (note the example LFO
driving the Helm instrument's cutoff), each labelled by the plugin it lands on.

Groups render as a Cyan header (`▦ name  vol`) with their member instruments
nested under them, then the group's bus effects (`(bus)`) and finally the
group's own modulators (magenta, also tagged `(bus)`). A group modulator's
target labels show `M<n> <plugin>: <param>` for a member instrument and
`Bus <effect>: <param>` for a bus effect.

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

Plugin parameters that declare value labels (`ParameterInfo::labels`, e.g.
the built-in filter's `type`) render as enums instead of bars — shown as
`◂ lowpass ▸` — and step one named value at a time with `Left`/`Right`
(or by clicking the left/right half of the row). Numeric entry is disabled
for enum parameters. In the session file they are still stored as plain
numeric `params` values (the index into the label list).

| Key | Action |
|-----|--------|
| `Down` / `Up` | Move selection in focused pane |
| `PageDown` / `PageUp` | Jump selection by a page |
| `Shift+Down` / `Shift+Up` | Move selected effect down/up (reorder, focus follows) |
| `n` | Add a new instrument (defaults to `builtin:osc`, opens the plugin selector to replace it; full range until set with `R`) |
| `i` | Replace instrument (opens instrument selector popup) |
| `R` | Set the selected instrument's key range (opens range popup, prefilled) — or, on a group header, rename the group (opens rename popup, prefilled) |
| `v` | Set the output volume of the selected instrument or group (host-side gain, opens value popup) |
| `a` | Add effect to the selected instrument, or — on a group/group-effect node — to the group's bus chain (opens effect selector popup). New effects default to 0.5 mix (half-wet); `builtin:filter` defaults to 1.0 (fully wet, since a half-wet filter barely filters) |
| `m` | Chain focus: add an empty LFO modulator — to the instrument's lane rack on an instrument-side node, or to the group's rack on a group / group-bus node. Param focus: modulate the selected parameter — opens a popup to bind it to a new (LFO/envelope) or existing modulator (a group bus-effect param binds a group modulator; an instrument/effect param binds a lane modulator; a **modulator's own param** binds a sibling modulator for cross-mod) |
| `l` | Param focus: MIDI-learn the selected parameter — arms a MIDI modulator and binds the next incoming CC / pitch-bend / aftertouch to it (works on plugin params and modulator fields) |
| `x` | Set the dry/wet mix of the selected effect or group bus effect (host-side blend, opens value popup) |
| `g` | Assign the selected instrument to a submix group (new / existing / ungroup, opens popup) |
| `p` | Open the preset selector for the selected instrument or effect (only if its plugin has presets) |
| `d` | Delete selected instrument/effect/modulator/pattern/group/group-effect/group-modulator (no confirmation; deleting a group ungroups its members) |
| `t` | Add modulation target (when a lane **or** group modulator is selected, opens the target selector; a group modulator's selector lists every member instrument's params, the group's bus effects, and sibling group modulators) |
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

TODO: `p` on a group bus effect (the `LoadPreset` intent already reaches those).
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

Opened by `p` on an instrument or an effect in the chain (not on a group bus
effect yet). Titled "Select Preset". Nothing opens if the plugin has no presets.

- **Top**: text input for filtering by name
- **Below**: single-column list of preset names, starting on the plugin's
  current preset
- `Up` / `Down` — navigate rows
- `Enter` — load the highlighted preset (the `LoadPreset` intent) and close popup
- `Escape` — cancel and close popup

### Value entry popup

Opened by `Enter` on a selected parameter (and also used for `b` BPM entry,
`x` effect mix entry, and `v` instrument volume entry).

- Shows parameter name, current value, and valid range
- Text input for entering a numeric value
- `Enter` — accept value and close popup; text that is not a finite number
  (`abc`, `nan`, `inf`) is ignored: the popup closes and nothing changes
- `Escape` — cancel and close popup

### Range entry popup

Opened by `R` to set the selected instrument's key range. Titled "Set Range"
and prefilled with the current range.

- Text input for a note range like `C0-B3`. Either bound may be omitted:
  `C4-` (C4 and up), `-B3` (up to B3), or empty (full range)
- `Enter` — accept and close popup (kept open on parse error)
- `Escape` — cancel and close popup

To build a split: press `n` to add a new instrument (this adds a full-range
lane defaulting to `builtin:osc` and opens the plugin selector to replace it —
cancelling the selector leaves the oscillator in place), then press `R` to set
that lane's key range. Repeat for each split zone.

### Rename group popup

Opened by `R` on a group header (range doesn't apply to groups). Titled
"Rename Group" and prefilled with the current name.

- Text input for the group's display name. Empty clears the custom name (the
  header falls back to "Group N"). The name is display-only (not sent to the
  audio thread), so it just updates the model and marks the session dirty.
- `Enter` — accept and close popup
- `Escape` — cancel and close popup

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

Opened by `t` when a modulator is selected. For a **lane** modulator it lists
candidate targets across the whole instrument chain (every plugin's parameters,
labelled by plugin, plus sibling lane-modulator parameters). For a **group**
modulator it lists every member instrument's params (`M<n> <plugin>`), each
member's modulators (`M<n> Mod<k>` — rate/ADSR/depth, for group→member-mod
cross-mod), the group's bus effects (`Bus <effect>`), and sibling
group-modulator params.

- `Up` / `Down` — navigate rows
- `Enter` — bind target and close popup
- `Escape` — cancel and close popup

### Modulate popup

Opened by `m` while focused on a parameter (the parameter-centric way to wire
modulation). Lists: "New LFO", "New envelope", and each existing modulator in
the relevant rack ("attach → …"). It works on:
- an **instrument/effect** param → operates on the instrument's **lane** rack;
- a **group bus-effect** param → operates on the **group** rack;
- a **modulator's own** param (its rate / ADSR stage / a target depth) →
  **cross-modulation**: creates or attaches a *sibling* modulator in the same
  rack that drives this field. The modulator can't modulate itself (it's
  excluded from the list), and enum/separator rows aren't modulatable.

- `Up` / `Down` — navigate rows
- `Enter` — create the chosen modulator (if new) and bind the selected
  parameter to it as a target (depth 0.5), then close
- `Escape` — cancel and close popup

This is the inverse of `t`: `t` starts from a modulator and picks a parameter;
`m` starts from a parameter and picks/creates a modulator. A group modulator
targeting a *member* instrument is wired from the modulator side (`t`), since
`m` on a member param creates a lane modulator.

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
| `l` | Toggle Locked mode: off-scale notes are dropped at the sender (Highlight, the default, only colours them) |
| `[` | Shift view octave down |
| `]` | Shift view octave up |

The scale persists in the session file under a top-level `[piano]`
section:

```toml
[piano]
scale = "C major"
locked = true           # optional; Locked mode, default false (Highlight)
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
`Up`/`Down`. With `--web` it starts with the address of the web interface, to open
in a browser (with the token, if there is one), and a warning when it listens beyond
this machine.

### Dirty indicator

When the session has unsaved changes (parameter tweaks, plugin adds/removes,
preset loads, effect reordering, a finished pattern recording, a captured
MIDI-learn control), the Session tab label shows an asterisk:
`Session *`. The asterisk clears on save.

### Session state

The main thread maintains an in-memory session model that tracks all user
changes: plugin selections, preset names, parameter values, mix values, volume,
and effect order. This model is the source of truth for `Ctrl+S` serialization.
Every user action (preset load, parameter tweak, plugin add/remove, reorder)
updates both the session model (for saving) and sends a command to the audio
thread (for playback). The dirty indicator is driven by this model. The model
types and the helpers that keep their indices in step with the audio graph live
in `src/app/model.rs`. The model, the session path, the dirty flag, the tempo,
the plugin catalog and the engine handles (command channel, plugin runtime,
notification receivers, the state shared with the note senders) belong to
`app::controller::Controller`; the TUI reaches them through `State::ctl`. The
controller folds what the audio thread reports (a finished pattern recording, a
captured MIDI-learn control) into the model, and `poll` tells the TUI what
changed. It also saves the session (`Controller::save`, `save_as`).

Every node of the model (an instrument lane, an effect, a group, a bus effect, a
modulator) has a `NodeId`: allocated by the model, never reused during a run,
not saved. The engine and the tree address nodes by position, which shifts when
others come and go; an id stays with its node. A lane's instrument plugin sits in
the lane's node. `SessionModel::locate` turns an id back into a position.

Edits that have an `Intent` go through `Controller::apply`, which changes the
model and the audio thread together and answers with `Event`s (`app::intent`,
`app::apply`; `docs/web-ui.md` §5). Built so far: `SetParam` (clamped to the
parameter's range, an enum rounded to a whole value), `SetGain` (instrument
volume, effect and bus-effect mix, group volume and pan, each with its own
range), `LoadPreset` (by preset id; the values the preset set are mirrored
into the model), `SetModulatorField` (an LFO's rate and waveform or an envelope's
stage, in a lane's modulator or a group's, clamped to `ModulatorField::range`, a
waveform rounded to its index: sine, triangle, saw, square) and `SetEdgeDepth` (a
modulation cable's depth, -1 to 1); and four that change the structure: `RemoveEdge`
(a cable, with the cables that drive its depth), `AddModulator { parent, kind }` (an LFO,
an envelope or an armed MIDI control at the end of the rack of a lane, `parent` being the
lane's node, or of a group), `AddModEdge { modulator, to }` (a cable to a plugin parameter at
depth `NEW_CABLE_DEPTH`, 0.5: where it may end is `SessionModel::param_targets`, a lane's
modulator reaching its lane's chain and a group's its members' chains and its bus effects;
refused for a port that is not a parameter, a plugin out of reach, a parameter it drives
already) and `RemoveNode` (a modulator, with its cables and those of the others that drove
its settings; anything else is refused until chain editing). An intent that changes the
structure has no event for a client to apply: `Applied::restructured` is set, `apply` calls
`resync` at once, and the subscribers are given a snapshot at the revision after the
intent's. Every applied intent bumps `Controller::rev`;
one that fails (`NotFound`, `EdgeNotFound`, `Invalid`) changes nothing. The TUI's
parameter, gain, preset and modulator-row edits use them (what each row of a
modulator's parameter pane is comes from one table, `modulator_row` in
`src/app/model.rs`, which says its range, whether it is an enum and which intent it
makes; the pane is drawn (`modulator_pane`) and the value popup opened
(`modulator_edit_state`) from it too, so a row's label, range and default are in one
place in the TUI; the Type row, which turns an LFO into an envelope, is still the TUI's
own edit). Its other edits still change the model and send the command
themselves, until their intents exist (but the TUI's `d` on a modulator, its `m` and
`l` keys and its target selectors use `Controller::remove_modulator`, `add_modulator` and
`add_target`, which the intents are made of, and `SessionModel::param_targets`); for deleting and moving an effect they
use `SessionModel::remove_effect` / `move_effect`, which keep the modulator
targets of the lane and of its group pointing at the same plugins, as the audio
thread does.

Whoever mirrors the session (the web runtime, for its browsers) subscribes an
`EventSink` (`app::subscribers`). The controller tells each about every applied
intent, and keeps what they have been told as a snapshot with every intent applied
to it (`Snapshot::apply`: the rules of `apply` read from the other side, which the web
client's `applyEvent` follows too). Because the TUI's other edits and what the audio
thread reports change the model with no intent, `Controller::resync` compares that
snapshot with the session now, and if they differ gives the sinks a snapshot to start
again from (`EventSink::resync`), at a revision of its own. The TUI loop calls it
after handling its input, and `poll` when the audio thread's reports changed the
model; with nobody subscribed it does nothing. Each TUI edit that becomes an intent
leaves less for it to find. (Compiled with the `web` feature or in tests only.)

The audio thread publishes what it is doing through `telemetry::Telemetry`
(`src/telemetry.rs`, `docs/web-ui.md` §6): a fixed array of atomic cells, one
*slot* each, that any thread can read. The model owns the `SlotAllocator` and
gives a slot to each modulator (its output this buffer), to each modulator target
on a plugin parameter (the value that parameter was set to this buffer, after
modulation; a target that drives a modulator's own field has none) and two to each
lane and group (a peak and a clip count; the master's are slots 0 and 1,
`telemetry::MASTER`). They sit on the model's nodes (`ModulatorSlot.output`,
`ModTargetSlot.effective`, `InstrumentNode.meter`, `GroupNode.meter`) and reach
the engine in the `ModTarget` or in `SetModulatorOutput` / `SetLaneMeter` /
`SetGroupMeter`. A slot is never reused, and a session that runs out of them just
has items without telemetry. The audio thread only stores (relaxed atomics, no
allocation, no lock). Lanes, groups, modulators and targets are created through
the loader or `Controller::{add_lane, add_group, add_modulator, add_target}`,
which allocate and send the slots; `SessionModel::check_ids` verifies them. The
`CLIP` light is the only reader so far.

Loading a preset clears all parameter overrides for that slot. The preset
sets every parameter to its own values, so previous overrides are discarded.
Any parameter tweaks made after the preset load are recorded as new overrides.

### Logging

All logging goes to stderr via `RawModeLogger`, same as in `play` mode. Use
`tang 2> debug.log` to capture logs to a file. No in-app log viewer.

While the TUI owns the screen it must not be corrupted by stray writes to the
terminal. Two layers guard this, active only when **stderr is a terminal**
(so `tang 2> debug.log` still captures everything):

- Rust `log` output is suppressed (`max_level = Off`) for the TUI lifetime,
  set before the background catalog scan spawns.
- The stderr **file descriptor** is redirected to `/dev/null` for the TUI
  lifetime (`tty::StderrSilencer`). Native plugin/library code — CLAP preset
  scanners, VST3 factories, ALSA/JACK probing — writes diagnostics straight to
  the fd, bypassing `log`; without this those lines land on the alternate
  screen. (`enumerate` uses the same guard.) Consequence: a panic while the TUI
  is up prints to `/dev/null`; re-run with `tang 2> debug.log` to capture it.
  Only stderr is redirected — stdout is the TUI's render surface, so a plugin
  that writes diagnostics to stdout (rare) is not covered.

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

Notes sound on key press and stop on key release. A key ends the note it began, at the octave
it began in, even if `[` or `]` was pressed while it was held; a key that is not down (no
press, or one that Locked mode refused) ends nothing when it goes up.

The browser page plays the same layout (`tang serve`, `tang --web`): `web/src/client/keyboard.ts`,
which `web/src/app/useKeyboardPiano.ts` feeds with the window's keys, so a key pressed with the
focus anywhere but in a field that takes text is the piano's. Keys are told by where they are
(`KeyboardEvent.code`), so a layout that is not QWERTY keeps the shape of the piano; `[` and `]`
change the octave (0 to 8) and the page's header says `Keys: octave 4`; a key with Control,
Command or Alt is the browser's. A note ends with the key that began it, at the octave it began
in, and what is held is let go when the window loses the focus or the page is hidden (the server
ends a closed connection's notes too). The notes go to the server as `noteOn` and `noteOff`: the
one note path, so the Piano tab's Locked mode applies to them.

## Signal chain

```
MIDI sources (hardware keyboards + virtual piano)
  → Group modulators apply (set_parameter on member + bus-effect targets)
  → For each instrument (filtered by note range):
      → Modulators apply (set_parameter on targets)
      → Instrument → volume gain → N Effects (in series, each with dry/wet mix)
      → lane meter (peak, clip count)
  → Each instrument routes to either:
      → its submix GROUP's bus  → group volume → N group effects
        → group meter, OR
      → the master mix directly
  → Sum all groups + ungrouped instruments
  → master meter (peak, clip count) → Audio output
```

Each instrument has its own effect chain. MIDI note events are filtered by the
instrument's key range; channel-1 (global) CC/pitch-bend/pressure is duplicated
to all instruments, while member-channel (2–16) per-note MPE expression is
routed only to the lane sounding a note on that channel (see "MIDI routing").

**Groups (submix buses).** An instrument can belong to a group. All members of
a group are summed, then run through the group's own end-of-chain effect chain
and output volume, before being added to the master. Ungrouped instruments go
straight to the master. Groups are one level deep (a group holds instruments,
not other groups). In the audio graph the instrument lanes stay a flat list,
addressed by `inst` index as usual; group membership is an overlay
(`InstrumentLane.group: Option<usize>`) and groups own their bus
effects/volume plus their own modulators — so all the per-instrument commands
are unchanged.

**Group modulators** are applied at the top of `process()`, before any member
lane renders: each group's modulators tick (envelopes triggered by the union
of member ranges), cross-mod resolves, and the resulting parameter writes are
collected and applied to the member lanes' plugins and the group's bus effects.
Members then read those values when they render; bus effects read them when the
group's chain runs. A member's own lane modulator, applied later inside the
lane, wins if it targets the same parameter as a group modulator.

**Reordering instruments** (`Shift+Up` / `Shift+Down` on an instrument, `Controller::
swap_lanes`) moves the whole lane: its plugins and effects, its modulators, its volume,
range, transpose and group, and its pattern, in the model and on the audio thread
(`SwapInstruments` swaps the lanes, and the pattern player is told which lane it is now).
A group's cables address their members by the lane's place among the group's lanes, so
when two lanes change places the cables are told the places the members have now
(`member_moves`, one function for the model and the audio thread), and keep driving the
same plugins. (Until the review of step 14 found it, `SwapInstruments` swapped only the
instrument plugin while the model swapped the whole node, so that after any reorder, grouped
or not, a lane's effects, modulators and volume were on different lanes in the engine and in
the view.) Taking the instrument plugin out of a lane (`d` on it, `Controller::clear_instrument`)
leaves the lane what else it has: its effects, modulators and volume.

Effects can be reordered within an instrument (or a group bus) in the Session
tab. Instrument volume is applied after the instrument's output and before its
first effect; group volume is applied to the member sum before the group's
first bus effect.

## Architecture

N+2 threads, no async (the web server, behind the `web` feature, is the one
island of async, on a thread of its own):

- **Audio thread** (cpal, JACK or ALSA on Linux) — processes the instrument
  chains, fills output buffers. Promoted to SCHED_FIFO real-time scheduling on
  first callback. The instrument list is owned (moved into) the audio callback
  closure — no mutex needed. A plugin swap mechanism exists via bounded crossbeam
  channels (send new plugin in, receive old plugin back for main-thread drop).
  The loader uses it to put the session's plugins into the audio thread, and
  `app::controller::Controller::poll` reaps the plugins handed back, in the TUI,
  in `play` and in the headless loop of `serve`.
- **MIDI thread(s)** (midir) — one per input device, all push into the MIDI channel.
- **Main thread** — runs the TUI loop (`tui::event_loop`), which waits on a channel
  of the terminal's events, the web interface's requests when there is one
  (`--web`) and a 100 ms tick, and draws; or the `play` loop, or the headless loop of
  `serve`. The virtual piano lives here and pushes into the same MIDI channel as
  hardware devices.
- **Terminal reader thread** (TUI only) — `tang-terminal`: crossterm can only be
  polled, so this thread polls the terminal (50 ms at a time, so that stopping it is
  quick), and sends what it reads, a failure included, to the loop
  (`src/tui/terminal_input.rs`). It is started when the terminal is set up and
  stopped before it is restored.
- **Catalog scan thread** (TUI only, transient) — enumerates installed plugins
  for the selector popup at TUI startup and exits when done. Sends per-format
  batches over an unbounded channel that `Controller::poll` drains into
  `app::catalog::Catalog`.
- **Splash render thread** (TUI only, transient) — owns the terminal during
  session plugin loading, animating the startup splash while the main thread
  loads plugins. Exits before `tui::run` takes over the terminal.
- **Web server thread** (`web` feature, `tang serve` and `tang --web`) — `tang-web`, a
  current-thread tokio runtime that runs the HTTP server, one task per WebSocket
  and the telemetry pusher (`src/web/server.rs`, `pusher.rs`). It never touches
  the controller: requests go to the main thread over a channel (`web::owner`),
  what the controller applies and the telemetry come back through the hub to
  each client's bounded queue, and notes go straight to `NoteInput`. In `tang serve`
  the main thread runs the headless loop (`web::runtime`); in `tang --web` the TUI
  loop answers the requests among its own business (`src/tui/web.rs`).

The shared LV2 world (`plugin::Runtime`) is created lazily on the first LV2
plugin load, not at startup — building the world scans every installed LV2
bundle, which sessions without LV2 plugins shouldn't pay for.

MIDI-to-audio communication via bounded MPSC channel (crossbeam-channel, capacity 1024).

Audio thread logging uses `log::debug!()` for per-event messages (filtered out at default
Info level) and `log::info!()` only once on first callback. No real-time safety issue at
default log levels.

TODO: Mirror audio output into a lock-free ring buffer for the oscilloscope display.

MIDI devices are hot-pluggable — `Controller::poll` (called from the TUI loop, the
`play` loop and the headless loop of `serve`) looks for new devices about once a
second (not in `serve` when there was no MIDI subsystem at startup).

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

**Null backend.** `--audio-device none` (`audio::NULL_DEVICE`) opens no host and
no device: a thread named `tang-null-audio` renders a block every
`buffer / sample rate` seconds (5.3 ms at 256 frames and 48 kHz) with the same
code the real callback runs (`audio::Renderer`) and throws the samples away. MIDI,
the meters and the telemetry behave as with a sound card, so CI, headless runs
(`tang serve`, later) and tests can drive and measure the engine, and a loud
session can be tried without making a sound. The cadence is kept without drift,
and a stall is not caught up with a burst of blocks. `stop` hands the plugins back
to the stopping thread to drop, which the real stream does not promise. It runs
at normal priority: no real-time promotion.

## Plugin compatibility

Plugin loading is behind a trait. Three formats supported:

- **LV2** — via livi.
- **CLAP** — via clack-host.
- **VST3** — via vst3-rs (coupler-rs/vst3-rs) with libloading.

### Built-in plugins

- **Oscillator** (`builtin:osc`) — one polyphonic oscillator with a
  selectable waveform, per-voice ADSR amplitude envelope. Parameters:
  `waveform` (enum: sine/triangle/square/saw), `detune` (±24 semitones / ±2
  octaves — wide so a modulator can sweep pitch dramatically; use small values
  or low modulator depth for subtle detuning), `volume`
  (0–1, smoothed ~5 ms), `attack`/`decay`/`release` (0–10 s), `sustain`
  (0–1), `pan` (−1 left … 0 center … +1 right, balance-style so center leaves
  both channels at full; smoothed ~5 ms). Envelope defaults (A=4 ms, S=1.0, R=12 ms) reproduce the original
  click-guard ramps; envelope times are floored to 2 ms so extreme settings
  stay click-free. Voices ring until their release completes. The old
  `builtin:sine`/`builtin:triangle`/`builtin:square` IDs still load (as
  aliases that start on that waveform) so older sessions keep working, but
  enumeration and the selector show only the unified `builtin:osc`.
- **Reverb** (`builtin:reverb`) — stereo effect with presets.
- **Filter** (`builtin:filter`) — stereo multimode filter. Parameters:
  `cutoff` (0–1, exponential 20 Hz→20 kHz so sweeps move musically),
  `resonance` (Q 0.5–10, default 0.707), `type` (enum, switchable live),
  `drive` (1–10, input gain into the analog models' saturation; no effect
  on the clean types). Types:
  - `lowpass` / `highpass` / `bandpass` / `notch` — clean 12 dB/oct
    trapezoidal state-variable filter, stable under cutoff modulation.
  - `ladder` — Moog/Mother-32-style 4-pole transistor ladder (24 dB/oct,
    warm, self-oscillates at full resonance).
  - `acid` — TB-303-style diode-ladder flavor: 18 dB/oct tap with
    high-passed feedback, so the squelch rides on top while bass stays put.
  - `ms20` — Korg MS-20-style Sallen-Key (12 dB/oct) with hard-clipped
    feedback; aggressive, screams at high resonance.

  The ladder/acid use a zero-delay-feedback (TPT) topology with `tan`-
  prewarped tuning: the resonant feedback is resolved instantaneously (no
  one-sample delay), so the cutoff does not wander with input level, the
  resonant peak/self-oscillation track the knob proportionally, and acid
  keeps its resonance down in the bass. The single tanh on the resonant
  feedback bounds self-oscillation while staying linear (correctly tuned)
  for small signals. `drive` saturates the input (`tanh`), adding harmonics
  — it does not auto-attenuate, so heavy drive gets louder/brighter like a
  real filter (and like a real filter, harmonics that fall above the cutoff
  are then filtered out, so driving a closed filter is cleaner than driving
  an open one).

  **Cutoff convention:** `cutoff` marks the filter's −3 dB point for *every*
  type — the standard definition, consistent with the clean SVF modes — so
  switching `type` at the same knob keeps the same brightness. Because a
  3–4-pole cascade's −3 dB point sits well below its per-stage corner, the
  analog modes raise the internal corner by a per-mode tuning multiplier
  (`LADDER_TUNE`/`ACID_TUNE`/`MS20_TUNE`) to compensate. A consequence: at
  high resonance the resonant peak sits somewhat above the knob (the peak
  and the −3 dB point are an octave apart in a steep ladder and can't both
  coincide with the knob) — it still tracks the knob proportionally, so
  sweeps stay musical.

  All analog models run at 2× internal oversampling; switching type resets
  filter state. For the analog types, `resonance` maps onto the model's
  feedback range (full = just past self-oscillation). Cutoff/resonance/
  drive are smoothed ~5 ms. Defaults to a fully open clean lowpass
  (transparent). The filter defaults to 1.0 mix (fully wet) when added —
  unlike other effects (0.5) — because at 50% the dry path passes the
  frequencies the filter removes (a highpass would barely change the sound).
  Adjust with `x` if a parallel/blended filter is wanted. The `#[ignore]`d tests `diag_full_report` and
  `diag_cutoff_consistency` are a measurement harness (Goertzel-based
  magnitude/THD/slope/aliasing) for iterating on the filter voicing.

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

- **Channel count validation**: the chains are stereo (`plugin::chain::CHAIN_CHANNELS`,
  2), and an effect must have as many audio outputs. The audio thread turns away
  one that has not (it logs why and hands the plugin back), and
  `Controller::add_effect` / `add_bus_effect` refuse it first, so the model never lists
  an effect the engine does not have. The instrument may have more outputs (the first
  two are used, the rest discarded).

  TODO: a session file that names an effect with other outputs still loads, and its
  model keeps the effect that the audio thread dropped; refuse such a session at load.

## TUI framework

The TUI is built on ratatui 0.30 with the crossterm backend. We target the
latest ratatui release and adopt new APIs as they become available. When
upgrading ratatui, update this version note.

### TUI tests

`src/tui/golden_tests.rs` builds a `State` by hand around a `Controller` over
stub engine channels (`app::testutil::stub_engine`), drives it with real key
events through `process_event`, and checks two things: the rendered frame
(characters **and** styles, via `insta` and ratatui's `TestBackend`; snapshots
in `src/tui/snapshots/`) and the commands the TUI sent to the audio thread. After
an intentional UI change run `INSTA_UPDATE=always cargo test golden` and review
the snapshot diff.

## Web UI

A browser front-end to the same Tang process (local or LAN). Audio, plugins
and MIDI stay native — the browser is a control surface, never an audio
engine — and the TUI stays first-class. Full design, protocol sketch and the
phase 0 step list: [`docs/web-ui.md`](docs/web-ui.md). Summary:

- **Controller** (`src/app/`): a plain struct that owns the live session
  model, the engine handles and the plugin catalog. Front-ends call
  `apply(Intent)` and receive `Event`s. The TUI is migrated onto it feature by
  feature, so no feature ever has two implementations. Built so far: `SetParam`,
  `SetGain`, `LoadPreset`, `SetModulatorField`, `SetEdgeDepth`, `RemoveEdge`,
  `AddModulator`, `AddModEdge`, `RemoveNode` and `Save` (see "Session state"). Effects are
  added by `Controller::add_effect` / `add_bus_effect` (loaded here, from the plugin
  catalog only, at the end of a lane's chain or a group's bus; the TUI's selector asks
  them), which the intent `AddEffect` wraps (browsers choose from the catalog the server
  tells them; the mock engine's nodes for the built-in effects are a fixture Rust writes,
  `web/src/client/fixtures/builtins.effects.json`).
- **Model**: graph-shaped (nodes, ports, edges, layout) with stable `NodeId`s
  (and `EdgeId`s for modulation edges); today's lane/group/modulator structure
  is projected into it: `SessionModel::graph()` (`src/app/graph.rs`) gives the
  nodes (instrument, effect, modulator, group, the master as `NodeId::MASTER`),
  their `parent`s and the edges between ports (`audio`, `member`, `modulation`
  with the target's depth as `amount`), and `Controller::snapshot()`
  (`src/app/snapshot.rs`) is that plus the revision, the telemetry layout and
  what belongs to the session as a whole (dirty, file name, tempo, piano). Node
  layout is to be stored in the session TOML as optional `ui = { x, y, pinned }`
  tables that the engine and the TUI ignore.
- **Telemetry**: the audio thread publishes modulator outputs, effective
  (modulated) parameter values and meters through lock-free slots; the web
  runtime streams them to the browser as binary frames. The slots, the engine
  side and the layout that says what each slot means
  (`SessionModel::telemetry_layout()`, `src/app/layout.rs`) are built (see
  "Session state"); `web::pusher` sends them to the browsers as binary frames (see "Server").
- **Protocol**: the types that cross the socket (`Intent`, `Event`,
  `StampedEvent`, `Graph`, `TelemetryLayout`, `Snapshot`) derive serde — camel
  case, enums as objects tagged by `type` or `kind` — and, under the `web`
  feature, ts-rs. The TypeScript in `web/src/protocol/generated/` is committed;
  a test (`app::protocol::typescript`, run by the CI job `typescript`) fails
  when it is not what the types generate. After changing a wire type:
  `TANG_UPDATE_TYPES=1 cargo test --features web typescript`, and commit the
  result. `examples/tracer.toml` has a golden JSON snapshot
  (`src/app/snapshots/`); review the diff when it changes
  (`INSTA_UPDATE=always cargo test the_tracer`).
- **Server** (`src/web/`, built; step 10): `axum` on its own thread behind the
  `web` Cargo feature, started by `tang serve` or by `tang --web`. Routes `/` (the
  embedded interface) and `/ws`; one WebSocket per client with a
  bounded outgoing queue (a client that falls behind is told and closed, and
  reconnects from a snapshot); loopback by default, `Origin`/`Host` checked on every
  request, a token for non-loopback binds, limits on message size, request rate,
  connections and write time. The audio, MIDI and controller threads stay
  synchronous. Messages: `app::protocol` (`ClientMessage`, `ServerMessage`); the
  plugin catalog (what can be added; step 19) follows a client's snapshot and is told again
  as the scan finds more (`catalog { rev, scanning, entries }`: ids and names, never where a
  plugin is installed; `tang serve` starts the scan, the TUI its own); the
  telemetry frames are made by `web::pusher` at 30 Hz for the channels a client
  asked for. `NoteInput` (`src/note_input.rs`) is the one note path of hardware
  MIDI, the virtual piano and the browser: the Piano tab's filter, the held-notes
  bits and the channel to the audio thread, with the notes a WebSocket holds ended
  when it closes. Tests: `web::` (in-process, with a real WebSocket client) and
  `tests/serve.rs` (the real binary on the null backend). See `docs/web-ui.md`
  §3.2, §7 and §8 for the as-built notes.
- **Frontend** (`web/`, built; steps 11 to 16, 18 and 19): TypeScript (strictest settings) +
  React + Vite, a framework-agnostic client layer (`web/src/client/`: reducers,
  telemetry buffers and slot readers, the connection with its reconnection and token, a
  vanilla zustand store, the frame loop, `Drive` (what a control shows while the server
  catches up, one intent a frame), the engine's modulation laws for the knobs' rings, and
  the mock engine, which speaks the protocol in the page), generated protocol types,
  design tokens as CSS variables (`web/src/ui/tokens.css`), the controls
  (`web/src/ui/`: `Knob`, `Choice`, `LiveBar`, `Meter`, which draw live values by
  writing to the DOM, never through React state) and the patch canvas
  (`web/src/app/patch/`: `toFlow`, a pure layout of the graph, drawn with React Flow:
  a strip for each instrument, its modulators above it, cables, a node's knobs bound to
  the session; step 14 added the controls of a modulator: an LFO's rate and waveform and an
  envelope's stages on the node (`Knob scale="log"` for a rate and a time, which run over
  decades), and a slider for the depth of each cable at its middle; step 15 added building:
  a "+" menu on every lane and group (`MenuButton`) that adds an LFO, an envelope or a MIDI
  control, a "×" on every modulator and every cable, a list at the foot of a modulator that draws
  a cable to any parameter it can drive (for a keyboard: a `MenuButton` with the plugins as
  headings, which finds an item by its first letters), and a cable dragged from a
  modulator's output to a parameter's port, the ports that take it marked while it is
  dragged; `client/editor.ts` is where the page asks for them and `Notices` /
  `NoticeBoard` is where it says what the server refused; where a cable may end is the
  controller's rule, read from the graph by `client/reach.ts` and pinned to the controller's by
  a fixture it writes, `web/src/client/fixtures/reach.json`). Step 16 made the page's keys a
  piano (`client/keyboard.ts`, `app/useKeyboardPiano.ts`; see "Virtual piano"). Step 18 made the
  page save the session (`app/SessionBar.tsx`: the file's name, "Saved" or "Unsaved changes", a Save
  button and Control+S / Command+S, which keep the browser from offering to save the page; `Editor.save()`).
  The client's store also keeps the server's plugin catalog (`ClientState.catalog`, step 19), and
  every lane and group has an "fx" menu that puts an installed effect in its chain (`client/catalog.ts`,
  `AddEffectMenu`; `Editor.addEffect`). What the controller
  clamps to (the ranges, what a new modulator starts as, the order of the waveforms) is
  generated into `web/src/protocol/generated/constants.ts` with the types and read by the mock
  engine and the controls, not copied. The page is the connection state, the canvas and the session as JSON
  (closed). `cd web && npm ci`, then `npm run dev` (Vite on 5173, forwarding `/ws`
  to `tang serve --web-origin http://localhost:5173`; `?engine=mock` in the address
  needs no server, `&scene=stress` serves eight lanes with 32 LFOs and 64 driven
  parameters and `&scene=lanes` two lanes with an LFO each), `npm test` (Vitest), `npm run lint`, `npm run typecheck`,
  `npm run storybook`, `npm run build` (→ `web/dist`), `npm run e2e` (Playwright,
  one test at a time, since they share the real program's session, which is a copy of the tracer in
  `web/.e2e-session/` so that a test that saves does not write the example; needs
  `cargo build --features web`; `npm run e2e:install` once; they include the
  performance tests of docs/web-ui.md §9.4, which need the Vite dev server for the
  count of React's commits). The program serves
  `web/dist` itself (`src/web/assets.rs`, `rust-embed`: read from the disk in a debug
  build, inside the binary in a release build, so **run `npm run build` before
  `cargo build --release --features web`**; without it the program serves a placeholder
  that says so). `web/dist/` keeps a `.gitkeep` so that the program builds without
  Node. The tracer's snapshot in `web/src/client/fixtures/` is generated:
  `TANG_UPDATE_TYPES=1 cargo test --features web` rewrites it along with the types (and
  `reach.json`, next to it, which says where the controller let a cable be drawn, and
  `builtins.catalog.json`, the built-in plugins as the catalog lists them, which the mock serves).
- **TUI and web together** (built; step 13, `docs/web-ui.md` §3.4): `tang --web`
  runs the TUI and the web interface on one controller, on the main thread. The loop
  applies a browser's requests and reacts to what they changed (a value a browser set
  is on the TUI's screen with the next frame, and a step the TUI takes starts from
  it); what the TUI changes through intents reaches the browsers as events, and the
  rest of what it does by a snapshot (`Controller::resync`, see "Session state").
  The tree is addressed by position and a browser can add or take out a node above
  the cursor, so the TUI keeps track of nodes and not of places: its cursor stays on
  the node it was on (found by its id, with the kind of entry: a lane's pattern is
  an entry of the lane's node, as its instrument is), and a popup that holds a place
  in the tree (a lane's range, a group's name, a selector of plugins, of targets or
  of presets, the modulate popup, the group popup) is closed when the entries of the
  tree have changed, as it would act on whatever is at its place now. A value being
  typed goes to the node under the cursor, which follows its node, and is closed
  only if that node is gone; a cable that is added or taken out moves no place and
  closes nothing.
  Tested by running the real loop with a real server and a WebSocket client that
  presses keys by sending them to the loop (`tui::golden_tests::with_web`), and by
  hand with the real program in a pty and a real browser (18 checks, both ways).
- **Scope**: track A first — a "rack-as-patch" canvas over the existing engine
  (serial audio chain, block-rate lane-global modulation, cables = modulation
  targets). A patchable synth engine (`builtin:patch`, track B) comes later.

Phase 0 (foundations) is built: the 13 steps of `docs/web-ui.md` §12. Steps 14 and 15 did the
first of phase 3 ahead of phase 1 (D18): a modulator's settings and a cable's depth are turned,
modulators and cables are added and taken out, in the browser and in the TUI. Step 16 made the keys
of the computer a piano in the page, so that what is built can be tried by ear. Phase 1 (session
parity) is next, §11.
TODO: `ui` layout fields in the session format (the canvas lays itself out until then; nodes cannot be moved).
TODO: make the Type row (LFO to envelope) an intent: until then a browser is given a snapshot
for it (`Controller::resync`). Draw cables that drive a modulator's rate, stages or depths from
the browser (`AddModEdge` takes a parameter port only; `RemoveEdge` takes any cable out).
TODO: `clear_instrument` (the first `d` on an instrument) leaves the cables to the parameters of
the plugin it empties on the lane, pointing at nothing (a lane with no instrument has no slot 0);
what to do with them when an instrument is replaced is open (f51: re-find a cable by its parameter's
name, or take it out).
TODO: telemetry slots are never reused (4,096 of them): a session that adds modulators and cables
for long enough runs out, and later cables have no live dot.
TODO: the canvas does not move to show a node that was added.
TODO: the slider of a cable's depth is at the middle of the cable, which can be over a node's
edge, and two cables with the same middle put theirs over each other; user-placed nodes and
sliders that move along the cable would settle it.
TODO: patterns as graph nodes (they have no `NodeId` yet).
TODO: events of their own for what the audio thread reports to the controller (a
finished pattern recording, a captured MIDI-learn control) and for what the TUI
changes without an intent: until then connected browsers are given a snapshot
(`Controller::resync`).
TODO: the events of one intent share its revision, which a client takes for a repeat
if there is more than one (every intent makes one event today).
TODO: the held-notes telemetry channel (a 16-byte bitset, on change) and keep-alive
pings for WebSockets.

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

The `web` Cargo feature (off by default) is the browser front-end: `ts-rs`
(which generates the TypeScript of the protocol types), the server (`axum`,
`tokio`) and `ctrlc` for stopping `tang serve`. `tokio-tungstenite` and
`futures-util` are in it for the tests' WebSocket client (axum's `ws` is built on
the same crate). See "Web UI".

CI runs clippy + tests on all three platforms:

- **Linux** — full default features (LV2 + VST3 + CLAP), `apt install liblilv-dev libasound2-dev libjack-dev`
- **macOS** — full default features (LV2 + VST3 + CLAP), `brew install lilv`
- **Windows** — `--no-default-features --features vst3` (CLAP is not feature-gated, so it's always built; LV2 is unsupported)

Each of them then also runs clippy (`--all-targets`) and the tests with `--features
web` added, which includes `tests/serve.rs`, the real binary on `--audio-device
none` (the Ctrl+C test is Unix only). The job `typescript` checks that the
generated TypeScript is current.

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
