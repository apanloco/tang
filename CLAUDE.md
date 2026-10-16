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
TODO: `--debug` is a global flag, so `play` and `serve` take it, but only the TUI draws the tally: there the monitor is
written by the MIDI input thread and read by nothing, and the flag changes nothing that can be seen. `play` could print the
lines the monitor keeps, or the two could refuse the flag.

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
  - `mute`, `solo` — muted / soloed (default: false, written only when true; see
    "Mute and solo")
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
- `mute`, `solo` — muted / soloed (default: false, written only when true; see
  "Mute and solo")
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
  not global lane indices, so it stays readable. A lane that leaves its group
  (or goes to another) has the group's cables to it taken out as `RemoveEdge`
  takes a cable out, and what they drove has the value the user set again
  (`Controller::set_lane_group`; before step 31 a parameter stayed at the
  value a cable of the group had last put there).
- **Envelope trigger:** a group envelope modulator is triggered by the union
  of its members' note ranges — it opens whenever any member would sound a
  note.
- **Application order:** group modulators are applied *before* the member
  lanes render (so members read the modulated values) and before the bus
  effects run. If a member's own lane modulator targets the same parameter, the
  lane modulator is applied later (inside the lane) and therefore wins. What
  wins is the lane's whole write: each side writes the parameter from the value
  the user set plus its own modulators' offsets, so the group's offset is not
  added to the lane's, and a lane's MIDI control that is not bound yet, which
  adds nothing, still writes the user's value over the group's (pinned in
  `plugin/chain.rs`, `a_lane_modulator_and_a_group_modulator_on_one_parameter_…`
  and `a_lane_control_that_is_not_bound_yet_…`). The page's ring follows it
  (`modulationsOn`, f134), and so does the mock engine's value of a parameter.

### Mute and solo

A lane and a group can be muted (`mute = true`) or soloed (`solo = true`), keys of its table
beside `volume`, written only when on. In the model they are `InstrumentNode::muted` / `solo` and
`GroupNode::muted` / `solo`; the audio thread has them from `GraphCommand::SetMute`, `SetSolo`,
`SetGroupMute` and `SetGroupSolo`, which the loader sends for what a file turns on.

- A **muted** lane or group is never heard. A mute wins over a solo: a lane that is muted and
  soloed is silent, and (since it is soloed) it silences what is not soloed all the same. Its
  notes, modulators and pattern go on, and a muted group's members and bus effects go on, so
  what is heard when it is brought back is where it would be (a note that is held through the
  mute is sounding, a reverb's tail is where it would be).
- While **anything is soloed** (a lane or a group) only what is soloed is heard: a soloed lane,
  a lane in a soloed group, and a group that is soloed or has a soloed lane in it (its bus is
  heard for that lane, with its volume and effects, and its other members are not). A lane's
  mute is its own: a lane in a muted group is not itself muted (its meter still reads); the
  bus is cut.
- A lane or a bus is let go of, or brought back, over one buffer by a linear ramp of its gain
  (`fade` in `plugin/chain.rs`: 5.3 ms at 256 frames), so that it does not click. A gain that is
  1 and stays so costs nothing, and one that is 0 and stays so puts zeros and does not multiply by
  them, so that a plugin that puts out `NaN` is silenced by a mute. The lane's meter and the group's
  are after it (a muted lane stops clipping); the scope taps of a node are before it (they show the
  signal of the node, not what is heard). The lane's mute and solo go where the lane goes (a swap
  of two lanes, undo of a removal: the lane is handed back whole).
- The terminal (`M` and `S`, see the key table) and the page (the "Mute / Solo" cell of a lane's
  and a group's node, which draws what cannot be heard dimmed) set them through the intents
  `SetMute` and `SetSolo`, which are steps of the history.

TODO: a click that solos one lane and takes the others' solo off (the Shift or Alt click of a
console), and a button that takes every solo off: the buttons turn each one by itself, as the keys do.
TODO: a muted lane (or one that is not heard for a solo) still renders its plugins every buffer, for
what a held note and a tail are when it is brought back; one that has been silent for a while could
stop, at the cost of that.

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
An LFO's `rate` below 0 is refused in the same way, wherever the LFO is (a lane's, a group's, an older
session's modulator nested in an effect): `instrument[0].modulator[0].rate is below 0 (-1): an LFO cannot go
round backwards` (`session::reject_backwards_lfos`; the phase of such an LFO counts down from 0 and its
shapes leave the -1 to 1 that a cable's law assumes: a saw from -3 to -1, f147). Zero is an LFO that
stands still, a rate above the knob's 50 Hz is the file's to say, a step sequencer's rate is refused in the same way
(`a step sequencer cannot go round backwards`), and a modulator that is neither (an envelope, a MIDI control, a macro)
has no rate that is read. The steps of a step sequencer are refused when the list is empty or has more than 16
(`instrument[0].modulator[0].steps has 17 values: a step sequencer has 16 steps at most`, `session::reject_bad_steps`).

### Modulator fields

Modulators are **lane-scoped** block-rate sources: one modulator belongs to an
instrument and can drive parameters anywhere in that instrument's chain — the
instrument itself or any of its effects — plus sibling modulators. Each target
names the chain `slot` it lands on: `0` (default) = the instrument, `1..N` =
the effects in order. In the TUI, modulators appear as the instrument's "mod
rack" (magenta), and the target selector lists every parameter in the chain
labelled by plugin (e.g. `Filter: cutoff`). Five source types are supported
(LFO, ADSR envelope, MIDI learn, macro, step sequencer):

(Groups can also own modulators — same source types and cross-mod, but their
targets span the whole group: any member instrument, the group's bus effects,
or sibling group modulators. See "Group modulators" above.)

**LFO modulator** (default):

```toml
[[instrument.modulator]]
type = "lfo"            # optional, "lfo" is the default
waveform = "sine"       # sine, triangle, saw, square, random, smooth (default: sine)
rate = 0.5              # Hz (default: 1.0; not below 0: a file with a negative rate is refused)

[[instrument.modulator.target]]
param = "cutoff"        # parameter name (resolved within `slot`'s plugin)
slot = 1               # chain slot: 0 = instrument (default), 1..N = effect
depth = 0.5             # fraction of param range, −1.0–1.0 (default: 0.5).
                        # Negative depth inverts the direction (LFO in
                        # anti-phase; a control that lowers instead of raises).
```

The shapes are `sine`, `triangle`, `saw` (rising), `square` and two that have a number of their own for every
cycle of the LFO: **`random`** (also `s&h` or `sh`: sample and hold) puts out the number of the cycle it is in, from
−1 up to but not including 1, and holds it until the next cycle begins; **`smooth`** glides from the number of
each cycle to the number of the next (a smoothstep, so that it eases at both ends and the cycles join without a
corner). Like the others they are bipolar, so the law of an LFO below is the same for them. The numbers are
not drawn from a generator that has state: a number is the top 24 bits of a hash of the modulator's *seed* and of how
many times it has gone round (`plugin::chain::noise`), so the same cycle of the same seed always has the same number, and
a session that is loaded again plays the same numbers. The seed is the order in which modulators came into the audio graph
(one counter for the lane modulators and the group modulators alike, `AudioGraph::next_modulator_seed`): a session loads
its modulators in the order of its file, so each has the seed it had the last time, and one that is added later, or put
back by undo, has the next one and so draws other numbers than it did before (the seed is not in the session file). The
cycles are counted as the phase wraps (`Modulator::tick`), so that turning the rate does not start the numbers again, and a rate
that goes round more than once in a buffer counts every cycle. The page's mock engine draws the same numbers from the same hash
(`web/src/client/noise.ts`, pinned to the engine's by a table that the engine writes, `fixtures/noise.json`), with the node's
id for the seed.

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
pressure / aftertouch drives the target; or the velocity or the pitch of the
notes the lane plays):

```toml
[[instrument.modulator]]
type = "midi"
midi_cc = 74            # bound CC number …
# midi_pitch_bend = true  # … or the pitch-bend wheel …
# midi_aftertouch = true  # … or key pressure (channel pressure / poly
                        # aftertouch — the "press harder" dimension on MPE
                        # controllers) …
# midi_velocity = true  # … or the velocity of the last note the lane plays …
# midi_key = true       # … or its number (the pitch). The five are mutually
                        # exclusive (the first of them, in this order, wins);
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
`CC 74 → cutoff` / `Pitch Bend → …` / `Aftertouch → …` / `Velocity → …` /
`Key → …` (or `MIDI (learning…)` while armed). `t` and `d` work on it like any modulator. A learned control
*combines* with your set value (see the apply math below) rather than
overwriting it. The whole path (a controller, the MIDI thread, the audio
thread's capture, the notification, the controller's binding, the browsers told,
the parameter following the knob, the saved `midi_cc`) is tested with a virtual
MIDI port in `tests/serve.rs` (`a_control_that_learns_…`: `#[ignore]`d, since it
needs CoreMIDI on a Mac or ALSA's sequencer on Linux: `cargo test --features web
-- --ignored`).

**Macro modulator** (a knob that a person turns, which nothing else moves):

```toml
[[instrument.modulator]]
type = "macro"
value = 0.5             # 0 … 1 (held to it when the session is loaded; default 0: at rest)

[[instrument.modulator.target]]
param = "cutoff"
depth = 0.5             # `value` times this, of the range, above your set value
```

A macro is one number, `value`, from 0 to 1, that is set from the TUI (the first row of its pane: `Left` / `Right`, or `Enter` and a number), from the page (a knob
on its node) or in the file, and it drives every target that has a cable from it as a CC does: at rest (0) it adds nothing, at full (1) it adds `depth` of the
range above the value the user set (a negative depth takes it below). A new one is at rest, so it moves nothing until it is turned. One knob drives many parameters,
each scaled by the depth of its own cable; a macro can drive the rate, a stage or the depth of a cable of another modulator too (cross-mod). The audio thread has
nothing of a macro but its value, so a change of it is a macro of the new value given to the engine in place of the old one (`SetModulatorSource` /
`SetGroupModulatorSource`, `controller::macro_command`). It is made in the TUI from a parameter (`m`: `New macro → cutoff`; in the group's rack for a bus effect's
parameter), in the page from the menu of a knob (`~`: "New macro") or from the "+" of a lane or a group ("Macro"), by `Modulate` / `AddModulator { kind: macro }`, and
is `~ Macro 0.50 → Filter: cutoff (50%)` in the tree and `Macro` (its value, the "Targets" separator and the depth of each cable) in the pane.
TODO: no cable drives a macro's value (`ModulatorField::Value` has no port, and the pane's `m` on its row opens nothing): a MIDI control bound to a macro would turn it
from a hardware knob, which is what a macro is for on a stage.

**Step sequencer modulator** (sixteen bars that go by at a rate, and drive what they have cables to by the value of the bar they are at):

```toml
[[instrument.modulator]]
type = "steps"
steps = [0.0, 0.5, -0.25, 1.0]  # 1 to 16 values, each -1 … 1 (held to it when loaded); a step that is not given is 0; no list at all is a staircase
length = 4                      # how many steps are in use, 1 … 16 (default: as many as `steps` has values, sixteen if none)
rate = 8.0                      # steps a second (default 8: an LFO's default is 1, so `rate` is an option in the file's types and the loader says which)
glide = 0.0                     # 0 … 1: the part of each step, at its end, that goes to the value of the next (default 0: hard steps)
retrigger = true                # whether a note starts it again at its first step (default true)

[[instrument.modulator.target]]
param = "cutoff"
depth = 0.5
```

A step sequencer goes through its first `length` steps at `rate` steps a second, and round again, and is the value of the step it is at (or, in the last `glide` of a
step, on its way to the next one's; the last goes to the first, and not to a step that is not in use): `steps_output` in `plugin::chain`, which the page's `stepsOutput`
(`client/steps.ts`) is pinned to by a table the engine writes (`fixtures/steps.json`). It drives as an LFO does, bipolar: `base + value * depth * range`. A new one
(TUI `m`: "New step sequencer"; the page's "+" menu on a lane or a group and the `~` menu of a knob; `AddModulator` / `Modulate` with the kind `steps`) is a staircase from
-1 to 0.875 in sixteen steps, at 8 Hz, with no glide, restarted by a note. Its rate can be driven by a cable (the rate has the port an LFO's has: `ModulatorField::Rate`);
the glide, the length and the retrigger are a person's, and no cable ends at them. The steps past the length keep their values, so that a sequence that is shortened and
lengthened again is as it was. With `retrigger` on any note-on with a velocity, on any channel, puts it at its first step (a lane's: the notes the lane takes; a group's: the
notes of any of its members). Where it is, the step as the whole part of the number and how far into it as the fraction, is published for a playhead in a telemetry slot
of its own after its output's (`ModulatorSlot::position`, `SlotKind::ModPosition`, `SetModulatorPosition`: `None` if the slots had run out, and then the page has no playhead).
All sixteen values are one intent, `SetSteps { node, values }`, told as `StepsSet` (each held to -1..1; a list that is not sixteen long, a value that is not a number and a node
that is no step sequencer are refused, with nothing changed), so that a stroke across the bars is one step of the history ("set steps", `Key::Steps`); the engine is given
them with the length, the glide and the retrigger in one command (`SetModulatorSteps`, `controller::steps_command`) and the rate by the LFO's (`SetModulatorRate`). In the tree
it is `~ Steps 16 8.0Hz → Filter: cutoff (50%)` (the steps in use, and the rate), and its pane, "Step sequencer", has the rate, the glide, the length, a retrigger row (off and on, a list), a row for each of the sixteen steps
(`Left` / `Right` move one by 5% of its range, and `Enter` takes a number, held to -1 … 1) and the targets. The page draws it as a node with four controls and sixteen bars with a playhead
(`ui/StepBars.tsx`, see "Web UI").
TODO: the terminal's pane does not dim the steps past the length and has no playhead (the page has both): its rows are text, and the terminal does not read the telemetry.
TODO: a step is held to a hundredth when it is painted on the page, which is a hundredth of the range of what it drives at depth 1 (a detune of ±24 semitones is 0.48 of a
semitone, and 0.12 at depth 0.25: out of tune by twelve cents; the terminal's `Enter` takes a number to as many places as a person types): a sequencer for a melody wants a
grid of semitones for what it drives, and a rate that follows the tempo (see `docs/web-ui.md`, step 57, "Limits").

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
  still-learning control contributes nothing — the base shows through; nor
  does a bound one that has sent nothing yet: it starts at rest, a CC or
  pressure at 0 and the pitch-bend wheel at its centre, `MidiBindSource::rest`).
  The rest-aware laws by source:
  - **Pitch bend** (bipolar): `base + (output − 0.5) * 2 * depth * range` —
    centre = no change, full bend = ±depth·range around the base. This makes
    the wheel behave like a pitch/mod wheel and is the natural mapping for
    expressive (MPE) controllers.
  - **CC** (unipolar): `base + output * depth * range` — rest (0) = no change,
    full = +depth·range above the base.
  - **Velocity** (unipolar, as a CC): the velocity of the last note-on the lane
    plays, over 127; `base + velocity/127 * depth * range`. Before a note it is
    at rest (0), and it keeps the last note's velocity when the note ends (a
    release goes by its own note's velocity). A note-on with no velocity is a
    note-off and does not count.
  - **Key** (bipolar around middle C): the number of the last note-on over 127,
    `base + clamp((note − 60) / 60, −1, 1) * depth * range` — note 60 (middle C)
    adds nothing and five octaves either way are the whole depth (`KEY_CENTRE`,
    `KEY_SPAN`; the page reads both). It rests at note 60 before any note.

  Velocity and key are not learned (no controller sends them as a control): they
  are bound when they are made: `m` on a parameter in the TUI ("New velocity" /
  "New key"), the page's "+" menu on a lane or group and the `~` menu of a knob
  ("Velocity", "Key"; the intents `AddModulator` / `Modulate` with the kinds
  `velocity` and `key`), or `midi_velocity` / `midi_key` in a file. Like
  every modulator they are block-rate and lane-global: a chord has its last
  note's. The notes they hear are the ones the lane's instrument is given: after
  the lane's range, remap and transpose, and a pattern's own notes. A group's
  hear the notes of its members' ranges.
TODO: the keys of the computer play every note at 100 (a key event has no
velocity), in the page and in `play`; only a MIDI keyboard and the page's piano
(by where the pointer is on a key) give a velocity control more than one value.
TODO: a key control is centred on middle C and spans five octaves for everyone
(`KEY_CENTRE`, `KEY_SPAN`): to track over fewer octaves a person scales the depth
of the cable, and the centre cannot be moved.
- **Macro** (unipolar, additive around the base, as a CC): `base + value * depth *
  range` — rest (0) = no change, full = +depth·range above the base.
- **Step sequencer** (bipolar, additive around the base, as an LFO): `base + value *
  depth * range`, where `value` is the step it is at, -1 to 1 (see "Step sequencer modulator").

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
a plugin empties it (`clear_instrument`: the cables that drove the plugin's parameters go first, and so does the
lane's note remap, which the audio thread drops with the plugin) and a second `d` takes the lane out.

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

A cross-mod cable works from the value the user set the field to (an LFO's rate,
a stage of an envelope, the depth of a cable) and writes it every buffer, the
last cable to write it standing: when the user sets the field the engine moves
what the cables that drive it work from (`update_cross_mod_base`: for a depth,
since f133, which had been left out, so that the cross-mod wrote the depth back
to what it was around the old value every buffer, and the slider did nothing).

A field that is turned on a log scale (the knob of an LFO's rate, 0.01 to 50 Hz,
and of an envelope's attack, decay and release, 0.001 to 10 s, runs over decades:
`ModulatorField::taper`) is moved on that scale, so the cable's `depth` is a
fraction of the knob's travel and not of the range in seconds or hertz. The
user's value is put at its place on the knob (`ln(value / min) / ln(max / min)`),
the source offsets that place by the laws above (an LFO plus or minus `depth`, an
envelope from `depth` of the way down to the value the user set, a MIDI control
as before) with the knob's travel, 0 to 1, as the range, and the result goes back
to a value (`Modulator::modulated_value`, which `apply_cross_mod` and a group's cable to
a member's modulator use). A swing is as far up the knob as down it: an LFO at
depth 0.5 on an attack of 10 ms goes from 1 ms to 1 s (it went from 1 ms to 5 s,
and sat at 1 ms for half of each turn). A sustain and the depth of a cable are
levels and go by the range, as does a range that starts at nothing (there are no
decades in it). A saved session's cross-mod cable to a rate, an attack, a decay or a
release means a fraction of the knob now, and no longer of the range.

### Node positions (`ui`)

A front-end that draws the session as a canvas (the browser, see "Web UI") keeps where each node is in the session
file, as a `ui` table on the node's own table. The engine and the TUI do not use them: the TUI does not show them,
and keeps them through a load, an edit and a save.

```toml
[ui]
master = { x = 1200, y = 300 }  # the master mix has no table of its own: its place is here

[[group]]
ui = { x = 800, y = 40 }

[[group.effect]]
plugin = "builtin:reverb"
ui = { x = 1100, y = 40 }

[[instrument]]
plugin = "builtin:osc"
ui = { x = 0, y = 0 }

[[instrument.effect]]
plugin = "builtin:filter"
ui = { x = 376, y = 0 }

[[instrument.modulator]]
ui = { x = 376, y = -200 }
```

- `x` and `y` are pixels on the page's canvas: finite numbers, whole or with decimals (saved as typed: a whole
  number has no point, `12.3` is not `12.300000190734863`). A node without `ui` is laid out by the page.
- A `ui` that is not a pair of numbers (`ui = 5`, `ui = { x = 1 }`, a `[ui]` that is not a table) is ignored, with
  a log line: a layout is not worth refusing a session for. A number that is `nan` or `inf` is refused like every
  number.
- The places are in the model as a side table (`SessionModel.ui`, by `NodeId`; the master's under
  `NodeId::MASTER`), read by the loader (`app::load`) and written by `app::save::to_save` and `session::to_toml`.
  The place of a node that is taken out stays in the table (ids are not reused but for what undo gives back, so nothing
  else reads it) and is not written.
- Older sessions nested `[[instrument.effect.modulator]]`: such a modulator keeps its `ui` when it migrates into the
  lane's rack.

### Pinned parameters (`pinned`)

A front-end that shows a plugin's parameters as knobs on its node (the browser) keeps which ones in the session file, as a
`pinned` list of parameter names on the plugin's table: the lane's instrument (on the `[[instrument]]`), an effect (`[[instrument.effect]]`)
and a bus effect (`[[group.effect]]`). The engine and the TUI do not use it: the TUI does not show it, and keeps it through a
load, an edit and a save.

```toml
[[instrument]]
plugin = "builtin:osc"
pinned = ["sustain", "detune", "waveform"]

[[instrument.effect]]
plugin = "builtin:filter"
pinned = ["resonance", "cutoff"]
ui = { x = 376, y = 0 }
```

- The order is the order the knobs are shown in. With none pinned (no `pinned`, or an empty list) a node shows the first eight
  parameters of its plugin; a parameter a cable ends at is shown too, for as long as the cable is there, and is not a pin.
- It is a key of the plugin's table beside `ui`, not part of it: a node can be pinned without being placed.
- It is read leniently, as a place is (a layout is not worth refusing a session for): what is not a list is left out and logged,
  and so is any entry of a list that is not a name. A name that the plugin has no parameter of, a separator, and a name that comes
  twice (the first place counts) are no pins; each is logged, and the next save leaves them out. A name that two parameters share
  is the first of them.
- In the model it is `PluginSlot::pinned`, the plugin's own parameter indices (`ParamSlot::index`, which cables and ports name too),
  so it goes where its plugin goes: an effect that is moved takes its pins along, an instrument that is replaced has none. The
  loader resolves the names (`PluginSlot::pin_names`) and the saver writes them (`pinned_names`). At most 64 can be pinned to a node
  (`MAX_PINNED`).

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

### Tempo

```toml
bpm = 96.0              # optional: the tempo patterns are recorded at, in beats a minute (20 to 300)

[[instrument]]
plugin = "builtin:osc"
```

A top-level key, before every table. It is the tempo that the next pattern is recorded at (the metronome's count-in and
the length of the recording are counted in it) and the one that the TUI shows (`120 BPM`, set with `b`). A session whose file
has none is at the tempo of its first recorded pattern (`[instrument.pattern] bpm`, below), or at 120 if it has none. A tempo
that is out of 20 to 300 is held to it when the session is loaded, and a value that is not a number (a string, `nan`, `inf`)
refuses the session, as every number does. The audio thread is told the tempo once when the session is loaded, if the file says
one or has a pattern (`SessionModel::bpm`, set by the loader). A save writes `bpm` only when a load would not come to that tempo
anyway (`SessionModel::bpm_to_save`), so that the files of sessions that never set one are as they were.
A lane that is added after the tempo was set starts at 120 in the audio thread (`PatternPlayer::new`) and is told the tempo when a recording starts, which the
TUI's `r` does first; a pattern that has been recorded keeps its own `bpm` whatever the tempo is set to afterwards.
TODO: a pattern does not follow the tempo (its events are frames, and its length is its beats counted at its own `bpm`); `docs/web-ui.md` Open question 10.

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

- `bpm` — the tempo the pattern was recorded at: its length in frames is `length_beats` counted in it (default: 120). It is not the
  session's tempo (see "Tempo"): setting that, with `b`, does not change a pattern that has been recorded
- `length_beats` — pattern length in beats (default: 4 = 1 bar in 4/4). The Length row of the pane sets it: the pattern that is there loops at the new
  length from then on (its notes stay where they are, and those that the loop no longer reaches wait for it to be long again), and the next recording is that long
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
- What is done to a pattern is done by intents, which the browser's requests are as well: `r` is `SetRecording { node, recording }`, `Ctrl+R` and `d`
  on a pattern are `ClearPattern { node }`, and the rows of the pane (typed or stepped) are `SetPatternField { node, field, value }` (see "Session state"). The
  pattern is on its lane's node in the graph a browser is given (`PatternInfo`: whether it is recording, enabled, looping and in key, its length in beats, the tempo
  it was recorded at and how many notes it has), and what is done to it is told as `PatternSet`.
- The browser page has the same controls in a block under every lane (`app/patch/PatternCell.tsx`, step 51; see "Web UI"): Record / Stop, Clear (it asks first: a pattern is
  not put back by undo), the length in beats, Play, Loop and In key, and a line that says what the pattern is. The count-in is `COUNT_IN_BEATS` (4) beats of the metronome,
  which the page reads as `PATTERN_COUNT_IN_BEATS`.

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
| 3 | Scope | The output as a wave or as a spectrum (see "Scope tab") |
| 4 | Help | Keybindings and usage reference (static, scrollable) |

TODO: the Piano tab draws the keyboard, the scale and the notes being held (see "Piano
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

A popup has the keys while it is open (so has the filter of the parameters, `/`).
What it takes as text is text, and a letter with Control held is none of it: a
popup ignores such a chord and does not put the letter in (`Ctrl+Z`, which undoes in
the chain, would type a z), except `Ctrl+C` and `Ctrl+Q`, which leave the program
from a popup as from the chain. Control with Alt is how Windows reports AltGr (`@`
and `\` on some keyboards), and is text; so is a capital letter (Shift).

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

The selected entry is highlighted (bold, white). Unselected entries are dimmed. A group's header is cyan
and a modulator magenta, a group's own included: they keep their colour, bold when selected and dimmed when not
(`TreeEntry::color`; the other entries have none of their own).

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
| `n` | Add a new instrument (defaults to `builtin:osc`, opens the plugin selector to replace it; full range until set with `R`; a step of the history, which the replacement that follows empties; needs the plugin catalog to name the oscillator, which the startup scan does at once) |
| `i` | Replace instrument (opens instrument selector popup). The lane keeps its effects, modulators, range, volume and group; the cables that drove the old plugin's parameters are taken out (the new plugin has parameters of its own), and a note remap goes with the old plugin |
| `R` | Set the selected instrument's key range (opens range popup, prefilled) — or, on a group header, rename the group (opens rename popup, prefilled) |
| `v` | Set the output volume of the selected instrument or group (host-side gain, opens value popup) |
| `M` / `S` | Mute / solo the lane or group the selected entry is of, as the intents `SetMute` and `SetSolo` do (see "Mute and solo"): the instrument, its effects, modulators and pattern are the lane's, a group's header, bus effects and modulators are the group's. They toggle, are steps of the history, and show at the end of the line as `[mute]`, `[solo]` or both (`▦ Pad  [mute]`, `♪ Oscillator  [Built-in]  [mute] [solo]`); the action bar of an instrument and of a group has them (and, so that the bar fits 120 columns, `replace`, `vol` and `add fx`) |
| `a` | Add effect to the selected instrument, or — on a group/group-effect node — to the group's bus chain (opens effect selector popup). New effects default to 0.5 mix (half-wet); `builtin:filter` defaults to 1.0 (fully wet, since a half-wet filter barely filters) |
| `m` | Chain focus: add an empty LFO modulator — to the instrument's lane rack on an instrument-side node, or to the group's rack on a group / group-bus node. Param focus: modulate the selected parameter — opens a popup to bind it to a new (LFO / envelope / macro / velocity / key / step sequencer) or existing modulator (a group bus-effect param binds a group modulator; an instrument/effect param binds a lane modulator; a **modulator's own param** binds a sibling modulator for cross-mod; a new modulator and its cable are one step of the history, and a modulator that drives the parameter already is not attached again) |
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
| `Ctrl+R` | Clear the pattern of the selected instrument or pattern (chain focus; `d` on the pattern does the same) |
| `b` | Set the tempo patterns are recorded at, in BPM (opens value entry popup; the `SetBpm` intent: held to 20–300, a step of the history, saved as `bpm`) |
| `o` | Show / hide the scope of the node under the chain cursor, in a strip under the chain and the parameters (see "Scope tab"; a terminal of 24 rows or more) |
| `O` | Wave or spectrum of that scope |
| `Ctrl+S` | Save session (to the file it was loaded from) |
| `Ctrl+Shift+S` | Save session as (prompts for filename, saves in the current session's directory) |
| `Ctrl+Z` | Undo the last thing done (the history of the controller, which the browser's Undo has too; not heard while a popup is open) |
| `Ctrl+Y` / `Ctrl+Shift+Z` | Redo |

A step of `Left` / `Right` is a twentieth of the range of the row (a hundredth with `Shift`, a tenth with
`Ctrl`). The rate of an LFO and the times of an envelope run over decades (0.01 to 50 Hz, 1 ms to 10 s), so
for them the range is a ratio and a step multiplies by that fraction of it: a rate by 1.53 (1.09 with `Shift`, 2.34 with
`Ctrl`), a time by 1.58 (1.10, 2.51), and `Left` divides by the same, so a step up and a step down of one size give back
the value. It is the scale that the page's knobs turn them on (`ModulatorField::taper`, which the page reads from
the generated constants). A sustain and the depth of a cable move by 0.05 (0.01, 0.10).
TODO: the bar of a rate or a time is still linear (0.01 to 50 Hz in one scale) though a step on it is a ratio, and so is a drag
on it with the mouse; a bar and a drag on the log scale would match a step and the page's knob.

Mouse: click to select, drag a parameter bar to set its value, drag the
parameter scrollbar to scroll. Mouse wheel scrolls the parameter list and the
Help tab.

TODO: `p` on a group bus effect, and a "≡" menu on one in the page: the controller refuses `LoadPreset` for a
bus effect (`a bus effect has no presets to load`), since the audio thread's `LoadPreset` command addresses a
lane's slot; a command for a group's chain would do it.
TODO: `e` shortcut to open the value entry popup directly (currently use
`Enter` from chain focus or land on a param and press `Enter`).

### Plugin selector popup

Opened by `i` (instruments only) or `a` (effects only). Same layout for both,
filtered by plugin type.

- **Top**: text input for filtering (matches against any column: name, type, etc.)
- **Below**: table with columns — Name, Format (LV2/CLAP/VST3), Params, Presets
- `Up` / `Down` — navigate rows
- `Enter` — select plugin and close popup
- `Escape` — cancel and close popup
- Typing updates the filter immediately

A built-in is added at once. Any other plugin can take a while to load (Pianoteq 9 takes about a third of a second here: `tang enumerate plugins` says), and the loop is blocked for as long as
it takes, so a frame is drawn first with a box in the middle of the screen that says `Loading <name>…`
(`State::pending_load`: `handle_event` takes the key, the loop draws, `run_pending` asks the controller for the plugin;
`process_event` is the two together, for the tests' keys, which have no frame between them).

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

- Text input for a filename, which goes through the intent `SaveAs { name }` (the
  one the browser's "Save as…" asks too), so the name is checked by
  `session::plain_file_name`: the spaces round it go, `.toml` is appended if
  missing, and it has to be a plain file name: no `/` or `\`, none of `:*?"<>|`
  or a control character, something before the extension, not a name that
  Windows keeps for itself (`con`, `nul`, `com1`…), at most 255 bytes. The file is
  saved in the current session's directory (cwd if there is no current
  session). A file that is there already is not written over (a link that leads
  nowhere counts as there), unless it is the session's own file: Save As to the
  name the session has is a save.
- `Enter` — save and close; `session_path` is updated so subsequent `Ctrl+S`
  targets the new file, and the browsers are told (`Saved { file }`). On failure
  (or an empty field) the popup stays open — the reason is shown in the popup
  and `session_path` keeps its old value.
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
modulation). Lists: "New LFO", "New envelope", "New macro", "New velocity", "New key",
"New step sequencer" (a velocity or a key follows the notes the lane plays: see
"MIDI-learn modulator"; a step sequencer is last, so that what a person's fingers
know does not move),
and each existing modulator in the relevant rack ("attach → …"; `NEW_MODULATORS`,
`src/tui/mod.rs`, is the table of the new ones). It works on:
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

The keys are a natural piano (white and black bodies) with a stripe along the
bottom of each for what it is in the scale: **gold** for the root, **teal** for
the other scale tones, none for the rest. A key that is held sinks (its body
goes a shade grayer, the stripe stays); one that is held and is **not in the
scale** goes red, the "wrong note" (in Locked mode it never appears: off-scale
notes cannot be held).

| Key | Action |
|-----|--------|
| `k` | Open scale picker (root + scale type, filterable); the choice is the `SetScale` intent |
| `l` | Toggle Locked mode: off-scale notes are dropped at the sender (Highlight, the default, only colours them); it is the `SetLocked` intent |
| `[` | Shift view octave down |
| `]` | Shift view octave up |

The browser page has the same piano under its canvas, with the same colours
(see "Web UI"); a scale or a lock chosen in either is seen in the other.

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

### Scope tab

The output as it goes by, drawn on a braille canvas as a **wave** or as a **spectrum**: the picture of the page's scope on the master node (see "Web UI"), with the same trigger, time bases and spectrum
(`src/tui/scope_view.rs`: pure functions that have the tests of `web/src/client/scope.ts` and `fft.ts`). A line on top says what is shown and the loudest sample of the picture
(`Peak: -6.0 dB`: decibels below full scale, `+3.5 dB` in red when the output clips, a dash for silence), the picture is in a box, and the keys are under it.

| Key | Action |
|-----|--------|
| `v` | Wave or spectrum |
| `Left` / `Right` | A shorter / longer time base for the wave: 256, 512, 1024 or 2048 samples across (5, 11, 21 and 43 ms at 48 kHz; it starts at 1024) |

- **Wave.** The left side in teal, and the right side under it in green where it is not the same (a mono sound is one line). It is drawn from where the left side goes up through the middle after having been 0.02 under it
  (so that a wobble is not a wave), which makes a wave that goes round again and again stand still; with no such place (silence, a wave that stays on one side) it is the latest frames. Lines at 0.5, 0 and −0.5.
- **Spectrum.** The mix of the two sides: a Hann window and a 2048-point FFT of the latest 2048 frames (a bin is 23 Hz at 48 kHz, so that the low notes are a few bins wide), from 20 Hz to 20 kHz on a logarithmic axis and from 0 to −90 dB,
  with lines at 100 Hz, 1 kHz and 10 kHz and at every 20 dB. A level falls 40 dB a second when the sound is quieter than it was (a wait of more than a quarter of a second between two looks is made up for a quarter of a second of the fall).
- The frames are copied from the scope ring (`telemetry::ScopeRing`, see "Architecture") on every frame that is drawn while the tab is shown, about ten a second, so that a wave that moves fast is seen in steps. The choices
  are the tab's own: they are not saved with the session, and not told to the audio thread or to the browsers (a browser's scope has its own).

**The scope of a node** (step 47, `docs/web-ui.md` D47; `o` on the Session tab). The picture of the page's scope in a node, in a strip of ten rows under the chain and the parameters (`scope_view::Strip`, `STRIP_ROWS`): what comes out of the
node under the chain cursor, as a wave or a spectrum, drawn as the Scope tab draws (`scope_view::paint`), with the node's name, the view, the time base and the loudest sample in the top line of its box and the keys in the bottom. The node is an
instrument that has a plugin (after its volume), an effect (after its mix), a group (after its pan) or an effect of a group's bus (after its mix); what has no signal, a modulator, a pattern and a lane that has no instrument, says so in the box
(`No signal here…`), and so does a node that no ring is left for (`No ring is free…`: the engine has eight rings, for the terminal and every browser together, `Controller::probe_ring` says which a node has). `o` shows the strip and takes it away,
`O` chooses wave or spectrum; both are the strip's own (not the Scope tab's, not saved, not told to the audio thread or to the browsers), and a terminal that has not room for the strip and twelve rows of the chain and the parameters (24 rows in
all: `split_for_strip`) shows the chain and the parameters and nothing else. The engine listens to a node only while somebody watches it, so the strip watches the node under the cursor, and only while it is shown and the Session tab is: every tick
of the loop (a turn of it begins with one, after the events of the terminal and the requests of the browsers, so that a node that a browser took out is let go of) calls `Strip::sync`, which asks `Controller::set_probes(Watcher::TERMINAL, …)` when the
node is another, and forgets the picture of the node that was watched (a spectrum falls slowly, and the levels of one node would be seen under the name of the next). `Watcher::TERMINAL` is the terminal's own number, the highest there is: the
owner of the web interface lets go of the watchers of the connections that have closed (`Owner::refresh_probes`) and keeps this one, so that `tang --web` has both. The frames are copied from the node's ring on every frame that is drawn
(`Strip::take`), about ten a second.

TODO: a time base for the strip (it is 21 ms; `Left` and `Right` are the parameters' on the Session tab), `o` in the action bar of the Session tab (its hints are in about a hundred snapshots; the Help tab lists it) and a word where a
terminal is too short for the strip (it shows nothing then), and the scope's choices and the strip's being on kept with the session or the config (the page's are not either: Open question 8).
TODO: a reading of the pitch (a bin of the spectrum is 23 Hz wide: the peak could be interpolated between bins), a vertical zoom for a quiet sound (the tracer's wave, at its oscillator volume of 0.1, is two pixels high on the page's scope),
and a key that freezes the picture. The page keeps the view and the time base in the master node, so that a reload goes back to the wave at 21 ms.

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
in `src/app/model.rs`, and the tempo that patterns are recorded at is the model's
(`SessionModel::bpm`: the first recorded pattern's, 120 for a session with none). The
model, the session path, the dirty flag,
the plugin catalog and the engine handles (command channel, plugin runtime,
notification receivers, the state shared with the note senders) belong to
`app::controller::Controller`; the TUI reaches them through `State::ctl`. The
controller folds what the audio thread reports (a finished pattern recording, a
captured MIDI-learn control) into the model, and `poll` tells the TUI what
changed. It also saves the session (`Controller::save`; Save As is the intent `SaveAs`).

Every node of the model (an instrument lane, an effect, a group, a bus effect, a
modulator) has a `NodeId`: allocated by the model, never reused during a run
but for what undo gives back (see "Undo and redo"), not saved. The engine and the tree address nodes by position, which shifts when
others come and go; an id stays with its node. A lane's instrument plugin sits in
the lane's node. `SessionModel::locate` turns an id back into a position.

Edits that have an `Intent` go through `Controller::apply`, which changes the
model and the audio thread together and answers with `Event`s (`app::intent`,
`app::apply`; `docs/web-ui.md` §5). Built so far: `SetParam` (clamped to the
parameter's range, an enum rounded to a whole value), `SetGain` (instrument
volume, effect and bus-effect mix, group volume and pan, each with its own
range), `LoadPreset` (by preset id; the values the preset set are mirrored
into the model), `SetModulatorField` (an LFO's rate and waveform, an envelope's
stage, a macro's value or a step sequencer's rate, glide, length and retrigger, in a lane's modulator or a group's, clamped to `ModulatorField::range`, a
waveform rounded to its index: sine, triangle, saw, square, random, smooth, and a length and a retrigger to a whole number), `SetSteps { node, values }` (all sixteen steps of a
step sequencer, each held to -1..1, told as `StepsSet` with what it has; refused for a list that is not sixteen long, a value that is not a number and a node that is no
step sequencer), `SetEdgeDepth` (a
modulation cable's depth, -1 to 1) and `SetRange { node, range }` (the notes a lane takes, both
ends included, or every note for none; refused for a node that is no lane, a note above 127 and a low
note above the high one; the TUI's `R` popup asks it too), `SetMute { node, muted }` and `SetSolo { node, solo }` (a lane or a group muted or soloed, see "Mute and solo"; refused
for any other node and, as for every intent, not found for the master; told as `MuteSet` and `SoloSet`, which a `Snapshot` applies; the model's `muted` and `solo` and the audio thread's
`SetMute`, `SetSolo`, `SetGroupMute` and `SetGroupSolo`; saved as `mute` and `solo`), `SetScale { scale }` (the Piano tab's scale, as
the session file writes it, `F# dorian`, flats, long names and any case taken; refused for text that is
not a scale; answered with `ScaleSet`, which has the root and the pitch classes a client draws from) and
`SetLocked { locked }` (the lock to the scale; the TUI's `k` and `l` ask them too), `SetBpm { bpm }` (the tempo that patterns are recorded at, held to 20 to 300;
refused, with nothing changed, for a value that is not a number; told as `BpmSet` with the tempo it has now; the model's `bpm` and the audio thread's `SetGlobalBpm`,
saved as the file's `bpm`; the patterns that are recorded keep the tempo they were recorded at; the TUI's `b` asks it), `SetRecording { node, recording }` (start a
recording on the lane `node`, as the terminal's `r` does: a lane that has no pattern is given an empty one, four beats, looping, not in key and not heard, at the session's tempo; the
audio thread is told the tempo and the length first, since the count-in and the recording are counted in them, and then to record; a recording that is stopped is stopped at once, and the
pattern that a recording makes when it is done comes from the audio thread, which `Controller::poll` folds in and the browsers are given a snapshot of; one that is started while it is going
on, or stopped while it is not, is told as it is and sends nothing, since a second start would begin the count-in again), `ClearPattern { node }` (the pattern of a lane taken out, and a recording that is going
on with it; refused for a lane that has none), `SetPatternField { node, field, value }` (`length`, 1 to 32 beats, and the switches `enabled`, `looping` and `transpose`, 0 or 1, rounded; each held to
its range, `PatternField::range`, and sent to the audio thread by its own command; a lane that has no pattern is given an empty one with it, so that the length of its first recording can be
chosen; refused for a value that is not a number, which is judged before the node is looked at) — all three refused, with nothing changed, for a node that is no lane (`a pattern belongs to a lane`) —
told as the one event `PatternSet { node, pattern }`, the pattern the lane has after it (`PatternInfo`, or null when it has none), and none of them a step of the history: a pattern is not put
back by undo, and what is done to one does not forget the steps there are (`Plan::Nothing`), `SetPinned { node, params }` (which parameters of a
plugin a front-end shows as knobs on its node, by the plugin's own indices in the order they are shown, replacing the list there was;
none gives the node back to the front-end's own choice; refused, with nothing changed, for a node that has no plugin, a parameter the
plugin has not, a separator, a parameter twice and more than `MAX_PINNED`; told as `PinnedSet`; the engine and the TUI have no use for
it, the session file keeps it, see "Pinned parameters (`pinned`)"), `SetNodeUi { node, ui }` (where a front-end
draws a node, `{ x, y }` in the pixels of its canvas or none for the front-end's own layout; any node, the master included;
kept to a tenth of a pixel and within 100 000 either way, `NodeUi::tidy`; told as `NodeUiSet`; the engine and the TUI have no
use for it, the session file keeps it, see "Node positions (`ui`)"), `RenameGroup { node, name }` (what a group is called: the name
without the spaces round it, at most `MAX_NAME` (64) characters and none of them a control character, and nothing gives the group back the
name it has by default, "Group" and its number; refused, with nothing changed, for a node that is no group and for a name that is not one;
told as `GroupRenamed`, which carries the name the group has now; the engine has no use for it, the session file keeps it, and the TUI's `R`
on a group header asks it); `Save` (the session's own file); `SaveAs { name }` (the session written as the file that
`session::plain_file_name` makes of `name`, beside its own file, which is its file from then on: the spaces round the name go, `.toml` is added
unless it ends so, and it is a plain file name, no `/` or `\`, none of `:*?"<>|` or a control character, something before the extension, not a name
Windows keeps and at most 255 bytes; refused, with nothing written and nothing changed, for a name that is not one and for a file that is there
already, a link that leads nowhere included, unless it is the session's own (that is a save), as `IntentError::Invalid`, and a write that fails is
`Io`; told as `Saved { file }` with the new name; no step of the history; a file that is there is refused by the write itself (an exclusive create,
`session::save_new`), not by a look at the path first; the TUI's `Ctrl+Shift+S` asks it too, and so does the page's "Save as…"), `Undo` and `Redo` (the history, below); and eleven that change
the structure: `RemoveEdge`
(a cable, with the cables that drive its depth), `AddModulator { parent, kind }` (an LFO,
an envelope, a macro at rest, an armed MIDI control, or (step 52) a velocity or a key control, bound at once, or (step 57) a step sequencer (a staircase), at the end of the rack of a lane, `parent` being the
lane's node, or of a group), `AddModEdge { modulator, to }` (a cable at depth
`NEW_CABLE_DEPTH`, 0.5, to a plugin parameter or to a modulator's setting: where a cable to a
parameter may end is `SessionModel::param_targets`, a lane's modulator reaching its lane's
chain and a group's its members' chains and its bus effects; a cable to a `rate`, `attack`,
`decay`, `sustain` or `release` port of a modulator, or to the `depth` port of one of that
modulator's cables, is as `SessionModel::modulators_in_reach` and `field_targets` say (what the
TUI's `m` on a modulator's field and its `t` offer), a lane's modulator reaching the others of
its rack and a group's its members' modulators and the others of its own, never itself; refused
for a port that is not one of these, a node or a plugin out of reach, a modulator that has no
such setting, a parameter or setting it drives already; a cable to a setting has a telemetry
slot like any other, for the value that setting is set to), `Modulate { to, kind, depth }` (a modulator of `kind` made for what the port `to` is, and a cable from it to
the port, as one thing: what the TUI's `m` and `l` do, and what the page's menu on a knob asks. It goes at the end of the rack
that reaches `to`: the lane's for a lane's instrument or an effect of its chain, the group's for a bus effect, and the rack a
modulator is in for a setting of it or the depth of one of its cables, where the new one is a sibling that drives it. The cable
starts at half depth, or at `depth` clamped as `SetEdgeDepth` clamps it, and a MIDI control is armed to learn as for
`AddModulator`. Refused, with nothing changed (nothing is made and no id is used), for what `AddModEdge` refuses of a cable
to `to` from a modulator that is not there yet, for a group, which has no parameter, and for a depth that is not a number:
`Controller::modulate_intent`; the plan of the cable is `param_cable` / `field_cable`, which `AddModEdge` uses too),
`AddEffect { parent, plugin }` (a plugin of the catalog, loaded by the controller
and nothing else, at the end of the chain of a lane, `parent` being the lane's node, or of a
group's bus: see `Controller::add_effect`), `AddInstrument { plugin }` (a lane at the end of
the session, full range and in no group, with an instrument of the catalog in it: see
`Controller::add_instrument_lane`), `ReplaceInstrument { node, plugin }` (the instrument of
the lane `node`, or an empty lane's, replaced by one of the catalog; the cables that drove
the old plugin's parameters are taken out: see `Controller::put_instrument`), `MoveEffect { node, index }`
(an effect of a lane's chain or of a group's bus put at place `index` of its chain, 0 being the first: the effects
between move one place the other way, the audio path goes through them in the new order and every cable stays
on the plugin it drove; refused for a node that is no effect, a place the chain has not and the place the
effect is at: see `Controller::move_effect` / `move_bus_effect`), `AddGroup { members }` (a group at the end, with no effects and
no modulators, full volume and a centred pan, and the lanes in `members` put in it at once, as `SetLaneGroup` does for each; none
makes an empty group; refused, with no group made, for a member that is no lane), `SetLaneGroup { node, group }` (a lane put in a
group, or taken out of the one it is in for none: the group it leaves lets go of it, its cables to the lane's plugins and
modulators go, with the cables that drive their depths, what they drove has the value that was set, and the cables to the members
after it are told their new ordinals; a lane put in the group it is in is as it was; refused for a node that is no lane and a group
that is no group: `Controller::set_lane_group`, which the TUI's `g` asks too) and
`RemoveNode` (a modulator, with its cables and
those of the others that drove its settings; an effect or a bus effect, with the cables that
drive its parameters and what drove their depths, the audio path closing up; a lane, with its
instrument, its effects and its modulators and the cables of its group to it, but not the last
lane of a session (`a session keeps its last lane`); a group, with its bus effects, its modulators and every cable of it, its
members staying in no group, and what the cables drove in them given back: `Controller::remove_lane` / `remove_group`. A `RemoveEdge` and a
`RemoveNode` of a modulator, of an effect, of a bus effect, of a lane and of a group are put back by undo (step 34, "Undo and redo"); the page still asks first, in the
words of the time before there was an undo). An intent that changes the
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
own edit). Step 33 moved the TUI's structure edits that have intents onto them: its plugin selector (`AddEffect`,
`ReplaceInstrument`), `d` (`RemoveNode`), `n` (`AddInstrument`), `g` (`AddGroup`, `SetLaneGroup`), Shift+Up and Shift+Down on an effect
(`MoveEffect`), `m`, `l` and `t` (`Modulate` for a new modulator and its cable, `AddModEdge` for a modulator that is there, `AddModulator` and
`SetEdgeDepth`; the target that a popup lists is a positional kind, which `SessionModel::endpoint_of` turns into the node and port that
`AddModEdge` and `Modulate` name) all go through `State::apply`, as the browser's requests
go through the controller, so that they are in the history, and `Controller::remove_effect`, `move_effect`, `remove_lane` and the others are
the intents' own (`SessionModel::remove_effect` / `move_effect` and the bus's keep the modulator targets of the lane and of its group
pointing at the same plugins, as the audio thread does). What the TUI still edits itself, for want of an intent: the order of the lanes
(`Controller::swap_lanes`, which forgets the history), the Type row and emptying a lane (`Controller::clear_instrument`, which does too). (The pattern was one of them until
step 50: `r`, `d` and `Ctrl+R` on a pattern and the rows of its pane ask `SetRecording`, `ClearPattern` and `SetPatternField` now, which are no step of the history.)

**Undo and redo** (`src/app/history.rs`, `src/app/journal.rs`, `docs/web-ui.md` §5, D35 to D37). The controller keeps a history of what a person did: a
step for each change that has an inverse, `SetParam`, `SetGain`, `SetModulatorField`, `SetSteps` (all the steps of a step sequencer are one thing: a stroke across the bars is one step, "set steps"), `SetEdgeDepth`, `SetRange`, `SetMute` and `SetSolo` (in the words `mute the lane of Oscillator`,
`unmute Pad`, `solo an empty lane`: never merged with the one before them, since a toggle that is merged with its opposite leaves a step that does nothing, and none for what changes nothing), `SetNodeUi`,
`SetPinned`, `RenameGroup`, `SetScale`, `SetLocked`, `SetBpm`, `MoveEffect`, and what is added: `AddEffect` (a lane's or a bus's), `AddInstrument`,
`AddModulator`, `AddModEdge`, `Modulate`, `AddGroup` (of lanes that are in no group) and `SetLaneGroup` (unless the lane's group drives it), made of the
intents that put it back and the intents that do it again (read from the model before the change, so that it puts back what the change
overwrote and does again what was asked) and the words of a button for it (`set cutoff`, `add Reverb`, `draw a cable`). **What is added is
taken out by undo (`RemoveNode`, `RemoveEdge`) and made again by redo under the ids it had**: `NodeIds::reuse` says which ids come next
before any new one, `Controller::carry_out` writes down the ids it hands out (`made_last`, `Made`), a `Plan::Added` becomes a step once they are
known (`Step::redo_made`), and `carry_out_all` says them to the allocator, refuses ids that are in use (`node 7 is there already`) and drops those
the intents did not use, so every step that names the node or the cable goes on naming it, whatever is undone and redone around it. (A telemetry
slot is not given again: a node that comes back has new slots, and they are never reused, 4,096 of them.) **What is taken out is put back by
what was taken** (step 34, D37), exactly as it was: while an intent is carried out the controller writes down what it takes out (`Controller::journal`,
a `Journal` of `Inverse`s; the TUI's and the tests' own removals leave nothing), the model hands back what it removed (`SessionModel::remove_edge`
returns the cables as `RemovedCable`: the cable with its id and its slot of the telemetry, its place, and what it was centred on, `centre_of`), a
`Plan::Removed` becomes a step that holds the journal (`Step::removed`, `Taken`), and undo puts everything back in the reverse order
(`Controller::rewind`, one revision, the structure said to have changed): each cable goes into its modulator at its place
(`SessionModel::insert_target_at`, `GraphCommand::InsertModTarget` / `InsertGroupModTarget`, which tell the cables that drive the depth of a later cable
of the modulator the place it has now, before it goes in), with the id and the slot it had; redo takes it out again and the step holds what that took.
Built for `RemoveEdge` and for `RemoveNode` of a modulator: the cables that address the modulator (its siblings', and its group's when the lane is a
member) go first, each written down as a cable, and then the modulator is written down with its own cables and what each was centred on
(`Inverse::Modulator`; `SessionModel::remove_modulator_at` hands it back), and undo puts it back at its place in its rack
(`SessionModel::insert_modulator_at`; the audio thread's `InsertModulator` / `InsertGroupModulator`, then `SetModulatorOutput` for the slot it publishes to and
`AddModTarget` / `AddGroupModTarget` for each cable) before the cables that addressed it, which tell the cables of the modulators after it the places they have
now, before it goes in; one that goes in at the end moves nothing, since a session is loaded a modulator at a time and the cable of an earlier one can address one
that comes later. A step is made of it in the words of the button that adds one like it (`remove an LFO`, `an envelope`, `a MIDI control`), from the page and
from the TUI's `d` alike. An effect of a lane's chain or of a group's bus is put back the same way, with **the very plugin that was in the chain**
(`Inverse::Effect`: the model's `PluginSlot` as `SessionModel::remove_effect` / `remove_bus_effect` hands it back, with its node, its values, its preset and its
mix, and the place it was at; the cables that drive its parameters go first, each written down as a cable). The controller makes a channel for the plugin
(`Controller::channel_for`: one for each removal that an intent carries out, none for a call that no intent asks for), sends the other end with the
command (`RemoveEffect { keep }`, `RemoveGroupEffect { keep }`), and the audio thread hands the plugin to it in place of the channel that gives plugins back to
the main thread (and to that one if no one is listening any more, `hand_back`); the journal holds the end that listens (`Kept::Asked`). Undo gets the plugins
first, so that what can fail fails before anything is changed (`Controller::rewind`): it waits for them for `recovery_wait` in all (`EngineHandles::recovery_wait`,
250 ms in the program: the audio thread takes a command within a block or two), and a plugin that has not come, or that the history let go of, is loaded again
(`reload_plugin`: from the id it was loaded from, with the preset it had and then the values the model has, which are the user's); a plugin that does not load,
or has other outputs than the chain's, refuses the undo with the reason (`Reverb could not be loaded`) and the step is dropped. Then everything goes back in the
reverse order (`SessionModel::insert_effect_at` / `insert_bus_effect_at`; the audio thread's `InsertEffect` / `InsertGroupEffect`, at the mix it had), each telling
the cables that address the effects at that place and after it (the lane's modulators', and the group's) the place they have now before the effect goes in, as the
removal tells them the other way. A plugin can hold a good deal of memory, so the history keeps the plugins of the newest steps only (`MAX_KEPT_PLUGINS`, 8: a
step that cannot keep all of its plugins keeps none, `History::keep_the_newest_plugins`) and an older step loads them again; and `Controller::shutdown` lets go of
what the history holds before the plugin runtime goes, since a plugin has to be dropped while the libraries it came from are there. A step is made of an effect
in the words of the button that adds one (`remove Reverb`). **A lane is put back whole**: the controller takes out the cables of its group to it first (each
written down as a cable), and then the lane, handed to a channel of its own (`RemoveInstrument { keep }`, `Inverse::Lane`: the model's `InstrumentNode`, with its
plugins' slots, its modulators and their cables, its pattern and its group, and `KeptLane`, the lane that the audio thread hands back with its plugins and
whatever they hold, its modulators and what they have in hand, its pattern and its meter); the lane's own modulators' cables go with it. Undo gets the lane first
(`Controller::recover_lane`): it waits `recovery_wait` for it, and **refuses the undo with the reason** if it did not come (`the lane of Oscillator was not handed
back by the audio thread`; a lane cannot be built again from the model, as a plugin can be loaded again, and the step is dropped); a lane that gave up its plugins
(the history keeps eight plugins in all, and lets go of the oldest steps' by taking them out of the lane it keeps, `KeptLane::take_plugins`) has them loaded again
into it (`put_plugins`: with the preset and the values each had). Then everything goes back in the reverse order: the lane (`SessionModel::insert_lane_at`, the audio
thread's `InsertInstrument`, at its place among the lanes and in its group; the pattern players are told the places the lanes have, and the cables of its group to the
members from its place on their ordinals), the tempo (`SetGlobalBpm`: the pattern player of the lane has the one there was when it went), and the cables of its group.
What the lane counted as held and what its pattern was playing is forgotten when it goes in (the notes were let go of while it was out, and a plugin that was not sent
the note-off keeps its voice, as it does when a lane's range is changed under a held note), and a group that is not there is no group. The words of a step are
`remove the lane of Oscillator`, or `remove an empty lane` for a lane that has no instrument. **A group is put back whole**, as a lane is: the controller takes out the cables
of its modulators first (each written down as a cable), and then the group, handed to a channel of its own (`RemoveGroup { group, keep }`, `Inverse::Group`: the model's
`GroupNode`, with its bus effects' slots, its modulators and its meter; the lanes that were its members, by node, read before it goes, since they are in no group once it has
gone; and `KeptGroup`, which the audio thread hands back with its bus effects and whatever they hold, its modulators, its meter, its volume and its pan). Undo gets the group
first (`Controller::recover_group`): it waits `recovery_wait` for it and **refuses the undo with the reason** if it did not come (`Pad was not handed back by the audio
thread`, or `Group 2` for a group that has no name, by the place it had), and a group that gave up its bus effects (`KeptGroup::take_plugins`) has them loaded again into
it (`put_plugins`: with the preset and the values each had); before anything is changed it refuses a group whose node, effects, modulators or cables are there already
and a member that is gone or is in a group (what was done after the removal was undone first, so that only a front-end that changed the session without an intent can have
made it so). Then everything goes back in the reverse order: the group (`SessionModel::insert_group_at`, the audio thread's `InsertGroup`: at its place among the groups,
the lanes in the groups from there on told the group they are in now, and its members, as the places their lanes have now, put in it) and then its cables, the last that
went first. The words of a step are `remove Pad`, or `remove Group 2`. With the group every removal of a node is a step: `Plan::Barrier` is no longer made for a node. **What is replaced is put back** too (step 35, the same
journal). An instrument that another replaces (`ReplaceInstrument`) is taken out of its lane as an effect is taken out of a chain: `Controller::install_instrument` makes a channel for it, asks the audio thread to hand it there
(`SwapInstrument { keep }`) and writes down, as `Inverse::Instrument`, the model's plugin (its values, its preset and its pins), the lane's note remap and pitch bend range (which go with the instrument they were for) and what
keeps the plugin, after the cables that drove it, each written down as a cable. Undo gets the plugin first (`recover_plugin`, told what the place asks of a plugin: an instrument has audio outputs, an effect has the chain's; one
that did not come, or that the history let go of, is loaded again with its preset and the values the user set), and then swaps it back in (`SwapInstrument`, with the remapper made again from the model) before the cables come
back, or empties the lane (`ClearInstrument`) if it had none, and refuses a lane that is gone or is no lane any more. The words are `change the instrument to Oscillator` (by the catalog's name for the new one), or `put
Oscillator in a lane` for a lane that was empty; redo replaces it again, loading the new instrument from the catalog. A preset that is loaded (`LoadPreset`) stays in the plugin and overwrites what it had, so what it overwrote is
written down before the model is given the preset's values (`Inverse::Preset`: the preset the plugin had, and the rows that the preset changes or that the user set over what they had, with their baselines,
`rows_a_preset_changes`: the preset that is loaded again would put its own over the user's); undo has the audio thread load the preset it had again, if the plugin has one by that name (a name that a session file gave and the
plugin has not is kept as the name), gives each row its value and its baseline in the model and sends the values (`SetParameter`, which centres the cables that drive them), and refuses a plugin that is gone, a bus effect's
and a lane that has no instrument any more. The words are `load preset Small Room`; redo loads the preset again. What a plugin holds that is neither a preset nor a parameter (a sample loaded in its own window, say) is not put
back by this: `Plugin` has no state to save. Two cables that drive each other's depths (only a file can make that) are not put back (`RemovedCable::still_driven`: one goes
while the other still drives its place, and once it has gone that place cannot be told from the next cable's), and a step is not made of such a
removal. A journal holds what it took by the places it had, so a structural edit that no intent asks for (the TUI's swap of two lanes, a direct call of a
method of the controller that takes something out, moves an effect or puts a lane in a group) forgets the history
(`Controller::forget_the_history_unless_an_intent_asks`: nothing is forgotten while an intent is carried out, which is a step or forgets it itself). A new
modulator and the cable to it are one intent, `Modulate`, and so one step in the words of the TUI's `m` and `l` (`modulate cutoff`, `learn cutoff`, `modulate rate`
for a setting of a modulator): it is a `Plan::Added`, whose step takes out the modulator, which takes its cable with it (`takes_out`: one node and one cable were
made), and redo makes both again under the ids they had. (There is no joining of the steps of several intents: the TUI's `m` and `l` were the one thing that
did it, until `Modulate`, and nothing asks for it any more.) Changes of one thing (one parameter, gain, setting or cable) a second or less apart are one step, so that a drag
is one (`SAME_STEP`; 200 steps are kept, `MAX_STEPS`; both are generated into `constants.ts`), a change after an undo ends what could be
redone, and an intent that is refused leaves no step. `Undo` and `Redo` are intents, answered as any are: each intent of the step is a
revision of its own (`Controller::undo` / `redo`), neither is a step, and one with nothing to do is refused (`there is nothing to undo`) and
is no revision; a step whose node has gone (a front-end took it out without an intent) is dropped, with the reason, and so is one whose cable's
id is in use (`cable 7 is there already`). What cannot be undone yet
(a group made of a lane that is in another (the lane leaves it, and the cables
of that group to it go), a lane put in a group or out of one that its old group drives, a `RemoveEdge` of cables that drive each other's depths,
and `Controller::set_instrument` and `clear_instrument`
for the TUI's `i` and `d`) empties the history, since a step from before it would be put back over what it changed; `Save` is no change. The TUI
asks by intents for what has them (the plugin selector, `d`, `n`, `g`, Shift+Up and Shift+Down on an effect, `m`, `l` and `t`), so they are in the
history; what has none is not (the Type row; the order of the lanes empties it) and the others do not empty it, and what is done to a pattern has intents and is no step (a
pattern is not put back by undo: Open question 11 of `docs/web-ui.md`). What Undo and Redo would do is
told to every browser as a message of its own, `History { undo, redo }` (the words of a button, or null), after the snapshot and the catalog
and after every change of it, before the answer to the request that made it (`web::owner`): it is no event, since a second event of one
revision is taken for a repeat. The page's buttons and keys ask for them (`ClientState.history`, `app/UndoRedo.tsx`) and so do the TUI's
Ctrl+Z, Ctrl+Y and Ctrl+Shift+Z; the mock engine has the same history (`client/history.ts`), pinned to the controller's by
`fixtures/history.json`.

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
gives a slot to each modulator (its output this buffer) and a second to each step sequencer (where it is: the step as the whole part of the number, how far into it as the fraction), to each modulator target
(the value that what it drives was set to this buffer, after modulation: a plugin
parameter, or a setting of a modulator, which the engine publishes as the last write to it) and two to each
lane and group (a peak and a clip count; the master's are slots 0 and 1,
`telemetry::MASTER`). They sit on the model's nodes (`ModulatorSlot.output`, `ModulatorSlot.position`,
`ModTargetSlot.effective`, `InstrumentNode.meter`, `GroupNode.meter`) and reach
the engine in the `ModTarget` or in `SetModulatorOutput` / `SetModulatorPosition` / `SetLaneMeter` /
`SetGroupMeter` (`modulator_slot_commands` makes the first two, the output's first). A slot is never reused, and a session that runs out of them just
has items without telemetry. The audio thread only stores (relaxed atomics, no
allocation, no lock). Lanes, groups, modulators and targets are created through
the loader or `Controller::{add_lane, add_group, add_modulator, add_target}`,
which allocate and send the slots; `SessionModel::check_ids` verifies them. The
`CLIP` light is the only reader of the meters' counts so far. Besides the slots, `Telemetry` has
the scope ring, the last frames of the master mix (see "Architecture").

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
one note path, so the Piano tab's Locked mode applies to them. The page also draws a piano under its
canvas (step 24; `web/src/app/piano/`): its keys are played with the pointer (`client/pointerPiano.ts`:
a glissando, a chord for several pointers; the lower on a key the pointer is, the harder the note, a velocity
from 20 at the key's top edge to 127 at its front edge (`velocityAt`; step 52), where the keys of the computer
play at 100: what a velocity control follows), a note that a key and the pointer both hold ends when both
let go (`client/holds.ts`, the provider's `useNotes`), and the keys light for the notes the server says
are held, whoever holds them (the `held` telemetry channel).

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
leaves the lane what else it has: its effects, modulators and volume. Putting an instrument in a lane
(`i`, `Controller::put_instrument`: from the catalog only, one with an audio output, like an effect)
replaces the one it has or fills an empty lane, under the lane's own node. Either way the cables that
drove the old plugin's parameters, a lane's and a group's, are taken out first (`take_out`, with what
drives their depths): a cable keeps its parameter's *index*, so left in place it would drive whatever the
new plugin has there. A note remap goes with the plugin too, since the audio thread drops the remapper
with it (`InstrumentNode::forget_remap`).

Effects can be reordered within an instrument (or a group bus) in the Session
tab (`Shift+Up` / `Shift+Down`; `Controller::move_effect` / `move_bus_effect`, which the page's
buttons reach by the intent `MoveEffect`). Instrument volume is applied after the instrument's output and before its
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

**Exit.** `main` calls `run()`, which stops everything that is Tang's, in order, and then
ends the process itself: it flushes stdout and stderr, prints an error as `Error: …`
(status 1) and calls `libc::_exit` (Unix; `process::exit` elsewhere). The ordinary exit runs what the plugins'
native libraries have to run at exit while the catalog scan, which may still be in one of
them (a quit a moment after the start finds it there), is in them, and the program crashed
on the way out (SIGSEGV or SIGBUS) about one time in twenty. `tests/cli.rs` holds what is
printed and the statuses (0, 1, and 2 for a command line that is not one), and
`tests/serve.rs` interrupts a `tang serve` early forty times in a row.

The shared LV2 world (`plugin::Runtime`) is created lazily on the first LV2
plugin load, not at startup — building the world scans every installed LV2
bundle, which sessions without LV2 plugins shouldn't pay for.

MIDI-to-audio communication via bounded MPSC channel (crossbeam-channel, capacity 1024).

Audio thread logging uses `log::debug!()` for per-event messages (filtered out at default
Info level) and `log::info!()` only once on first callback. No real-time safety issue at
default log levels.

**The scope ring.** The audio thread keeps the last 8192 frames of the output mix (`telemetry::ScopeRing`, `Telemetry.scope`: 170 ms at 48 kHz, both sides, every sample a 32-bit cell of its own), stored at the end of
`AudioGraph::process`, after the master meter, with no lock and no allocation, and any thread copies the latest frames out (`latest`: at most `SCOPE_READ_MAX`, 2048, a quarter of the ring, so that what is read is not written over
while it is read; a copy that the writer overtook is made again, up to three times, and then taken as it is). The TUI's Scope tab reads it on every frame that it draws, and the web runtime's pusher on every tick for a client that has asked
for the `scope` channel (see "Web UI"). Before the first buffer it is silence.

**The probes.** Besides the master's ring the audio thread has a pool of eight (`telemetry::PROBES`, `Telemetry.probes`: rings of the same kind) for the signal at the output of a node: of a lane's instrument (after
its volume), of each effect (after its mix) and of a group (after its pan; a bus effect, after its mix), stored by taps in `InstrumentLane` and `Group::finish` into the ring that the controller has told the audio thread the node has
(`GraphCommand::SetProbe { target, ring }`, `ClearProbes`; a lane or a group that nothing listens to has an empty list of taps, a check at each stage). The controller says which ring each watched node has (`Controller::set_probes(Watcher, &[NodeId])`,
`src/app/probes.rs`: a watcher is a client of the web interface or the terminal, two watchers of a node share its ring, a node that no ring is left for has one when one is free, and what has no signal has none: a modulator, a lane that has no
instrument, the master) and keeps the audio thread's taps where the nodes are as the structure of the session changes (`reconcile_probes`, from `conclude` and `poll`). Which node a ring is for is in the telemetry
(`Telemetry::probe_node`) for the pusher, which sends each client the frames of the nodes it watches (channel 4: see "Web UI"). The terminal is a watcher too, `Watcher::TERMINAL`, for the scope of a node on its Session tab (see "Scope tab"), so the
probes are in every build; `keep_probes_of`, which the owner of the web interface calls for the connections that have closed, is with the `web` feature or in tests only.

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

**Denormals.** `audio::Renderer::render` has the floating point unit flush denormals to
zero for the length of each block (`NoDenormals`, a guard: FTZ and DAZ in the MXCSR on
x86_64, FZ in the FPCR on aarch64, nothing on another processor; the state it found is put
back when the block is done): a number nearer to zero than the smallest normal 32-bit float
(1.2e-38), in what a filter, a reverb or an envelope computes or what it computes with, is
zero. The tails of these run down through the denormals (more than half the samples of the
two seconds after a note through a filter that is shut were denormal, f149) and arithmetic
on them takes many times as long on some processors (a hundred times on x86; not measured
here). It covers the plugins' own code as well as the built-in ones, and it is a guard of
the block and not a setting made once on a thread, since a host's callback is not always on a
thread of its own and a plugin may change the unit. The null backend and the sound card's
callback render through the same `Renderer::render`, so both have it.

The flush does not end a tail, and a tail that does not end is not silence. A state that moves
by a product of its own distance to something (`s += g * (x - s)`, the integrators of the
filter) stops where that product is flushed, near the smallest normal number over `g`, and
stays there: after a note the built-in filter put out a constant of 2e-38 to 5e-35 (by the
type and the cutoff; the bandpass and the notch, and the ladder and the MS-20 at cutoff 0.9,
ran down to zero), the same for the 70 seconds it was watched, so that a scope never read
exactly zero (f160: the browser test of the scope failed four runs of twelve on it, a side
effect of f149's flush: through the tracer's filter, with the flush 12 of 15 places of the LFO
at which a key is let go left a tail that did not end in 8 seconds, and without it none did;
a filter at a constant cutoff had stalled without it too, at the denormal numbers, 1e-45 to
3e-42, for the 30 seconds it was watched). The filter lets go of each of its states that is
under `TAIL_FLOOR` (1e-20, −400 dB) at the end of every block, so that a tail runs down by its
own arithmetic, far above the numbers it stops at, and then is zero
(`a_note_through_a_filter_of_each_type_and_cutoff_ends_in_silence_after_it_is_let_go`). The
built-in delay and reverb, at their defaults, ran down to zero within 100 seconds (watched
for ten minutes); a plugin of another make has what it has.

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
  stay click-free. Voices ring until their release completes. The voices are in a map with room for every key, made with the oscillator, so a note-on
  does not allocate on the audio thread (`notes_that_start_sound_and_end_do_not_allocate` counts).
  Four parameters were added after `pan` (so the others keep their places), and what they start at changes nothing: a chord of each waveform is held
  to what it rendered before them by `the_oscillator_renders_what_it_did_before_it_had_a_unison_and_a_pulse_width`.
  `unison` (a choice of 1 to 7 oscillators for each note, named by how many: the value is the place in the list, 0 is one; default 1) with `spread`
  (0–1 semitone, default 0.15: how far the outer oscillators are from the pitch of the note, each way, the others evenly between) and `width` (0–1,
  default 0.5: how far across the channels they are put, the lowest on the left and the highest on the right, by a law of equal power, so that a
  channel keeps the level of one oscillator whatever the width is, and not by a balance, which would take them out of the other side); both do
  nothing for a unison of one, and a single channel has no width. The oscillators of a note start in different places of their cycles (the first at
  its beginning, as a note has always begun), and the sum is brought down by the root of how many there are. A change of the unison while a note
  sounds adds or takes away an oscillator at once (a click: it is a choice, and not meant to be swept). `pulse` (0.05–0.95, default 0.5) is how much
  of the cycle a square is high: the offset of a pulse that is not a half (its mean) is taken out and it is scaled so that its higher side is at most 1,
  so that a half is the square it was and a narrow pulse does not carry a hum; the other waveforms have no use for it. A pulse is as naive as the
  waveforms are (no band-limiting: it aliases at high pitches, a narrow one more than a half, as the square and the saw do).
  TODO: the voices are in a hash map that is walked in an order that changes from one run to the next, so that the sum of a chord differs by a rounding
  from one run to another (the pinned test compares to a millionth); a fixed array of the 128 notes would have a fixed order, and would be faster.
  The old
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
- **Delay** (`builtin:delay`) — a stereo delay that puts out its echoes only, as the reverb
  puts out its tail only: the host's `mix` (0.5 when it is added) is the blend with the sound
  put in, so that what is heard first of a note is the note, and an echo of it one `time` later.
  Parameters: `time` (s, 0.001–2, default 0.375), `feedback` (0–0.95, default 0.45), `tone`
  (0–1, default 0.7: a one-pole low-pass in the feedback, exponential from 500 Hz to 20 kHz, and
  none at all from 0.999 up, so that the first echo is always the sound as it was put in and each
  repeat is darker than the one before) and `pingpong` (0–1, default 0: how far the feedback
  crosses to the other side, and the sound put in goes to the left as the sum of the two sides,
  so that a mono sound starts on the left and goes from side to side; 0 keeps the sides apart),
  and, in a section of their own as the reverb's are, `mod_rate` (0.05–10 Hz, default 0.5) and
  `mod_depth` (0–10 ms, default 0: off): a sine that moves the time of the left side, and a cosine
  that moves the right side's, a quarter of a cycle apart, by `mod_depth` milliseconds each way
  (a tape echo's wobble at 1 or 2 ms and 0.5 Hz, a chorus at 5 ms and 2 Hz; a modulator can move
  either, `mod_depth` over a range of 10 ms, where a cable to `time` has a range of 2 s). A change
  of the depth is gone through over 50 ms. Four presets (Slapback, Tape Echo, Ping-Pong, Dub Echo;
  Tape Echo has a wobble of 1.5 ms and Dub Echo of 0.8). A change of `time` (a knob, a
  modulator) is gone through by a one-pole of 10 ms and no faster than half a sample of delay for
  each sample that goes by, so a jump is a sweep in the pitch of the echoes, within an octave
  down and a fifth up, as on a tape echo, and never a click or a held sample; the delay's place
  is a 64-bit number, since a one-pole in 32 bits stops short of what it follows by as much as the
  steps of its number are over its coefficient (0.1 of a sample at 4,800 samples, and then every
  echo is a fraction of a sample from where it was asked for). `feedback` and `pingpong` are gone
  through over 5 ms. The echoes are soft-clipped (`soft_clip`, shared with the reverb), so that a
  feedback of 0.95 on a loud sound stays within full scale, and a sound that is not a number is not
  put in (it would stay in the echoes for ever). The lines are made for the longest time, and
  nothing is allocated while it runs. `examples/echo.toml` is a session with it.

TODO: a delay that follows the session's tempo (a `time` that is a note value at `bpm`), and a
finer step for a modulator's depth (a cable's depth is a fraction of the range, so that ±5 ms on
the `time` of a delay is 0.0025 of its 2 s, finer than the page's slider (0.01) and the terminal's
keys set it: a file can say it, as `examples/echo.toml` does, and `mod_depth` has the milliseconds).

## Plugin I/O handling

How Tang handles common plugin I/O mismatches:

- **Multi-output instruments** (e.g. Pianoteq with 16 outputs): the chain uses
  the first stereo pair (channels 1-2) and discards the rest. This is the standard
  main mix output for all known multi-output instruments.

- **VST3 parameter lists**: a VST3 plugin's parameters are listed without those that it hides (`kIsHidden`) or says are read only
  (`kIsReadOnly`), and without the placeholders that it makes so that a host can send MIDI controllers through its parameters: the
  parameters that its `IMidiMapping` points at, over 16 channels and 130 controllers, that cannot be automated (`is_listed`,
  `midi_controller_parameters` in `src/plugin/vst3.rs`). Pianoteq 9 has 2,080 of them among its 2,294 and ROLI Airwave Player 2,080
  among 2,089, so what is left is 214 and 9; `tang enumerate` counts what is listed. The routing of controllers to the plugin is
  by parameter id and is as it was. (A test of Pianoteq 9 is `#[ignore]`d: `cargo test -- --ignored`, with it installed.)
  A VST3 parameter that has 1 to 64 steps (`stepCount`) is a list, an enum to the page and the terminal: its labels are what the plugin says at
  each step (`getParamStringByValue`: "Off" and "On", "EXP" and "LIN", Studio Drums' "Scale Type" and "Root Note", Pianoteq's 30 "Bounce Sync
  Speed"s), and its value is the step, 0 to n, since VST3's steps are k/n whatever the plain range that the plugin says (`is_list`,
  `step_to_normalized`; Pianoteq's plain ranges are all 0 to 1). A session that has such a parameter at a plain value that is not a step (0.34 for
  the 10th of 29) reads it as the step 0.34, which the model rounds: only a list of more than two, of a plugin whose plain range is 0 to 1.

- **Sidechain inputs**: effects with more audio inputs than the chain provides
  (e.g. Calf Reverb with 2 audio + 1 sidechain = 3 inputs) get silence on the
  extra ports. Sidechaining is not supported.

- **Atom sequence ports (LV2)**: only passed to plugins that declare atom
  sequence input ports. Effects without them (e.g. ACE Reverb) get no event
  buffer — avoids livi's AtomSequenceInputsSizeMismatch error.

- **Channel count validation**: the chains are stereo (`plugin::chain::CHAIN_CHANNELS`,
  2), and an effect must have as many audio outputs. The audio thread turns away
  one that has not (it logs why and hands the plugin back), and one rule
  (`app::controller::outputs_fit_the_chain`) refuses it first: in
  `Controller::add_effect` / `add_bus_effect`, in undo (a plugin that is loaded
  again) and in the loader, so the model never lists an effect the engine does not
  have. A session file that names one is refused when it is loaded, with its place in
  the file and the plugin's own name (`instrument[0].effect[1]: Foo has 1 audio
  output, and an effect here needs 2`; `group[0].effect[0]: …` for a group's bus).
  The instrument may have more outputs (the first two are used, the rest discarded).

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
  `SetGain`, `LoadPreset`, `SetModulatorField`, `SetSteps`, `SetEdgeDepth`, `SetRange`, `SetMute`, `SetSolo`, `RemoveEdge`,
  `AddModulator`, `AddModEdge`, `Modulate`, `AddEffect`, `AddInstrument`, `ReplaceInstrument`,
  `SetNodeUi`, `SetPinned`, `SetBpm`, `SetRecording`, `ClearPattern`, `SetPatternField`, `MoveEffect`, `AddGroup`, `SetLaneGroup`, `RenameGroup`, `RemoveNode` (a modulator, an effect, a lane
  or a group), `Save`, `SaveAs`, `Undo` and `Redo` (see "Session state"). Effects are
  added by `Controller::add_effect` / `add_bus_effect` (loaded here, from the plugin
  catalog only, at the end of a lane's chain or a group's bus; the TUI's selector asks
  them), which the intent `AddEffect` wraps (browsers choose from the catalog the server
  tells them; the mock engine's nodes for the built-in effects are a fixture Rust writes,
  `web/src/client/fixtures/builtins.effects.json`). Instruments are put in by
  `Controller::put_instrument` (a lane's, which the TUI's `i` asks) and `add_instrument_lane`
  (a new lane), from the catalog only, which `ReplaceInstrument` and `AddInstrument` wrap
  (`builtins.instruments.json` is the mock engine's node for each built-in instrument).
  A preset is loaded by `LoadPreset` (the instrument of a lane, or an effect of a lane's chain; a
  bus effect is refused, since the audio thread's command addresses a lane's slot), which also
  centres the cables that drive the plugin on the preset's values; the mock engine answers with
  what the controller says each built-in preset does (`builtins.presets.json`, which Rust writes).
- **Model**: graph-shaped (nodes, ports, edges, layout) with stable `NodeId`s
  (and `EdgeId`s for modulation edges); today's lane/group/modulator structure
  is projected into it: `SessionModel::graph()` (`src/app/graph.rs`) gives the
  nodes (instrument, effect, modulator, group, the master as `NodeId::MASTER`),
  their `parent`s and the edges between ports (`audio`, `member`, `modulation`
  with the target's depth as `amount`), and `Controller::snapshot()`
  (`src/app/snapshot.rs`) is that plus the revision, the telemetry layout and
  what belongs to the session as a whole (dirty, file name, tempo, piano). A lane's
  pattern is on its node (`NodeKind::Instrument.pattern`, a `PatternInfo`, or null while it has none). Node
  layout is stored in the session TOML as optional `ui = { x, y }` tables that the
  engine and the TUI ignore (`Node.ui` in the graph; see "Node positions (`ui`)"), and so is which of a plugin's parameters its node
  shows as knobs (`Plugin.pinned` in the graph; see "Pinned parameters (`pinned`)").
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
  result. The tracer scene (`web/src/client/fixtures/tracer.toml`: the tests' own
  copy of what `examples/tracer.toml` was, since a person who runs the program on
  the example and saves writes over it) has a golden JSON snapshot
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
  plugin is installed; `tang serve` starts the scan, the TUI its own); what Undo and Redo would do
  follows the catalog (`history { undo, redo }`, step 32: the words of a button or null, told again when they change, before the
  answer to the request that changed them); the
  telemetry frames are made by `web::pusher` at 30 Hz for the channels a client
  asked for (`meters`, `modulation`, `held`: a value for each of the 128 notes, 1 for a note that
  is held, from the same bits the Piano tab reads, and `scope`: the latest 2048 frames of the output mix, the left side and then the right, 16 KB a frame, made only while a client asks for it; and, from step 46, the signal at a node:
  a client says which nodes it wants with `probes { nodes }`, the whole list each time and at most 64, is answered with `probes { nodes }` (those that have a signal and a ring) and is sent a frame for each of them on channel 4, with the node's id where
  the layout's revision is, 2 × 2048 values, D47). `NoteInput` (`src/note_input.rs`) is the one note path of hardware
  MIDI, the virtual piano and the browser: the Piano tab's filter, the held-notes
  bits and the channel to the audio thread, with the notes a WebSocket holds ended
  when it closes. Tests: `web::` (in-process, with a real WebSocket client) and
  `tests/serve.rs` (the real binary on the null backend). See `docs/web-ui.md`
  §3.2, §7 and §8 for the as-built notes.
- **Frontend** (`web/`, built; steps 11 to 16 and 18 to 44): TypeScript (strictest settings) +
  React + Vite, a framework-agnostic client layer (`web/src/client/`: reducers,
  telemetry buffers and slot readers, the connection with its reconnection and token, a
  vanilla zustand store, the frame loop, `Drive` (what a control shows while the server
  catches up, one intent a frame), the engine's modulation laws for the knobs' rings, and
  the mock engine, which speaks the protocol in the page), generated protocol types,
  design tokens as CSS variables (`web/src/ui/tokens.css`), the controls
  (`web/src/ui/`: `Knob`, `Choice`, `LiveBar`, `LiveTrace`, `LiveEnvelope`, `Meter`, which draw live values by
  writing to the DOM or a canvas, never through React state) and the patch canvas
  (`web/src/app/patch/`: `toFlow`, a pure layout of the graph, drawn with React Flow:
  a strip for each instrument, its modulators above it, cables, a node's knobs bound to
  the session; step 14 added the controls of a modulator: an LFO's rate and waveform and an
  envelope's stages on the node (`Knob scale="log"` for a rate and a time, which run over
  decades), and a slider for the depth of each cable at its middle; step 15 added building:
  a "+" menu on every lane and group (`MenuButton`) that adds an LFO, an envelope or a MIDI
  control, a "×" on every modulator, every effect and every cable, a list at the foot of a modulator that draws
  a cable to any parameter it can drive (for a keyboard: a `MenuButton` with the plugins as
  headings, which finds an item by its first letters), and a cable dragged from a
  modulator's output to a parameter's port, the ports that take it marked while it is
  dragged; `client/editor.ts` is where the page asks for them and `Notices` /
  `NoticeBoard` is where it says what the server refused; where a cable may end is the
  controller's rule, read from the graph by `client/reach.ts` and pinned to the controller's by
  a fixture it writes, `web/src/client/fixtures/reach.json`). Step 16 made the page's keys a
  piano (`client/keyboard.ts`, `app/useKeyboardPiano.ts`; see "Virtual piano"). Step 18 made the
  page save the session (`app/SessionBar.tsx`: the file's name, "Saved" or "Unsaved changes", a Save
  button and Control+S / Command+S, which keep the browser from offering to save the page; `Editor.save()`). Step 49 put the
  tempo in that bar (`app/Tempo.tsx`, a field of beats a minute that is a `spinbutton`: what is typed is asked for as `SetBpm` with Enter or on leaving the field,
  Escape gives it up, and the arrow keys turn it by one, by ten with Shift, each step asked as it is made, so that a held key is one step of the history; the server
  holds it to 20–300 and the field shows what it was held to; `Editor.setBpm`; the field is the page's `ui/NumberField`, which the length of a pattern uses too). Step 51 put
  the pattern of a lane in the page: a block under the controls of every lane, an empty lane too (`app/patch/PatternCell.tsx`: a button that records, `Editor.setRecording`,
  red and "Stop" while a recording goes on; a menu that clears the pattern, `Editor.clearPattern`, which asks first, in words that say it is for good; the length, a
  `NumberField` of 1 to 32 beats, `Editor.setPatternField`; the switches Play, Loop and In key; and a line, `client/patternWords.ts`: "4 beats · 3 notes · 90 BPM", the tempo
  it was recorded at, or "Counting in, then 4 beats at 120 BPM: play now" while it records). The block is as tall whatever it shows (`PATTERN_HEIGHT`, 97 px measured in a
  browser, which the layout adds to every lane's node), and a lane that has no pattern has it too with what has no use dimmed (its length is the one its first recording
  takes, `PATTERN_DEFAULT_LENGTH`). The mock engine does what the engine does with a recording (`MockServer.settle`, a timer that looks at the recordings every 50 ms while
  there are any: it counts in, takes the length of the pattern at the tempo it began with, counts the notes of the lane's keys that are played, and when it is done the lane has
  the pattern and the clients are given the session, as the controller does when it polls). Step 52 added the controls that follow the notes a lane plays (see "MIDI-learn
  modulator"): a velocity and a key, made from a lane's or a group's "+" menu and from a knob's "~" menu (`client/modulatorKinds.ts` has the words of every kind and tells a source's
  kind back), shown as MIDI controls (`velocity of the last note`, `pitch of the last note`), their rings by the laws (a key is bipolar around middle C: `KEY_CENTRE`, `KEY_SPAN` are
  generated constants), and followed by the mock engine as the engine follows the notes (`MockServer.#listened`: a lane's hear the notes in its keys as its transpose makes them, a
  group's the notes of its members' keys as they are played; no remap, no patterns). The piano under the canvas plays a pointer harder lower on a key (20 to 127, `velocityAt`).
  Step 53 gave every lane and group a mute and a solo (see "Mute and solo"): a "Mute / Solo" cell last in the node's grid (`MuteSoloCell`: two buttons, `M` and `S`, `aria-pressed`, named
  `Mute Oscillator` and `Solo Pad`; a press asks `Editor.setMute` or `setSolo` for the other way and the buttons show what the session has, nothing is changed before the answer, and the title
  says what a press does), a node that cannot be heard drawn dimmed (`tang-node--unheard`, `opacity: 0.55` and not brighter under the pointer: a lane or a group that is muted, or not soloed
  while something is, and the effects in the chain of one; `audibleAt` over `audibleOf` in `client/audible.ts`, which is the engine's law written again and pinned to a run of the engine over every
  combination of the flags of two groups and four lanes by `fixtures/audible.json`, 4,096 cases), and the mock engine's meters and scope silent for what is not heard (`MockServer.frame`; no fade).
  The client's store also keeps the server's plugin catalog (`ClientState.catalog`, step 19), and
  every lane and group has an "fx" menu that puts an installed effect in its chain (`client/catalog.ts`,
  `AddEffectMenu`; `Editor.addEffect`). Step 20 added the instruments: every lane has a "♪" menu that
  puts another instrument in it (`ChangeInstrumentMenu`; `Editor.replaceInstrument`) and the bar a
  "+ Instrument" menu that adds a lane with one (`app/AddInstrument.tsx`; `Editor.addInstrument`); the
  canvas moves to a node the page has added when it is not all in the view (`client/reveals.ts`,
  `app/patch/view.ts`), and not for a node another front-end adds. Step 21 added the presets: every
  instrument and every effect of a lane whose plugin has some has a "≡" menu that loads one
  (`PresetMenu`, `presetsOf`; `Editor.loadPreset`), the one loaded last marked and the menu opening on it
  (`MenuItem.current`). Step 22 added all the parameters of a plugin (a node shows the first eight as knobs):
  a "⚙" button on every instrument and effect docks a panel beside the canvas with a filter by name and a
  page of forty rows at a time, turned like the knobs (`ParamPanel`, `ParamRow`, `client/paramSearch.ts`;
  Escape or the button again closes it). Step 23 added the keys of a lane: every lane has a "Keys" cell
  with a lowest and a highest key (or `any` at an open end) that asks `SetRange` (`RangeCell`; the notes are
  named as in the TUI, `client/notes.ts`). Step 24 added the piano: under the canvas, the keys of a
  keyboard in the scale of the session (the root gold, the other notes teal), lit while a note is held
  and played with the pointer, with the scale as two choices and a "Lock to scale" toggle
  (`app/piano/`: `Piano`, `PianoBar`, `PianoKeys`; `client/pianoLayout.ts`; the intents `SetScale` and
  `SetLocked`). Step 25 made the nodes movable: a node is dragged by its header (the controls are
  `nodrag`), the page sends `SetNodeUi` once, when it is put down, every browser draws the node there,
  the session saves it, and a button on the canvas, Arrange, arranges the nodes again (`Editor.moveNode`,
  `resetLayout`; `useMovableNodes` and `app/patch/moving.ts` keep a node in hand while the session is laid out again; a node that the layout would put
  on one that was placed by hand, or within 16 pixels of it, is moved clear of it, below it and above it for a modulator: `clearOfPlaced` in `toFlow.ts`). Step 26 drew
  the cables between modulators: an LFO's rate and the stages of an envelope have a port on the top edge of their knob as a plugin's
  parameters have, a cable is dragged to one from another modulator's output (or chosen from the list at the foot of the
  modulator, which lists those settings after the parameters, under a heading for each modulator), and a modulator that
  drives the setting of another is drawn a row above it in a layout from nothing, so that a cable runs down (`app/patch/toFlow.ts`: `tiersOf`; a cable that is drawn leaves the nodes where they are, step 44;
  `client/reach.ts`: `fieldsOf`, pinned to the controller's rule by `fixtures/fields.json`; `Editor.addCable` takes a port). Step 27 made
  the modulation visible: a modulator's node draws what it has put out for the last four seconds as a line that scrolls (`ui/LiveTrace.tsx`,
  from the page's own samples of the telemetry, `client/trace.ts`: a rate that another modulator drives is seen to wander), and a cable is as
  bright as the output of its modulator is high (`app/patch/cableLight.ts`). Step 28 put the effects in order: every effect has
  two buttons that move it one place earlier or later in its chain, a lane's and a bus's alike, the one that cannot go that way
  dimmed (`MoveEffectButtons` in `app/patch/nodes.tsx`, `chainPlaceOf`; `Editor.moveEffect` asks the intent `MoveEffect`, which the
  mock engine does as the controller does, pinned to it by `fixtures/moves.json`), and the canvas lays the chain out in the new
  order; a browser lets go of the focus of an element that the page moves in the DOM, so the button that was pressed is given it
  again (`keepFocus` in `app/focus.ts`). Step 29 drew the shape of an envelope on its node: the line its four stages make, with a dot where
  the envelope is while notes are played (`ui/LiveEnvelope.tsx`, `client/envelope.ts`: a stage's width is the logarithm of its time, and the
  dot's stage is told from the level and which way it was last going, since the telemetry carries no phase: `courseAfter`). Step 30 let a person
  choose the knobs of a node: each row of a plugin's panel has a pin (`Show cutoff on the node`, pressed when the node shows it) that adds
  the parameter to the node or takes it off (`client/pinning.ts`: a node shows what is pinned to its plugin, or the first eight, and the
  target of a cable while the cable is there; `Editor.pinParam` keeps the other knobs as they were, `SetPinned` replaces the list), and a
  button under the count gives the node back to the first eight; the pins are the session's, so every page has them and the saved file
  has them by name. Step 31 brought the groups into the page: every lane has a "Group" cell, a choice of none, each group by its name or
  a new group with the lane in it (`GroupCell`; `Editor.setLaneGroup` and `addGroup`, which show the canvas the group, since the layout
  moves it to where its members are), the bar has "+ Group" (`app/AddGroup.tsx`), the name of a group is changed in place (a ✎ button
  turns it into a field: Enter or leaving the field asks `RenameGroup`, Escape asks nothing; `GroupNameField`), and a lane or a group is
  taken out by a "×" that opens a menu that asks first, in words that say what goes with it and what stays (`RemoveLaneOrGroupMenu`,
  `removalOf`; the only lane of a session has none, and there was no undo for it then). The mock engine does all of it as the controller does
  (`fixtures/groups.json`). Step 32 gave the page Undo and Redo: two buttons in the bar of the session that say what they would put
  back or do again (`app/UndoRedo.tsx`, from `ClientState.history`; `Editor.undo` and `redo`) and the keys Control or Command+Z,
  Control or Command+Shift+Z and Control+Y (not in a field that takes text, which has its own undo, and not when there is nothing to
  do); the mock engine keeps the controller's history (`client/history.ts`, `fixtures/history.json`). Step 33 put what is added in it
  (effects, instruments, modulators, cables, groups and the group of a lane: the page's "Undo add Reverb" takes the effect out and "Redo add
  Reverb" makes it again under the id it had), with the mock's ids and slots pinned to the controller's after every undo and redo by the shape
  of the session that the fixture records for each change of structure (`client/counters.ts`, `client/membership.ts`). Step 34 put what is
  taken out in it, as it was: a cable, a modulator and an effect (a lane's or a bus's, with the very plugin that the audio thread hands back) are put
  back by "Undo remove an LFO" or "Undo remove Reverb" with their cables, at their places and under their ids, and so are a lane and a group, whole ("Undo remove the
  lane of Oscillator", "Undo remove Pad": the group with its bus effects, its modulators and its members); their "×" asks first all the same, and says "for good" (see the
  TODO on its words). Step 35 put what is replaced in it: a preset that is loaded ("Undo load preset Small Room": the preset and the values the plugin had) and an instrument that another replaces
  ("Undo change the instrument to Oscillator": the very plugin that the audio thread handed back, with the cables that drove it and the lane's note remap); what still empties the history is a group made of a lane that
  is in another and a lane put in a group or out of one whose modulators drive it. Step 36 drew the cables to the depth of a cable: every cable that starts at a
  modulator is a row at the foot of its node (`CableRows`, `PatchNodeData.cables`; the layout counts them, `nodeHeight(node, knobs, cables)`), with a port on the node's left edge named after the cable
  (`depth-<id>`: `DepthPort`, `depthHandle`) where a cable from another modulator ends to drive how deep that one goes: it is dragged to the port (the whole row takes it while one is dragged) or picked
  from the list at the foot of a modulator, under the modulator whose cables they are (`depth → Filter: cutoff`); the modulator that drives the depth of another's cable is a row above that one, as for a setting;
  the cable is drawn round the node it ends at, with its slider on the run under what drives it (`cableRoute`); and a cable that ends at a depth is told by the cable whose depth it is (`ADSR → depth of LFO → Filter: cutoff`:
  `endLabel`, `cableWords` in `client/names.ts`, which the editor uses as well for what it says of a refusal). Step 37 put the TUI's `m` and `l` on the page: every knob (a parameter of a plugin that is
  shown as a knob, and the rate and the stages of a modulator), every row of the ⚙ panel and every row of a modulator's cables has a `~` button (`ModulateMenu`, `app/patch/ModulateMenu.tsx`: quiet until the pointer or the
  focus is on the control, in the corner of its knob) with a menu of a new LFO, a new envelope, a new macro, a new MIDI control, a new velocity and a new key (step 52), under "New", and of the modulators that are there and can still drive it, under "Attach". A new
  one is made for the control with its cable in one intent, `Modulate` (`Editor.modulate`: a MIDI control is armed to learn and its cable is at the whole range, as the TUI's `l` has it, the others (a velocity and a key too) at half depth; the canvas is
  shown the node it made), and goes in the rack that reaches the control (the lane's for a lane's plugin, the group's for a bus effect, the modulator's own for its rate, a stage or the depth of its cables, where it is a
  sibling that drives it); one that is there gets a cable (`Editor.addCable`, the intent `AddModEdge`). What could drive a control is `driversOf` (`client/reach.ts`: `drivablesOf` read from the other side, tested against it
  over two fixtures), and the menu looks it up when it is opened (`MenuButton`'s `onOpen`), not when anything changes: there is a button on every knob. A control that is chosen from a list (a waveform, a parameter whose
  values have names) has no button, as the TUI's `m` leaves such a row out (a cable can still be dragged to its port). Step 38 let the page save the session
  under another name: a "Save as…" button after Save in the bar (`app/SaveAs.tsx`) opens a field in its place with the name of the file the session has,
  selected; Enter or OK asks `Editor.saveAs` (the intent `SaveAs`) for the name as it was typed, and the server says what a name may be and where the
  file goes (`session::plain_file_name`: the page has no rule of its own; the mock engine has it written out, `client/filenames.ts`, pinned to the
  controller's name by name by `fixtures/filenames.json`, 67 names); a refusal is said among the notices (the name in quotes, cut after 24 characters) and the field stays, Escape and Cancel leave, and
  `Control+Shift+S` and `Command+Shift+S` open it (Control+S is still the save). The bar takes a second row in a window too narrow for all of it (`flex-wrap`).
  Step 39 drew what a cable to a modulator's own setting does to it: the knob of an LFO's rate or of a stage of an envelope has the ring and the dot that a
  parameter's knob has (`ModulatorFieldControl`: the dot is the telemetry's value of the setting, which the engine publishes and the layout names at the port the
  cable ends at, and the ring is `settingExtent`, the extent of the last cable alone, since the engine's writes to a setting are last-wins and not summed as a
  parameter's are), and the slider of a cable whose depth another cable drives has a bar under it, from the middle, for the depth the engine makes of it
  (`CableDepthControl`, a `LiveBar`; the slider stays what the user set). The controller gives every cable a slot for that value (`Controller::add_target` and
  the loader), and the mock engine does as it does. Step 40 made the page say when the server is slow to answer: a request that has waited more than 400 ms
  (`SLOW_AFTER`) for its answer (a plugin that is slow to load: the program does it on the thread that answers, so everything else waits behind it) is
  counted by the client (`Client.slow`, a store of its own: the client's state is told to every control at every change, and a request goes at every frame of
  a drag), and a "Working…" light shows over the top of the canvas while any is (`app/BusyLight.tsx`: a progress bar with no value, since the page's one `status`
  is the connection's); nothing is blocked by it, and it does not say what is being done. Step 41 made the ring of a parameter that a lane's modulators and its
  group's both drive the lane's alone (`modulationsOn`, from the engine's order of writes: see "Application order" under "Group modulators"), made the mock engine's value
  of a parameter that several cables drive the engine's own (each cable's offset made from the value the user set and added, not chained), and had its MIDI control (the mock
  has no MIDI) rest where the engine's does. Step 42 gave the master node a scope under its meter (`ui/LiveScope`, drawn on a canvas and not by React: the wave of the output from where it goes up through the middle, so that a wave that goes
  round again and again stands still, with the right side under it where it is not the same, or its spectrum, whose levels fall slowly; two choices under it, the view and for the wave a time base of 5 to 43 ms, which are the viewer's own and
  not the session's). The page asks the server for the `scope` telemetry channel while the master node is on it (`Client.watchTelemetry`: counted, and asked again on every connection), works the spectrum out itself
  (`client/fft.ts`: the server sends samples, and a spectrum of them is a drawing too) and is told what the frequencies are by `Meta.sampleRate`. The mock engine's scope sounds like the notes that are held. The terminal's
  Scope tab draws the same (see "Scope tab").
  Step 43 made the ring and the dot of a rate, an attack, a decay and a release follow the engine's law for such a setting, a fraction of the knob's travel (see "Cross-modulation"):
  `settingExtent` takes the scale of the setting, the mock engine does as the engine does, and how a range is spread over a knob is in `client/travel.ts`, which `ui/knob-geometry.ts` exports too.
  Step 44 stopped the canvas from being rearranged under a person's hand: `toFlow(graph, remembered)` is given where it put the nodes last (`Flow.places`) and leaves a node where it was while its slot is the same
  (`slotsOf`: an effect is in the place of its chain that it is at, a modulator in its rack, a lane in the session, a group and the master in the column that the layout gives them, and what is in a group's strip in the slot of the group),
  so that a cable that is drawn or taken out, and a node that is added or taken out, moves nothing but what needs room: a node that has grown, or a modulator that is added to a strip that has no room above it, moves the rows below it down,
  strips whole (`makeRoom`), and a group's strip and the master go along with a lane that grows past them (a lane that is new goes below the ones that stayed, `clearOfKept`; a node on one that a person placed goes clear of it, last, `clearOfPlaced`).
  `useLayout` keeps the places and lays the nodes out from nothing when "Arrange" is pressed (a button in the corner of the canvas, "Arrange the nodes again" to a screen reader; lit while a node has a place or one is not where the layout has it, `Flow.arranged`),
  which gives the places back (`resetLayout`) and then fits the view to every node (`FitWhenArranged`). The view moves to show a node that is added where it cannot be seen.
  Step 46 gave every instrument that has a plugin, every effect and every group a button in its header (the sine wave, "Scope of X") that shows a scope of what comes out of it under its controls, with the view and the time
  base of the master's (`app/patch/NodeScope.tsx`: `ScopeButton`, `NodeScope` and `ScopePanel`, which the master is drawn with too): the page asks the server for the signal while the scope is there (`Client.watchProbe`, counted, and
  asked again on every connection: D47), which nodes show one is the viewer's own (`PatchView` holds the set, `scopes.ts` hands it to the nodes; it is not saved, not sent and not in the history), and the layout makes room for it
  (`toFlow(graph, remembered, scopes)`: `canScope`, `PatchNodeData.scope`, `NODE_SCOPE_HEIGHT`, 157 as a browser draws it; `useLayout(graph, scopes)`). The mock engine has eight rings and sounds like the notes that are held. A scope that is opened where it cannot be seen moves the canvas to the node, as a node that the page added does (`Reveals`, once the layout has the scope). The badge of a node
  (what kind of plugin it is) is under its name in the header, in small type, so that the name has what the buttons leave.
  Step 57 added the step sequencer (see "Step sequencer modulator"): a node with its rate (a knob with a port, as an LFO's), its glide and its length (knobs with none: `ModulatorValueControl`), its
  retrigger (a choice of off and on: `RetriggerControl`) and, above the trace of what it puts out, sixteen bars (`ui/StepBars.tsx`: a `role="slider"` for each step, a bar up from the middle for a value above nothing and
  down for one below it, the steps past the length dimmed, a playhead on the step the engine says it is at that is written to the DOM on the frames and never rendered). A stroke across them paints them
  (`client/stepbars.ts`: `columnAt`, `stepValueAt`, `paintTo`, `playheadStep`; a pointer that jumps bars sets the ones between along the line, a bar has the value at its height to a hundredth and nothing near the
  middle), a key moves one (up and down by a fiftieth, the page keys by a tenth, Home and End to the ends; the arrows that go across move the focus from bar to bar, and one bar is in the tab order) and a double
  click puts one back to nothing; a stroke is all the sixteen values a frame at a time (`Drive.steps`, a `Control` of a list: `Control` is generic, and takes which values it will send), shows what it has painted until the
  server has answered and is one step of the history. It is made from the "+" menu of a lane or a group ("Step sequencer") and the `~` menu of a knob ("New step sequencer"), and its node is
  `STEPS_NODE_HEIGHT` tall (an LFO's and the bars and the gap above them: 321 against 249 as a browser draws them). The mock engine has the same sequencer (`stepsPlace`, the engine's wrap at the length, which
  puts out the position the playhead follows; a note starts it again when it is set to). `e2e/steps.spec.ts` runs the node, a stroke that is one Undo and its steps in the saved file, and a note that puts the playhead on the first step.
  What the controller
  clamps to (the ranges, what a new modulator starts as, whether a setting is turned by distances or by ratios, the order of the waveforms) is
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
  `reach.json`, next to it, which says where the controller let a cable be drawn, `fields.json`, which says
  which settings and cable depths of other modulators it let a cable be drawn to, `moves.json`, which says what
  it makes of moving each effect of a session to each of some places, `groups.json`, which says what it makes of adding a group,
  putting a lane in one, naming one and taking a lane or a group out, `history.json`, which says what it makes of scripts of intents and of time going by
  (the events, what Undo and Redo would do after each, the words of a refusal), and
  `builtins.catalog.json`, the built-in plugins as the catalog lists them, which the mock serves). The fixtures say numbers as the wire does
  (`app::testutil::wire`, not `serde_json::to_value`, which widens a 32-bit number to a 64-bit one).
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
parity) has begun, §11: the controller takes effects and modulators out of the chains (step 17) and the
page saves the session (18), puts effects (19) and instruments (20) in the chains, loads presets (21), reaches every
parameter of a plugin (22), sets a lane's keys (23), has the piano (24), moves the nodes of the canvas, keeping where
they are in the session (25), draws cables between modulators (26), shows what a modulator puts out (27), moves
effects along their chains (28), draws the shape of an envelope (29) and lets a person choose the knobs of a node (30);
steps 31 to 41 followed (groups from the page (31), undo in four steps (32 to 35), cables to the depth of a cable (36), a modulator made for a
knob from the knob (37), Save as (38), the ring and the dot of a setting that a cable drives (39), the light for a request that is slow (40) and the ring of a
parameter that a lane's and its group's modulators both drive (41): see "Frontend" above and `docs/web-ui.md` §12).
TODO: move a node with the keyboard (React Flow's own needs a node to be selected, which the canvas keeps them from being),
and snapping to a grid.
TODO: make the Type row (LFO to envelope) an intent: until then a browser is given a snapshot
for it (`Controller::resync`).
TODO: the canvas numbers the nodes of one name in the order of the graph (`displayNames`), so an effect that is moved past one
of its name changes name with its place (Filter 3 becomes Filter 4, and the one it passed Filter 3): a number that stays with
the node, by its id, would keep it, but would number a loaded session's nodes out of the order they are drawn in.
TODO: a trace of an LFO faster than 15 Hz aliases: the telemetry is 30 values a second (the engine's blocks are much shorter); a telemetry
channel of the lowest and highest output of each block, or a faster one, would draw it.
TODO: the light for a slow request says that the server is slow and not what it is doing (which plugin it loads). The terminal says it for a plugin chosen in its
selector ("Loading Pianoteq 9…") but not for one that a browser asks for, nor for an undo that loads one again: its loop is blocked then and draws nothing for
as long as the load takes. Moving the loads to a worker (`docs/web-ui.md` §3.1) would end it.
TODO: the bar under the slider of a cable whose depth is driven says where the engine has the depth, but not how far the cables that drive it can swing it (a knob's ring); a
slider has no room for one, and the bar's scale is the slider's whole width while the thumb's travel is inset by half a thumb at each end.
TODO: replacing an instrument takes out every cable that drove the old one; keep the ones whose
parameter the new plugin has under the same name (a vibrato on `detune` could survive a change of
oscillator): a cable would have to be drawn again with the new plugin's index.
TODO: telemetry slots are never reused (4,096 of them): a session that adds modulators and cables
for long enough runs out, and later cables have no live dot.
TODO: pinning from a knob on the node itself (a pin is set in the panel only), ordering the pins by dragging (they are shown in the order they
were pinned), and a node with no knobs at all (an empty `pinned` list is the front-end's own choice, the first eight; docs/web-ui.md D33).
TODO: the panel's sliders ignore what a parameter says of its steps (`stepCount`) and of its group (`group`): a stepped
parameter is turned as if it were continuous, and the filter looks at names only. (The VST3 adapter makes a parameter of 1 to 64 steps a list
of names, an enum, so that it is not turned as a number; what is left is one with more steps, and the groups, which no plugin of this machine reports.)
TODO: a lane added from the browser is full range and in no group (its own choice of group puts it in one; drawing its audio to a group is
not built), and an instrument put in from it starts at its own defaults (or at a preset the page then loads): taking a lane or a group out
asks first (see the TODO on the words of its "×").
TODO: the slider of a cable's depth is at the middle of the cable, which can be over a node's
edge, and two cables with the same middle put theirs over each other; sliders that move along the
cable would settle it (nodes can be placed since step 25, which helps but does not settle it; a cable that ends at a depth has its slider on the
run under what drives it, since step 36).
TODO: patterns as graph nodes (a lane's pattern is on the lane's node, as a `PatternInfo`, but has no `NodeId` of its own).
TODO: the page does not draw the notes of a pattern (a snapshot has how many there are), nor says where a recording is: it knows that one goes on, and not whether it is counting
in or recording, nor how far it is (the engine reports its end and nothing before it); a telemetry channel or an event for the phase of a recording would give a progress bar.
TODO: events of their own for what the audio thread reports to the controller (a
finished pattern recording, a captured MIDI-learn control) and for what the TUI
changes without an intent: until then connected browsers are given a snapshot
(`Controller::resync`).
TODO: the events of one intent share its revision, which a client takes for a repeat
if there is more than one (every intent makes one event today).
TODO: undo of what moves between groups: a group made of a lane that is in another and a lane put in a group or out of one whose modulators drive it (the cables of the group that has it go) empty
the history until their steps exist (`Plan::Barrier` in `src/app/history.rs`, `planOf` in `web/src/client/history.ts`).
TODO: the TUI's edits that have no intent (the Type row) are not in the history, and do not empty it; the order of the lanes with
Shift+Up / Shift+Down on a lane and `Controller::clear_instrument` do (a step that took something out puts it back by the places it had).
TODO: a cable that a file has to a cable that is not there (`mod_depth = [0, 0]` to a modulator with no cable) is centred on something else after undo of the
removal of its modulator than the loader gave it; it drives nothing, so it makes no difference to what is heard, and the tests leave it out.
TODO: the "×" of a lane and of a group asks first, in words that say it goes "for good" (`RemoveLaneOrGroupMenu`), from when a removal could not be undone: both can be
now; whether to keep the question is the user's to say, and the words are to be changed either way.
TODO: an all-notes-off for a lane that comes back by undo (and for the members of a group that does, and for an instrument that is swapped in or out): a note that was held when it was taken out stays held in a plugin that
was not sent the note-off.
TODO: the controller's layout of the telemetry lists a cable by the place of its modulator in the rack, and the mock engine's lists one that is drawn after
the others: a cable of an earlier modulator is told in another order (no reader depends on it; the fixture's scripts draw from the last modulator of a rack).
TODO: the TUI's `n` and then a choice from its selector is two steps, an add and a replacement: it takes two Ctrl+Z.
TODO: keep-alive pings for WebSockets.
TODO: the places that the layout remembers are the page's: a page that is opened again, and a second browser, lay the nodes that no person has placed out from nothing, so that the canvas is not what it was when it was left (docs/web-ui.md D45, Open question 6).
TODO: until the nodes are arranged a cable between two modulators that are in one row runs across what is between them, and its slider can be over a node (the cables' routes are made for the layout from nothing).
TODO: the view moves to show a node that is added where it cannot be seen, and may leave the others out of it.

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

- A patchable voice (track B, `builtin:patch`): a proposal, with what the engine has and lacks, the order of work and the ten decisions that
  are the user's, is `docs/track-b.md`; nothing of it is started
- Recording audio output to WAV file
- Auto-reconnect on audio device disconnect
- Background/daemon mode with system tray icon
- In-app volume control
- MIDI device selection tab
- Audio device selection tab
- A keyboard as a node: the notes of the computer's keys, an on-screen piano or a MIDI device as a node with a "MIDI cable" to a
  lane, in place of the lane's own key range, so that there can be several keyboards with ranges of their own (a split, a
  layer) and other sources of notes (a pattern, an arpeggiator, Web MIDI) are nodes as well (a port kind next to audio and
  modulation; today every note goes to every lane through `NoteInput` and each lane filters it by its range)
- Audio input as a source: a lane whose chain starts at an input of the sound card (a microphone, a line) in place of an
  instrument, through the same effects, groups and modulators (an envelope follower is a modulator to come with it); and
  from there Tang as a plugin (VST3 / CLAP) whose buffer comes from the DAW: `audio::Renderer` is already the one code the
  sound card's callback and the null backend run, so a plugin's `process` would be a third thing to drive it

## Notes for Claude

- This document serves as a design document for us.
- We use TODO:s (on new lines) to clarify what is yet to be implemented.
- Notify me when you find refactorings we should do before implementing new things.
- Notify me when you find discrepancies in this document vs how it actually works.
