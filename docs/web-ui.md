# Web UI — design

Status: **phase 0 is built** (the 13 steps of §12), and step 14 after it (a
modulator's settings and a cable's depth, edited in the browser and in the TUI);
phases 1 and on (§11) are planned. `CLAUDE.md` carries the short version and the TODOs; this file is the
long form. Update both as steps land (tick the boxes in §12, move TODOs out of
`CLAUDE.md` into the relevant section). Decisions are listed in §14 with their
status.

## 1. Goals and non-goals

Goals

- A browser front-end to the *same* Tang process (local or over the LAN). The
  audio engine, plugins and MIDI stay native. The TUI stays first-class.
- Parity with the TUI's Session and Piano tabs.
- A modular "rack-as-patch" canvas for experimenting with synth building and
  visual modulation (track A, §9.3), with modulation visuals driven by the
  engine's real values rather than re-simulated in the browser.
- A foundation that can grow: a typed protocol, a framework-agnostic client,
  performance budgets, and tests at every layer.

Non-goals (for now)

- Audio in the browser. Plugins are native binaries; the browser never
  renders audio.
- Plugin native GUIs. Tang has none; the browser is a parameter surface.
- Free-form audio patching and per-voice / audio-rate modulation. That needs
  engine work (track B, §13).
- Timeline or arrangement views, multi-user collaboration, mobile-first
  layout.

## 2. Starting point

As of commit `02f8b04` (branch `feat/midi-learn-mpe-expression`).

**The audio side is already UI-agnostic.** `AudioGraph` is owned by the cpal
callback and mutated only through `GraphCommand`s on a bounded channel (64).
Feedback comes back through `pattern_rx`, `learn_rx` and `return_rx`, and
through shared `Arc`s (`HeldNotes`, `PianoFilter`, `MidiMonitor`).

**The control side lives inside the TUI.**

- The session exists in five shapes: `session::SessionConfig` (TOML),
  `tui::Loaded*` (transfer structs), the TUI's live mirror (`InstrumentNode`,
  `GroupNode`, `PluginSlot`, …), `session::Save*` (save DTOs), and the audio
  graph. `main.rs::run_session` plus the two modulator loaders build them;
  `impl State` keeps them in sync, including index fix-ups for cross-mod and
  group ordinals (mirrored again on the audio side).
- Domain methods are welded to the cursor (`adjust_param` reads
  `chain_state.selected`), so no other front-end can call them. `tui/mod.rs`
  has 3 tests.
- Parameter values are mirrored by assumption. The live plugin is on the
  audio thread, so loading a preset instantiates a throwaway second copy of
  the plugin to read the values. Plugin loads block the key handler.
- In TUI mode nothing drains `return_rx` (removed plugins stay alive in the
  16-slot channel; once it is full, the audio thread's `let _ = try_send(old)`
  drops them on the audio thread), and MIDI hot-plug is never polled (only
  `play` does). Both fixed in step 5 (`Controller::poll`, in the TUI and `play`).
- The model never mirrored the engine's modulator-target fix-ups when an effect
  is removed or reordered (`fixup_slots_after_remove`,
  `remap_slots_after_reorder`, `remap_group_bus_after_reorder` in
  `plugin::chain`). After such an edit the TUI kept stale targets (shown as
  `→ fx1: cutoff`) and saved them with the old slots, so a reload could attach a
  modulation to a different effect. Found while writing the fix-up tests; fixed
  right after step 2. Still open: the engine's `RemoveEffect` / `ReorderEffect`
  handlers only fix the lane's own modulators, so a group modulator's
  `GroupMember` target on that lane's effect slots keeps its old slot (by
  reading the handlers; no test exercises it). The controller's remove /
  reorder intents (steps 5-6) are the place to fix both sides.

**Not plumbed to any UI yet:** modulator outputs, effective (modulated)
parameter values, meters/clip, scope.

**Engine facts the UI has to respect**

- Modulators tick once per audio buffer: ~190 Hz at 256 frames / 48 kHz (the
  macOS/Windows default), ~47 Hz at 1024 (the Linux default).
- Modulators are lane-global, not per-voice; an envelope shapes parameters,
  not voices.
- Modulation combines additively, `clamp(base + Σ offsets)`; an envelope
  treats the base as its peak (see "Modulator fields" in `CLAUDE.md`).
- `RemoveModTarget` / `RemoveGroupModTarget` are implemented in the engine but
  no front-end sends them yet. The canvas needs them to delete cables.
- Only three builtins exist: `osc`, `filter`, `reverb`.

## 3. Architecture

```text
control    browsers ─WS─▶ web ─intents─▶ Controller ─GraphCommand─▶ audio
           browsers ◀─WS─ web ◀─events── Controller ◀─notifications─ audio
           TUI ──────── direct calls ───▶ Controller
notes      browsers ─WS─▶ web ─▶ NoteInput ─MIDI channel─▶ audio
telemetry  browsers ◀─binary frames─ web ◀─atomics─ audio
```

`web` is the web runtime (feature `web`), `Controller` runs on the owner thread,
`audio` is the cpal callback.

### 3.1 Controller

`app::Controller` is a plain struct: no threads inside, a `&mut self` API. It
owns the live `Session` model, the engine handles (`cmd_tx`, `plugin::Runtime`,
the notification receivers, the `MidiManager`), the plugin catalog, the session
path and dirty flag, a revision counter, the undo stacks and the subscriber
hub.

```rust
impl Controller {
    fn apply(&mut self, intent: Intent) -> Result<Applied, IntentError>;
    fn poll(&mut self) -> Polled;        // notifications, catalog batches, loader
                                         // completions, reap returned plugins,
                                         // MIDI hot-plug; what changed (model,
                                         // catalog), so a front-end redraws only that
    fn snapshot(&self) -> Snapshot;
    fn subscribe(&mut self, sink: EventSink) -> SubscriberId;
}
```

It is driven by whichever thread owns the loop: the TUI loop (`tang`), a
headless loop (`tang serve`), or the TUI loop with a web runtime attached
(`tang --web`). Exactly one thread mutates the model, so nothing guards it
with a lock. The web runtime never touches the model; it sends requests over a
channel and receives serialized events and snapshots back.

A dedicated controller thread with the TUI as a client was considered and
rejected for now: it would force the TUI onto a replicated or shared read model
and make its actions asynchronous, for little gain. Because the controller is a
plain struct, moving it to its own thread later is a wrapper change.

Known cost: blocking work (plugin loads, preset-value mirroring) blocks the
owner, so a web client's plugin load stalls the TUI and vice versa. Phase 1
moves loads to a worker whose completions `poll()` picks up.

### 3.2 Web runtime (feature `web`)

- `axum` on `tokio`, on its own thread(s). Routes: `GET /` (embedded SPA) and
  `GET /ws`. Everything async stays inside this module; the audio, MIDI and
  owner threads remain synchronous.
- One task per connection with a bounded outgoing queue. If a client lags, the
  hub closes it; the client reconnects and resyncs from a snapshot.
- Telemetry frames are produced here, from the lock-free `Telemetry` atomics at
  the runtime's own cadence. The owner thread is not involved.

**As built (step 10).** The runtime is one thread (`tang-web`) running a
current-thread tokio runtime, started by `web::server::Server::start`; a few
browsers at 30 Hz are nowhere near what one core does, and one thread keeps every
future away from the synchronous rest of the program. Its parts, each in
`src/web/`:

- `security` (pure: the `Host` / `Origin` policy and the token), `hub` (the
  clients and their bounded queues), `connection` (what one client's messages
  mean: token, limits, notes), `owner` (what the owner thread does with the
  requests), `server` (the sockets), `pusher` (the telemetry frames), `runtime`
  (all of it built in one place and the headless loop), `mod` (`tang serve`).
- A client's queue holds 256 frames. A broadcast to a client whose queue is full
  evicts it: the connection task tells it (`Closing`, error `overloaded`, close code
  1013) and closes the socket, and the client reconnects and starts from a snapshot.
  A telemetry frame is only skipped for such a client. A write to a client that
  takes longer than ten seconds gives the client up too (a client that has stopped
  reading is only noticed when its socket buffers are full, which is megabytes).
- Requests reach the owner over a bounded crossbeam channel (1024); a request that
  does not fit is answered "busy" (`rateLimited`), and the owner never waits for a
  client: every send to a client is a `try_send`.
- One semaphore permit per connection bounds how many there are (32; the 33rd gets
  503) and tells a stopping server when they have all gone.
- Not built: keep-alive pings (a client that vanishes without closing is noticed
  when something is written to it). (What the audio thread reports to the
  controller, a finished pattern recording or a captured MIDI-learn control, has no
  event of its own, but reaches a connected client as a snapshot: §3.4.)

### 3.3 Real-time note lane

`app::NoteInput` is one lock-free component that does what the `MidiManager`
callback and `VirtualPiano` each do today: `PianoFilter` gate → `HeldNotes`
update → `try_send` on the MIDI channel. The browser's notes arrive on the
WebSocket, are handled in the web runtime and call `NoteInput` directly, with no
controller round trip. The runtime tracks the notes each connection holds and
sends note-offs when the connection drops, so a closed tab cannot leave notes
stuck.

### 3.4 The TUI and the web runtime on one controller (step 13)

`tang --web[=ADDR]` runs the TUI and the web interface on one controller. The main
thread owns it, and its loop, `tui::event_loop`, waits for the terminal, the browsers
and a tick together.

- **The loop.** crossterm can only be polled, so the terminal is read on a thread of
  its own (`tui/terminal_input.rs`: it polls with a 50 ms wait, which is also the most
  that dropping it waits, and sends what it reads, a failure included, over a
  channel). The loop `select!`s on that channel, the requests of the browsers
  (`Runtime::requests`, a clone of the owner's receiver; `never()` when there is no
  web interface, and again once the server is gone, or the loop would spin on a
  channel that has nothing more to give) and a 100 ms tick. Every turn: `tick` (the
  controller's `poll`, the catalog, the CLIP light), `Owner::refresh`, draw; then it
  waits; and what came is handled: the terminal's events that are waiting, all of
  them before the next frame, then `Controller::resync`; or a browser's request and
  those behind it. `event_loop` and `render` are generic over the ratatui backend, so
  the tests run the real loop against a `TestBackend`. `tui/web.rs` is the runtime as
  far as the loop needs it, with a stand-in for builds without the `web` feature
  (`Web` has no values there, and the loop has no cfgs).
- **Web to TUI.** A request is applied by the owner (`Runtime::serve`), which hands
  back the intents it applied; the loop reacts to them as `State::apply` does to the
  TUI's own (`react_to`: a group's volume is in its header in the tree). The rest of
  what the TUI shows it reads from the model when it draws, so a value a browser set
  is on the screen with the next frame, and a step the TUI takes starts from it.
- **TUI to web.** What the TUI changes through intents (parameters, gains, presets)
  reaches the browsers as the events `apply` publishes. The rest of what it does
  (`n`, `R`, `b`, `g`, `d`, `m`, `t`, …) it still does to the model directly, and
  nothing tells the browsers. `Controller::resync` finds it by comparing.
  `app::subscribers::Subscribers` keeps what its subscribers have been told: the
  snapshot the first one started from with every intent applied to it
  (`Snapshot::apply`: the rules of `Controller::apply` read from the other side, as the
  client's `applyEvent` reads them). `resync` builds the snapshot of the session now and
  compares; if they differ it bumps `rev` and gives the subscribers the snapshot
  (`EventSink::resync`). The web runtime's `HubSink` sends it as a `snapshot` message to
  the subscribed clients and sets the revision and the layout that the connections and
  the telemetry frames read. With nobody subscribed nothing is built. The loop calls it
  after each batch of the terminal's input, and `poll` calls it when the audio thread has
  reported something, so a finished pattern recording or a captured MIDI-learn control
  reaches a browser too (as a snapshot, not as an event of its own). Every TUI edit that
  becomes an intent leaves less for it to find.
  - The test that matters is `Snapshot::apply`'s: for intents of every kind (a value
    clamped, an enum rounded, a preset) the snapshot after the events is the controller's
    own. It found two rules the client's reducer lacked: an intent leaves the session
    dirty (`meta.dirty`), and the values a preset sets are the parameters' defaults, as
    the controller keeps them as the baseline (f27, f28, fixed).
  - A subscriber that keeps a copy the way a client does (the tests' `Mirror`) ends up at
    the controller's snapshot whatever was done to the session; an intent applied to what
    it has not heard of (a lane added in the TUI and not yet resynced) leaves a snapshot
    for it to take, not a wrong copy.
  - Cost: a `resync` that finds nothing takes about 0.13 ms in a debug build for 8 lanes
    with modulators (57 nodes, 50 KB of JSON), of which the snapshot is 0.1 ms; it runs
    once per batch of the terminal's input, not on the tick.
- **The browser's side** needs nothing new: the client takes a `snapshot` at any time
  (replacing its copy and the layout). A `layout` message is still sent for a layout
  that changes with no resync; one that a snapshot carried is not told twice
  (`HubSink::resync` sets it in `Shared`, and `Owner::refresh` compares with that).
- **What the TUI shows.** The tab bar says where the web interface listens and how many
  browsers are on it, `web 127.0.0.1:8765 (2)`, before the CLIP light and the tempo, and
  shortened to `web (2)` or left out so that it never touches the tabs. The Help tab has
  the address to open (with the token, if there is one) and, for an address beyond this
  machine, the two warnings `tang serve` logs. The address and the warnings are printed
  on stdout before the TUI takes the screen (the TUI's log is off). A port that is taken
  is an error before the terminal is taken over.
- **The flag.** `--web[=ADDR]` is the top level's, for the TUI alone: the address (an IP
  and a port, `127.0.0.1:8765` by default) comes only after an `=`, so that `tang --web
  x.toml` is a session; `--web-origin` (repeatable) needs it; written before a
  subcommand it parses and `Cli::check` refuses it (`serve` has `--web-bind`). In a
  build without the `web` feature it is refused before the session is loaded.
- **Checked.** The tests run the real loop with a real server and a WebSocket client on
  a thread of its own, which presses keys by sending them to the loop: a cutoff set in
  the browser is where the TUI's next step starts from, and the browser hears that step
  as an event; the tempo and a new lane set in the TUI reach the browser as snapshots; a
  browser that comes late is told the current revision; a group volume and a preset set
  in a browser show in the TUI; a stopped server does not make the loop spin. By hand:
  the real program on the null backend (nothing heard), the TUI in a pty read through a
  terminal emulator and a real Chromium on its page, 18 checks both ways, all passed
  (the cutoff and the resonance each way; the session's asterisk; a lane added in the
  TUI, with the values set before still in the browser; the browser counted in the tab
  bar and no longer once it closes; the TUI quits with status 0).
- **Costs and limits.** One thread runs the TUI and answers the browsers: a plugin load
  a browser asks for blocks the TUI and the other way (§3.1's known cost), and a browser
  dragging a knob makes the TUI draw once for each frame's request, up to 60 a second. A
  port that is taken was found only after the session had loaded, because the server needs
  the controller to start (f29); it is found at once now, by trying the address and letting
  it go (`web::preflight`, before the session is loaded; the server's own bind, later, says
  so too if the port is taken in between). The events of one intent share its revision, and a
  client takes the second of two for a repeat (f30; every intent makes one event today).
  The terminal reader was run on macOS only (crossterm's reader is used from a thread).

## 4. Model

### 4.1 Session and IDs

`app::Session` holds the instruments, groups, global BPM and a mirror of the
piano scale/mode, plus `next_id`. Every node gets a `NodeId(u32)`: allocated by
the session, never reused during a run but for what undo gives back (D36), not persisted. Modulation edges get
stable `EdgeId`s too. The audio thread keeps addressing by position (inst /
slot / index); the controller resolves an id to a position per intent, and the
TUI's index fix-ups (`fixup_tui_*`, `shift_tui_*`) move into the model so they
run in one place.

*Today:* the model is `SessionModel` (instruments, groups and the `NodeIds`
allocator; the BPM and the piano mirror still sit on the controller), the ids
exist for every node and `locate` turns one into a position, and the index
fix-ups already live in `app::model` (the `fixup_*` / `remap_*` functions and
`SessionModel::{remove_effect, move_effect}`). Modulation edges have ids too
(step 8): every `ModTargetSlot` has an `EdgeId`, a series of its own so that no
node id moved, allocated by the loader and by `Controller::add_target`, and
checked with the node ids after every key press of the golden tests. A target is
still a row of its modulator in the model; the id is what a client names it by.

### 4.2 Graph projection

The wire model is graph-shaped even though the engine is lane-shaped: nodes,
ports, edges, layout. The controller projects the lane/group/modulator
structure into it on demand and translates edge edits back into intents.

| Today | Node | Edges |
|---|---|---|
| `[[instrument]]` plugin | Instrument | audio out → first effect (or group / master) |
| `[[instrument.effect]]` | Effect | audio chain |
| `[[instrument.modulator]]` + targets | Modulator | one modulation edge per target |
| `[[group]]` | Group | member edges in; audio out → master |
| `[[group.effect]]` | Effect (bus) | audio chain after the group |
| `[[group.modulator]]` | Modulator | edges to member params, bus params, member modulators |
| pattern | Pattern | display only in track A |

- Ports: `in` / `out` (audio), `out` (modulation) on modulators, `Param(u32)`
  on plugin nodes, and, for cross-mod, on modulators `rate`, `attack`, `decay`, `sustain` and
  `release` (a setting of the modulator) and `depth { edge }` (the depth of one of its cables);
  as built, `app::graph::Port`, a cable to any of them since step 26.
- Edge kinds: `audio` and `member` are derived; `modulation` is user-editable,
  with `amount` = target depth (−1…1).
- Containment (`parent`) lets the outline derive the TUI's tree.
- Edge edits map to intents: drawing a cable = `AddModEdge` →
  `AddModTarget`; deleting one = `RemoveEdge` → `RemoveModTarget`; moving an
  end = remove + add.

*As built (step 8, `src/app/graph.rs`):* `SessionModel::graph()` gives a `Graph
{ nodes, edges }`.
- **Nodes** are in the order groups (each followed by its bus effects and
  modulators), lanes (each followed by its effects and modulators), the master.
  A `Node` is `{ id, parent, name }` plus a `kind`: `instrument` (plugin or null
  for an empty lane, range, transpose, volume), `effect` (plugin, mix; a lane
  effect and a bus effect are the same kind, the `parent` says which), `modulator`
  (its source: LFO waveform and rate, ADSR, or a MIDI control, bound or learning),
  `group` (volume, pan) and `master`. The master mix is a node of the graph with
  the reserved id `NodeId::MASTER` (`u32::MAX`) that the model does not have. A
  plugin carries its id, format, parameters (the value the user set, default, range,
  enum labels, unit, section, step count) and presets. **Patterns are not nodes:**
  they have no id in the model; the canvas gets them in phase 3.
- **Ports** are a flat enum: `audioIn`, `audioOut`, `modOut`, `param { index }`
  (the plugin's own index), `rate`, `attack`, `decay`, `sustain`, `release` (the
  fields of a modulator that another can drive) and `depth { edge }` (the depth
  of one of its edges). A node's ports follow from its kind and its plugin's
  parameters; they are not listed per node.
- **Edges** are `{ from, to }` endpoints (node and port) plus a `kind`: `audio` (a
  link of the audio path), `member` (a lane feeding its group) and `modulation {
  id, amount }`. Only modulation edges have ids: the others are derived and there
  is nothing to name them for. A group modulator's edges reach its members' chains
  and modulators and its own bus effects; one whose aim is gone (an effect, member
  or sibling that is not there) has no edge instead of a crash.
- **Differs from the sketch above:** the `Param(u32)` / `ModField(...)` ports are
  one flat enum, and the edge ids are only for modulation edges. `parent` for a
  lane is the group it is a member of, as the outline needs.

### 4.3 Parameter metadata

`ParameterInfo` is `index, name, min, max, default, labels`. Add, all optional:
`unit`, `group` (path), `step_count`, `flags` (hidden / read-only / stepped).
Builtins fill them in phase 0; the CLAP, VST3 and LV2 backends in phase 1 (VST3
units and step counts, CLAP module paths and flags, LV2 port groups and scale
points). Without them knobs read "0.37" instead of "1.2 kHz" and a big plugin's
parameters are one flat list.

*As built (step 8):* `ParameterInfo` has `unit`, `group` and `step_count` (0 for a
continuous parameter, n for n steps, an enum of n+1 labels has n); the built-ins
fill them in, and `ParamSlot` carries them from the loader. **`hidden` and
`read-only` flags are not there yet:** nothing produces them until the CLAP,
VST3 and LV2 backends fill the metadata in (phase 1). The filter's cutoff has no
unit on purpose: its value is a position on an exponential scale, not a
frequency, so "0.37 Hz" would lie.

**Open:** value → display text needs the plugin instance, which the audio thread
owns. Options: a main-thread shadow instance (which also replaces the
preset-mirror hack), static formatting tables for builtins, or number + unit for
everything else. Decide in phase 1.

### 4.4 Layout persistence

An optional `ui` inline table on each node, ignored by the engine and the TUI
and preserved on save:

```toml
[[instrument]]
plugin = "builtin:osc"
ui = { x = 80, y = 120 }

[[instrument.effect]]
plugin = "builtin:filter"
ui = { x = 320, y = 120 }
pinned = ["cutoff", "resonance"]

[[instrument.modulator]]
type = "lfo"
ui = { x = 320, y = 20 }
```

The same on `[[group]]`, `[[group.effect]]` and `[[group.modulator]]`; the master
node's position goes in a top-level `[ui]` table. `pinned` lists the parameters
shown as knobs on a plugin's node (plugins can have hundreds; the panel lists all of them); it is a key of the plugin's table
beside `ui` (step 30, below), and a cable's target is shown while the cable is there, as a cable and not as a pin.

**As built (step 25).** The positions are built; `pinned` came with step 30, below. A node's table has `ui = { x, y }`
for each kind of node (a lane, an effect, a modulator, a group, a bus effect, a group's modulator) and the master's is
`master = { x, y }` in the top-level `[ui]`; `CLAUDE.md` ("Node positions") has the format as a person reads it. The numbers are
pixels on the page's canvas, finite, whole (written without a point) or with decimals (as typed: an f32 widened to f64 would
print `12.300000190734863`); a `ui` that is not a pair of numbers is ignored with a log line (a layout is not worth refusing a
session for), while `nan` and `inf` are refused like every number. In the model the places are a side table, `SessionModel.ui`,
by `NodeId` (the master's under `NodeId::MASTER`): `app::load` fills it (a modulator that an older session nested under an effect
keeps its place when it migrates into the lane), `app::save::to_save` and `session::to_toml` write it, and the place of a node
that is taken out stays in the table (ids are not reused but for what undo gives back, so nothing else reads it) and is not written. The graph has them as
`Node.ui`, left out of the JSON of a node that has none, so that a session with no places is the JSON it was. The TUI shows
nothing of them and keeps them through a load, an edit and a save. A node with no place is where the page's layout puts it, and
the layout of the others does not leave room for one that was moved: a node dragged away leaves its space to whatever comes.

**As built (step 30).** `pinned` is a key of the plugin's own table, beside `ui` and not inside it as this section first said: on the
lane's `[[instrument]]` (its instrument), on an `[[instrument.effect]]` and on a `[[group.effect]]`, a list of the parameters' names in the
order they are shown (D33 says why it is not in `ui`). It is read leniently, as a place is: what is not a list is left out with a log line,
and so is any entry that is not a name; a name the plugin has no parameter of, a separator and a name twice are no pins (each is logged, the
next save leaves them out) and a name that two parameters share is the first of them. In the model it is `PluginSlot::pinned`, the plugin's
own parameter indices, which is where cables and ports name a parameter too (`pin_names` resolves a file's names, `pinned_names` goes back), so
it goes where its plugin goes: the effect that is moved takes its pins along, and the instrument that is replaced starts with none, with nothing
to remember. The graph has it as `Plugin.pinned`. At most 64 can be pinned to a node. A session with nothing pinned is written as it always was,
and the TUI shows nothing of the pins and keeps them through a load, an edit and a save. A cable's target is not pinned by the cable: it is
shown for as long as the cable is there (§9.3), since a pin that came and went with a cable would have been in the session file.

## 5. Intents and events

`Intent` is everything a front-end can ask for, addressed by `NodeId`. The TUI
resolves cursor → ids and then calls `apply`.

- Params and gains: `SetParam`, `SetGain { Volume | Mix | GroupVolume |
  GroupPan }`, `LoadPreset` (built in step 6; the rest of the list is not). (`LoadPreset { node, preset }` takes a
  preset's `id` and the node of a lane's instrument or of an effect in a lane's chain. The plugin's parameters take the
  preset's values, which the event carries with its name (`PresetLoaded`), and the cables that drive them are centred
  on those (step 21: until then the audio thread put the old values back at the next buffer, f64). Refused, with
  nothing changed, for a node that has no plugin, a lane with no instrument, a preset the plugin does not have, and a
  bus effect (`a bus effect has no presets to load`: the audio thread's command addresses a lane's slot).)
- Structure: `AddInstrument`, `ReplaceInstrument`, `AddEffect { parent, plugin }`,
  `RemoveNode`, `MoveEffect`, `SetRange`, `SetLaneGroup` (planned as `AssignGroup`: the intent says what
  changes, the group a lane is in, and none takes it out), `AddGroup`,
  `RenameGroup`. (`AddEffect` is built, step 19: `parent` is the node of a lane (an empty lane has
  one too) or of a group, and `plugin` the `id` of an entry of the catalog the server told (§7). The
  server loads it, puts it at the end of the chain (a lane's, or a group's bus) in a node of its own, and
  starts it at the mix that suits it. It is a change of structure, told by a snapshot like the others
  (D20); the node it made is the last effect of the parent that the client did not have before
  (`Editor.addEffect`). Refused, with nothing changed and no revision, for a parent that is neither a
  lane nor a group, a plugin the catalog does not name (a path of the client's own is not loaded, however
  it is spelled), an instrument, a plugin that does not load, and one whose outputs are not the chain's
  channels.) (`AddInstrument { plugin }` and `ReplaceInstrument { node, plugin }` are built, step 20: a lane at the
  end of the session with an instrument of the catalog in it, and the instrument of the lane `node` (an empty lane
  is filled) replaced; the cables that drove the old plugin's parameters are taken out. Refused, with nothing
  changed, for a node that is no lane (checked first), a plugin the catalog does not name, an effect, one that does
  not load, and one with no audio output; told by a snapshot like the others; `Editor.addInstrument` resolves with
  the node of the lane.) (`SetRange { node, range }` is built, step 23: the notes a lane takes, both ends included, or
  every note for `null`; `node` is a lane's (an empty lane's too). Refused, with nothing changed, for a node that is no
  lane, for a note above 127 and for a low note above the high one ("the range goes from F#6 down to E2: its low note has
  to be the lower"); answered by `RangeSet`, an event and not a snapshot, since no node or cable comes or goes. The TUI's
  `R` popup asks it, so that what it sets reaches the browsers.) (`MoveEffect { node, index }` is built, step 28: `node` is
  an effect of a lane's chain or of a group's bus, and `index` the place it is to have among that chain's effects, from 0
  (the first, nearest the instrument or the group) to one less than they are. The effects between its old place and the new
  move one place the other way, the audio path goes through them in the new order, and every cable stays on the plugin it
  drove (`Controller::move_effect` / `move_bus_effect` change the model, which re-numbers the slots of the cables, and tell
  the audio thread, together). Refused, with nothing changed and no revision, in this order, for a node that is not there, a
  node that is no effect (`only an effect can be moved`: a lane, a group, a modulator), a place the chain has not (`the chain has
  no place 3`) and the place the effect is at (`that effect is at that place already`). A change of structure, told by a
  snapshot; the TUI's Shift+Up and Shift+Down are the same two controller methods, one place at a time.)
  (`AddGroup { members }`, `SetLaneGroup { node, group }` and `RenameGroup { node, name }` are built, step 31. `AddGroup` makes a
  group at the end of the session, with no effects and no modulators, volume 1 and pan 0, and puts the lanes of `members` in it in the
  same intent, each as `SetLaneGroup` would (the members are all looked at before anything is made, so a refusal leaves no group
  behind); none makes an empty group. `SetLaneGroup` puts the lane `node` (an empty lane's too) in the group `group`, or, for `null`,
  takes it out of its group; a lane is in one group at most, so one that is in another leaves it. What a lane leaves is no longer
  driven by it: the cables of the group it leaves, to the lane's plugins and to its modulators, are taken out (with the cables that
  drive their depths, as `RemoveEdge` does it) and what they drove has the value that was set again, and the cables of the group to
  the members after the lane are told the places they have now (`member_moves`) and keep driving the same plugins. A lane put in the
  group it is in is as it was. Refused, with nothing changed and no revision, for a node that is no lane (`only a lane can be put in a
  group`) and a group that is no group (`that node is not a group`). Both change the structure and are told by a snapshot (D20); the
  group `AddGroup` made is the last group the client did not have before (`Editor.addGroup` resolves with its node). `RenameGroup`
  names a group: the name is trimmed, at most `MAX_NAME` (64) characters and none a control character (`NAME_LIMIT` in `constants.ts`),
  and a name of nothing gives the group the name it has by default, "Group" and its place, counting from one. Refused for a node
  that is no group (`only a group has a name to change`), a name that is too long (`a name is at most 64 characters`) and one with
  a control character (`a name has no control characters`). It is answered by `GroupRenamed { node, name }`, an event and not a snapshot,
  and the name it carries is the one the group has now, so that a client never works out a default name: a group that is called
  nothing is called by its place, which a client that has the event does not need to know. The TUI's `g` and `R` ask them, so that what
  they do reaches the browsers.)
- Modulation: `AddModulator`, `SetModulatorField`, `AddModEdge`, `RemoveEdge`,
  `SetEdgeDepth`, `LearnMidi`. (`SetModulatorField` and `SetEdgeDepth` are built, step
  14: an LFO's rate and waveform and an envelope's four stages, in a lane's modulator or
  a group's, clamped to `ModulatorField::range` with a waveform rounded to its index
  (sine, triangle, saw, square), refused with nothing changed for a node that is no
  modulator, a setting its kind lacks or a value that is not a number; and the depth of a
  cable, -1 to 1, with `EdgeNotFound` for one that is gone. The events are
  `ModulatorFieldSet` and `EdgeDepthSet`, carrying the value after clamping. Step 15 built the
  four that change the structure: `RemoveEdge { edge }` (a cable, and with it the cables that
  drive its depth, and theirs in turn: left pointing at a cable that is gone they would
  drive whichever one is put there next), `AddModulator { parent, kind }` (an LFO, an
  envelope or an armed MIDI control at the end of the rack of a lane, whose node is its
  instrument's, or of a group; any other parent is refused), `AddModEdge { modulator, to }`
  (a cable at depth 0.5, `to` a node and a port of it: a plugin's `param`, where the controller
  decides where it may end, D21; or, since step 26, a modulator's `rate`, `attack`, `decay`,
  `sustain` or `release`, or the `depth` of one of that modulator's cables, where it decides by
  `SessionModel::modulators_in_reach` and `field_targets` (D29) and refuses, with nothing changed
  and in this order, a node that is not there, the modulator itself (`a modulator cannot drive its
  own settings`), a node that is no modulator, one out of reach, a setting its kind lacks (`that
  modulator has no rate`; a depth that is not one of that node's cables is `EdgeNotFound`) and one
  the modulator drives already; a cable to a setting has no telemetry slot of its own, since a
  target that drives a modulator's own field has none) and `RemoveNode { node }` (a modulator, with its cables and the cables
  of the others that drove its settings; since step 19 also an effect or a bus effect, with the
  cables that drive its parameters, a lane's and a group's, and what drove their depths, the audio
  path closing up round it; since step 31 also a lane, with its instrument, its effects, its modulators and the cables of its
  group to it, and a group, with its bus effects, its modulators and every cable of it, its members staying in no group, with what its
  cables drove in them given back (`Controller::remove_lane` / `remove_group`). Taking one out loses an instrument and all it has, and
  there is no undo yet, so a front-end asks a person first (the page does, D34); the controller refuses the last lane of a session,
  which keeps one: `a session keeps its last lane`). A refusal changes nothing and is no revision. `LearnMidi` is the
  rest of the list that is not. Step 37 built `Modulate { to, kind, depth }`, a modulator and a cable to a port as one intent, for what the
  TUI's `m` and `l` and the page's menu on a knob do: the modulator is made of `kind` (a MIDI control armed to learn) at the end of the rack that
  reaches `to`, the lane's for a lane's instrument or an effect of its chain, the group's for a bus effect and a modulator's own rack for its
  rate, a stage or the depth of one of its cables, where the new one is a sibling that drives it; the cable starts at half depth, or at `depth`
  (clamped as a depth that is set is). It is refused, with nothing changed and no id used, for a depth that is not a number, a node that is not
  there, a group (`a modulator drives a parameter of a plugin or a setting of a modulator, and that is neither`) and everything that a cable to
  that port is refused for (`AddModEdge` and it plan the cable with the same functions, `param_cable` and `field_cable`; the modulator it would be
  drawn from is not there yet, and takes its place at the end of its rack). It is one step of the history, `modulate cutoff` or `learn cutoff`.)
- Patterns: `RecordPattern`, `ClearPattern`, `SetPatternField`.
- Globals: `SetBpm`, `SetScale`, `SetLocked`, `Save`, `SaveAs`, `Undo`,
  `Redo`. (`SetScale { scale }` and `SetLocked { locked }` are built, step 24 (the plan said
  `SetPianoMode`; a boolean says what it is): the Piano tab's scale, as the session file writes it
  (`C major`, `F# dorian`: the roots may be sharps or flats, the names of the kinds short or long, in
  any case), and whether the piano is locked to it, so that a note outside the scale is dropped where it
  is sent. `SetScale` is refused, with nothing changed, for text that is not a scale ("Cé" is not a
  scale: a root and a kind of scale, like C major or F# dorian"); the events are `ScaleSet`, which has the
  scale as the file writes it, its root, its kind and the pitch classes of its notes, so that a client
  draws it knowing no scale, and `LockedSet`; `PianoMeta` in the snapshot has the same, and the reducer of
  the page and `Snapshot::change` take the two events. The TUI's `k` and `l` ask them (choosing the
  scale that is set is no change there, as before). Both are saved with the session. `Save` is built, step 18: it writes the session to its own file, as the TUI's
  `Ctrl+S` does, and nobody names a file; it is answered by a revision and the event
  `Saved { file }`, which leaves the session with nothing unsaved; a session with no file, or
  one that cannot be written, is refused with the code `io`, which is no revision and changes
  nothing. `SaveAs` waits for the one validated file name, §8. `Undo` and `Redo` are built, step 32: the controller keeps a history
  and these two carry out what its last step says, see "Undo / redo" below and D35.)
- Pins: `SetPinned { node, params }` (not undoable; marks the session dirty). Built, step 30: `node` is a lane's node (its instrument), an
  effect or a bus effect; `params` the plugin's own indices (`Param.index`) in the order the front-end shows them, replacing the list there
  was, and none gives the node back to the front-end's own choice (the first eight). Refused, with nothing changed and no revision used, in
  this order: a node that is not there (`NotFound`; the master is no node of the model, for the controller), a node that has no plugin
  (`that node has no plugin`, `that instrument has no plugin` for an empty lane), more than 64 (`at most 64 parameters can be pinned to a
  node`: `MAX_PINNED`, `PINNED_LIMIT` in `constants.ts`), then each parameter in order: one the plugin has not (`the plugin has no parameter
  N`), a separator (`X is a separator, not a control`: the model has them, the graph has none) and one listed twice (`parameter N is listed
  twice`). The answer is the event `PinnedSet { node, params }`, which `Snapshot::apply` and the page's reducer apply (the plugin's own
  `pinned`), and which the TUI leaves alone. The graph has the list in every plugin, `Plugin.pinned` (empty for none).
- Layout: `SetNodeUi` (not undoable; marks the session dirty). (`SetNodeUi { node, ui }` is built, step 25: `ui` is
  `{ x, y }`, in the pixels of the canvas, or `null` to give the node back to the page's layout; `node` is any node of the
  graph, the master (`NodeId::MASTER`) too, which the controller finds though it is not a node of the model. The controller
  keeps a place to a tenth of a pixel (half away from zero), within 100 000 either way, and never `-0` (`NodeUi::tidy`; the
  limit and the tenth are in `constants.ts`, `NODE_UI_LIMIT` and `NODE_UI_PARTS`, which the mock engine reads). Refused, with
  nothing changed, for a node that is not there (`NotFound`, for `null` too) and for numbers that are not numbers (`Invalid`:
  "(NaN, 1) is not a place"; over a WebSocket they are `null` and do not decode at all). Answered by `NodeUiSet { node, ui }`, an
  event and not a snapshot, with the place as it is kept. The engine hears nothing, and the TUI ignores it. The page asks it
  as `Editor.moveNode`; `Editor.resetLayout` gives every node that has a place back to the layout, one request after the other.)

`apply` returns `Applied { rev, events, restructured }` or an `IntentError`
(`NotFound`, `EdgeNotFound`, `Invalid` and `Io`; `Busy` waits for the plugins that load off the
audio thread). (§5 first sketched an `undo` on `Applied`, the inverse intents of the change: it is not
on the answer, the controller keeps the history, D35.) Every request carries a client `req_id` for the
ack/err reply. Note on/off bypasses the controller (§3.3).

Events carry a monotonically increasing `rev` (in the code it sits on `Applied`,
one per intent; on the wire (step 8) `StampedEvent` is the revision and the event
in one object, `Applied::stamped()` makes them): `ParamSet`, `NodeAdded`,
`NodeUpdated`, `NodeRemoved`, `EdgeAdded`, `EdgeUpdated`, `EdgeRemoved`,
`OrderChanged`, `Meta` (dirty, path, BPM, piano), `PluginLoading`,
`PluginLoaded`, `PluginFailed`, `CatalogBatch`, `PatternRecorded`,
`MidiLearned`, `Devices`, `Resync`. Structural events carry whole replacement
nodes so client reducers stay trivial; value events are tiny. A client that sees
a gap in `rev` refetches a snapshot. (**As built, step 13:** the structural events
are not built, and until they are the controller's `resync` stands in for them: a
change no intent made is told by a snapshot at a revision of its own, §3.4. **Step 15**
tells the intents that change the structure the same way (D20): `Applied` has `restructured`,
`apply` calls `resync` at once, and the subscribers are given the new structure whole, at
the revision after the intent's, before the answer to the client that asked. The controller's
projection of the graph is the only one: a client that derived the audio path, the order of
the nodes or the layout of the telemetry from events would be a second. What a client that
added a node needs to know, the id of the node, is in that snapshot: it is the last
modulator of the parent that it did not have before the request (`Editor.addModulator`).)

**Undo / redo** (steps 32 to 34, D35 to D37). The controller keeps a history of what a person did (`src/app/history.rs`): for each
intent that has an inverse, the intents that put back what it overwrites (read from the model before it is applied) and the intents
that do it again, in the words of a button for it (`set cutoff`, `move a node`, `rename a group`, `add Reverb`, `draw a cable`). The intents
that have one are `SetParam`, `SetGain`, `SetModulatorField`, `SetEdgeDepth`, `SetRange`, `SetNodeUi`, `SetPinned`, `RenameGroup`, `SetScale`,
`SetLocked` and `MoveEffect` (step 32), and what is added (step 33): `AddEffect`, `AddInstrument`, `AddModulator`, `AddModEdge`, `Modulate` (step 37), `AddGroup` of lanes
that are in no group, and `SetLaneGroup` of a lane that its old group does not drive. What is added is taken out by `RemoveNode` and `RemoveEdge`
and put back by redo **under the ids it had** (D36): the controller writes down the ids that an intent hands out (`Made`), the step keeps them
(`redo_made`), and `carry_out_all` says them to the allocator before the intents are carried out, so that a step from after the add (a gain set on the
effect that was added) finds the effect when it is redone; ids that are in use are refused, and ids the intents did not use are dropped. A change of one thing (the same parameter, gain, setting or cable, `Key`) a second or less after the
last of it is one step, so that a drag is one (`SAME_STEP`; a pause of more than a second makes a step of its own, and exactly a
second does not), 200 steps are kept (`MAX_STEPS`; the oldest go), a change after an undo ends what could be redone, an intent that is
refused leaves no step, and `Save` is no change. `SetNodeUi`, `SetRange`, `SetPinned`, `RenameGroup`, `SetScale`, `SetLocked` and
`MoveEffect` have no key, and each is a step of its own. `Undo` and `Redo` are intents like the others: a step is applied
intent by intent, each a revision of its own, answered with the events of all of them (and `restructured` if one was), and neither
is a step; with nothing to undo (`there is nothing to undo`, `... to redo`) it is refused and is no revision; a step whose node has
gone (a front-end took it out without an intent) is dropped, with the reason, and what it did before it failed stays. What has no
inverse yet, a group made of a lane that is in another group (the lane leaves
it and the cables of that group to it go: nothing brings them back), a lane put in a group or out of one whose modulators drive it, a `RemoveEdge` of cables that drive each
other's depths (D37) and the TUI's `Controller::set_instrument` and `clear_instrument`, **empties the history**: a step from before it would be put back over what it changed.
**What is taken out** (step 34, D37) is put back by undo from what was taken, exactly as it was: the controller writes down what an operation takes out (`Journal`) and the step puts
it back in the reverse order, under its ids, in its place and with its slots of the telemetry; redo takes it out again. Built for `RemoveEdge` (a cable, and the cables that drive its
depth and theirs, each back at the place it had) and for `RemoveNode` of a modulator (the cables that address it, and the modulator with its own cables and what each was centred
on, back at its place in its rack and under its ids, the cables that addressed it after it) and for `RemoveNode` of an effect, a lane's or a bus's (the cables that drive it, and the effect with
**the very plugin that was in the chain**: the controller makes a channel for it and the audio thread hands it there, so that undoing it is at once and does not load the plugin again, D8; a plugin that
did not come in time, or that the history let go of (it keeps the plugins of the newest 8 only), is loaded again with its preset and the values the user set; it goes back at its place in its chain and at its mix)
and for `RemoveNode` of a lane (the cables of its group to it, and the lane handed back **whole** by the audio thread, with its plugins, modulators, pattern and meter: it goes back at its place among the lanes and
in its group, the cables of its group to the later members told their ordinals; one that did not come in time refuses the undo, since a lane cannot be built again from the model, and one that gave up its plugins
to the history's limit has them loaded again into it) and for `RemoveNode` of a group (the cables of its modulators, and the group handed back **whole** by the audio thread, with its bus effects, modulators, meter, volume and pan:
it goes back at its place among the groups with the lanes that were its members, and then its cables go back; one that did not come in time refuses the undo, and one that gave up its bus effects to the history's limit has them loaded again into it).
**What is replaced** (step 35) is put back from what it replaced: an instrument that another replaces (`ReplaceInstrument`: the very plugin, if the audio thread handed it back and the history kept it, else loaded again from
its id, its preset and the user's values; the lane's note remap and pitch bend range, which go with the instrument; the cables that drove it, put back after it; a lane that was empty is emptied again) and a preset that is loaded
(`LoadPreset`: the audio thread loads the preset the plugin had again, and each value that the load overwrote, with what it is compared with when the file is saved, is given back). Redo replaces the instrument again, from the
catalog, and loads the preset again.
A structural edit that no intent asks for (the TUI's swap of two lanes, a
call of a method of the controller that takes something out, moves an effect or puts a lane in a group, with no intent being carried out) forgets the history
(`forget_the_history_unless_an_intent_asks`): a step that took something out puts it back by the places it had.
What Undo and Redo
would do is told to the clients as the message `History { undo, redo }` (§7), which is not an event.

**Continuous controls.** Clients send at most one `SetParam` per frame per
parameter; when several are queued the owner applies the latest. The engine's
command channel is bounded and `send` blocks, so the owner must never flood it.

## 6. Telemetry

Telemetry publishes engine truth: modulator outputs (once per buffer), effective
parameter values (the `clamp(base + Σ offsets)` result written in
`apply_modulators_to_chain` and the group equivalents), and per-lane / group /
master peak and clip. A scope ring can follow later.

- **Mechanism.** `telemetry::Telemetry` is a preallocated array of 4096
  `AtomicU32` cells (f32 bits, relaxed ordering); one cell is a *slot*. The main
  thread's `SlotAllocator` (it lives in `SessionModel`) hands slots out and never
  takes one back, so index shifts never re-key a slot; slots 0 and 1 are the
  master meter (`telemetry::MASTER`). A slot reaches the engine in the target's
  own struct (`ModTarget.effective`) or in a command sent right after the one that
  creates the item (`SetModulatorOutput`, `SetLaneMeter`, `SetGroupMeter`), which
  leaves the creating commands as they were. The audio thread only does relaxed
  stores and one `fetch_add` per clipped buffer: no allocation, no locks (a
  counting allocator in the tests checks that for lanes, groups, patterns, remapped
  notes and the built-in plugins). If slots run out, that item has no telemetry
  (logged); audio is unaffected.
- **What is published.**
  - *Modulator output* (`ModulatorSlot.output`): what the modulator put out this
    buffer, as the engine computed it: an LFO −1…1, an envelope 0…1, a MIDI
    control 0…1 (pitch bend: 0.5 is centre).
  - *Effective value* (`ModTargetSlot.effective`): what the target's parameter was
    set to this buffer, i.e. its base value plus every modulator's offset on it,
    clamped. It is a slot per target but the same number for every target on one
    parameter. A target that drives a modulator's own field (cross-mod) has none.
  - *Meters* (`MeterSlots { peak, clips }`): master, every lane
    (`InstrumentNode.meter`, measured after the lane's volume and effects and
    before it joins its group) and every group (`GroupNode.meter`, after its
    volume, bus effects and pan, before the master). The peak is held and falls
    20 dB per half second, so a reader at 30 Hz sees at most about 1.3 dB under
    the loudest sample since its last look. `clips` is a count that goes up by one
    for each buffer with a sample beyond full scale (|s| > 1.0) and is never
    reset, so any number of readers can watch it. The TUI's `CLIP` light is the
    first reader (`src/tui/clip.rs`).
- **Layout.** A `TelemetryLayout { rev, meters, modulation }` tells clients what
  each slot means: two lists, one per channel group, of `{ slot, node, port, kind
  }` with `kind` one of `peak`, `clips`, `modOutput`, `effective`. A frame carries
  the values of one list in its order. `rev` is a fingerprint of the entries, so it
  changes exactly when they do, whoever changed them (the TUI edits the model
  without telling anyone, so a counter would need every edit to bump it).
  `SessionModel::telemetry_layout()` builds it from the slots the model holds
  (`InstrumentNode.meter`, `GroupNode.meter`, `ModulatorSlot.output`,
  `ModTargetSlot.effective`; `check_ids` verifies them). The meters are at the
  master (its `audioIn`), each group's and each lane's `audioOut`; a modulator's
  output is at its `modOut`; a parameter's effective value is at the plugin node's
  `param` port, named once per parameter, by the lane's slot rather than the
  group's (the lane's modulator is applied after the group's and wins). An item
  without a slot or without an aim has no entry.
- **Wire.** WebSocket binary frames: `u8 kind | u8 flags | u16 n |
  u32 layout_rev | n × f32`, where value *i* belongs to layout entry *i*. About
  30 Hz per subscribed channel group (`mod`, `meters`); nothing is sent when
  nobody subscribes. Held notes go as a 16-byte bitset, on change (as planned; as built they
  are a third channel of 128 values, see step 24 below).
  **As built (step 10):** the channels are `meters` (kind 0) and `modulation`
  (kind 1), little endian, `flags` 0. `web::pusher` makes a frame per wanted
  channel every `Config::telemetry_period` (33 ms) from `Shared::layout()` and the
  atomics, and the hub queues it for the clients that asked
  (`{"type":"telemetry","channels":["meters"]}`, replacing what they asked for
  before; `[]` stops). A clip count goes as a count in an `f32` (exact up to 2^24
  buffers). A frame names the `rev` of the layout it follows; the layout is in the
  snapshot, and a `layout` message tells the subscribed clients when it changes
  (a frame of a revision the client has not been told of is to be ignored).
  **As built (step 24):** the held notes are a third channel, `held` (kind 2): 128 `f32` values, one for each
  note at the place of its number, 1 for a note that is held (by whatever source: a MIDI keyboard, the
  computer's keys of the TUI or of a page, the pointer on a page's piano) and 0 for the rest,
  `layout_rev` 0 (it follows no layout, and a client takes it at any time), every tick like the other channels
  (512 bytes at 30 Hz for each client that asks). They are read from the same `HeldNotes` bits the Piano tab
  reads (`NoteInput::held`, which the server hands the pusher). The bitset on change that this section planned
  is 16 bytes and fewer of them; it was not worth a second frame format and the bookkeeping of what each client
  has been told (D27). A client forgets which notes are held when its connection is lost (the server ends the
  notes of a connection that goes, and the first frame after it is back says what is held then).
- **Visual vocabulary.** Knob = base value; ring = modulation extent (depth ×
  range); dot = effective value.

## 7. Wire protocol

- `GET /ws` upgrades to one WebSocket per client. Text frames are JSON
  (`Request { req_id, intent }`, `Response { req_id, ok | err }`,
  `Event { rev, … }`, `Snapshot`); binary frames are telemetry. The server
  speaks first with `Hello { protocol, rev, features }`; a snapshot follows
  `Subscribe`.
- The Rust types in `app` (`Intent`, `Event`, `Snapshot`, …) derive `serde`
  and, under `web`, `ts-rs`. The TypeScript types are generated into
  `web/src/protocol/generated/` and committed; CI fails if they are stale.
- **JSON conventions (step 8).** Names are camel case. An enum is an object tagged
  by `type` (intents, events, ports, modulator sources, MIDI controls) or `kind`
  (a node's, an edge's), the other fields beside the tag; an enum without
  payloads is a string (`"groupPan"`, `"saw"`). A `NodeId` / `EdgeId` is a bare
  number. A struct that wraps an enum flattens it, so a node is `{ "id", "parent",
  "name", "kind": "effect", "plugin", "mix" }` and an event is `{ "rev", "type":
  "paramSet", ... }`. Absent values are `null`, not left out. Floats are `f32` in
  the engine and as short as they can be on the wire.
- **The roots** of the generated types, in `app::protocol`: `Intent`, `Event`,
  `StampedEvent`, `Graph`, `TelemetryLayout`, `Snapshot` (and what they use).
  `ClientMessage` and `ServerMessage` (below) come with the server. Generation is
  configured in code (`export_all`, large integers as `number`), not through
  `TS_RS_*` variables and not with `#[ts(export)]`, so the output does not depend
  on who runs it and a plain `cargo test --features web` writes nothing. The
  test `app::protocol::typescript` compares a fresh generation with the committed
  directory (a changed, a missing and an extra file each fail it);
  `TANG_UPDATE_TYPES=1 cargo test --features web typescript` rewrites it. The CI
  job `typescript` runs it without the plugin formats.
- The protocol is versioned (`protocol` integer in `Hello`); the client refuses
  a mismatch with a clear message.
- HTTP serves only the SPA and `/ws`; there is no REST API.
- **The messages (step 10)**, `app::protocol`, JSON text frames, tagged by `type`
  (camel case):
  - client → server: `auth { token }` (the first message when `Hello.needsToken`),
    `subscribe`, `telemetry { channels }`, `request { reqId, intent }`,
    `noteOn { note, velocity }`, `noteOff { note }`.
  - server → client: `hello { protocol, rev, features, needsToken }` (first),
    `snapshot { snapshot }` (the answer to `subscribe`, and, since step 13, also sent
    unasked to the subscribed when the session changed in a way no event describes: the
    client replaces its copy, the layout included, and carries on from the revision in
    it), `event { event }` (a `StampedEvent`), `layout { layout }`, `catalog { rev,
    scanning, entries }` (step 19: the plugins that can be added; see below),
    `history { undo, redo }` (step 32: what Undo and Redo would do; see below),
    `ack { reqId, rev }`, `rejected { reqId, error }`, `closing { error }` (the last thing
    before the server closes), with `error = { code, message }`, `code` one of `notFound`,
    `invalid`, `io`, `badRequest`, `unauthorized`, `rateLimited`, `overloaded`.
  - **The catalog (step 19).** The plugins installed on this machine are scanned on a
    background thread (the TUI starts it when it starts; `tang serve` does, since it has no TUI)
    and arrive by format: the built-ins at once, VST3, which loads each plugin, last. A
    subscribed client is sent `catalog` right after its snapshot, as far as the scan has got, and
    again whenever the scan finds more or ends: each message is the whole catalog, so a client
    replaces what it has (`rev` goes up with each batch found, `scanning` says whether more may
    come). `entries` are `{ id, name, format, isInstrument, params, presets }`: what a person
    chooses by, and never where the plugin is installed (`PluginInfo::path`, §8): a client names a
    plugin by its `id` and the program loads only what is in its own catalog. The catalog is not
    part of the session: it is no revision and leaves the session as it was.
  - **The history (step 32, D35).** What Undo and Redo would do is told as `history { undo, redo }`: the words of
    a button for what the last step to undo and the last to redo are (`"set cutoff"`), or `null` for what there is
    none of. A subscribed client is sent it right after its snapshot and its catalog, and again whenever it is not
    what the connection was last told, after the events of the request that changed it and before the answer to
    that request (`web::owner`: `tell_history`, after `apply`, after what a front-end of the program's own did
    (`refresh`), and on `subscribe`), so that a page which sees `ack` has seen what it can do now. It is the
    history of the program, not of the connection: what one page set can be put back from another, and each is told.
    It is no event: it is no change of the session, and a second event at the revision of an intent would be taken
    by a client for a repeat (`applyEvent`). A client that loses the connection forgets it and is told again by the
    server it comes back to. `undo` and `redo` are requests with no fields (`{ "type": "undo" }`).
  - **Order.** The owner makes the cut between a snapshot and the events after it:
    `subscribe` is answered by marking the client subscribed and queueing the
    snapshot in one step, and events are only made on the owner's thread, so a client
    gets the snapshot of revision *n* and then exactly the events of the revisions
    after *n*. A request is answered after the events it made, so the requester
    hears `event` and then `ack`; the others hear the event.
  - **Close codes** of the WebSocket: 1001 the server stops, 1008 the connection
    broke a rule (the reason is in the `closing` message before it), 1013 the client
    fell behind.
  - **Notes** go straight to `NoteInput`, not through the controller: the Piano
    tab's scale filter (Locked mode), the held-notes bits and the MIDI channel to the
    audio thread, like a hardware key. The connection remembers which notes it holds and ends them when it
    ends, however it ends.

## 8. Security

The server loads native code and writes files, and any web page can open a
WebSocket to localhost.

- Bind loopback by default. A non-loopback bind needs `--web-bind` plus a token,
  printed as `http://host:port/#token=…` (the fragment never reaches logs or
  `Referer`) and sent in the first WebSocket message; nothing is processed
  before it validates.
- Validate `Origin` and `Host` on every upgrade (cross-site WebSocket hijacking
  and DNS rebinding), loopback included.
- Plugins load only by catalog id; clients never supply paths. Session
  switching, when it exists, is limited to the sessions directory.
- Save As takes a plain file name only: no separators, no `..`, not absolute;
  `.toml` appended. The TUI's current `dir.join(name)` is not safe to reuse (an
  absolute `name` replaces `dir`), so one validated function serves both
  front-ends.
- Limits: maximum message size, a per-connection intent rate cap, bounded
  outgoing queues.

**As built (step 10).**

- The guard (`web::server`) applies `Policy::check` to every request, the page
  included, on loopback as well. On loopback `Host` has to be `localhost`,
  `127.0.0.1` or `[::1]` on the server's port, and an `Origin`, if there is one, the
  server's own page (or an origin given with `--web-origin`); a request without an
  `Origin` does not come from a web page and is let through. Beyond loopback the
  names cannot be listed, so `Host` is free and an `Origin` has to be the `Host` it
  came to. A header that is there but not text, or comes twice, is refused rather
  than read as absent. Refusals are `403`, and do not repeat what they were sent.
- Every answer carries `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
  `Referrer-Policy: no-referrer` and `Cache-Control: no-store`; the page has a CSP
  that allows nothing but its own inline style (to be widened when the real
  interface is served).
- The token (`--web-bind` beyond loopback: made at startup, 128 random bits as hex,
  in the fragment of the printed address) is compared in constant time, and has to
  be the first message; a connection that has not sent it within 10 s, or sends
  anything else first, is told (`unauthorized`) and closed.
- Limits: 64 KiB per message (the socket refuses a longer frame or message before
  it is read whole, and the connection checks again), a token bucket per connection
  (burst 300, 200 a second) that every request, note on, `subscribe` and `telemetry`
  message spends from (a request over the limit is rejected, a note on dropped, a
  `subscribe` or `telemetry` over it ends the connection, and a note off is never
  dropped), 32 connections, 256 frames per client queue, 1024 waiting requests, and
  ten seconds for a write.
- **Not checked:** the policy against a real browser (the tests use the same
  headers a browser sends but are not one), and TLS: there is none, so a token sent
  over a LAN is in the clear. Put a reverse proxy in front for anything but a
  trusted network.

## 9. Frontend

### 9.1 Stack

TypeScript (strict), React, Vite; `@xyflow/react` (React Flow) for the canvas;
Zustand for state; Radix primitives under our own design system (CSS variables,
tokens from the TUI palette: held red, root amber, scale teal, modulation
magenta, groups cyan); Vitest, Storybook, Playwright. Svelte 5 + Svelte Flow was
the alternative; Rust/WASM UI frameworks were ruled out (no comparable
node-editor, virtualization or accessibility ecosystem). If the canvas outgrows
React Flow, only that component is replaced: the model and protocol are
renderer-agnostic.

### 9.2 Layering

```text
web/src/
  protocol/generated/   ts-rs output (committed)
  client/               framework-agnostic: connection, store, reducers,
                        telemetry buffers, mock engine
  ui/                   design system: tokens, Knob, Slider, EnumStepper,
                        Meter, Node, Port, Cable, PianoKey
  app/                  React views: Outline, Canvas, Inspector, Piano, Toolbar
```

`client/` has no React dependency, so a UI experiment (a canvas variant, a mixer
view, a WebGL scope) is just a new view on the same store. A mock engine
implements the same interface for UI development, Storybook and tests without
audio hardware. Dev: `npm run dev` (Vite proxying `/ws` to `tang serve`) or the
mock engine. Build: `npm run build` → `web/dist`, embedded with `rust-embed`
under feature `web` (debug builds may serve from disk).

**As built (step 11).** `web/src/client/`: `reducers.ts`, `telemetry.ts`,
`connection.ts` (`Client`, `Transport`), `websocket.ts`, `mock.ts` (`MockServer`),
`fixtures.ts`; `web/src/protocol/index.ts` lists the generated types the app uses;
`web/src/ui/` has the tokens (`tokens.css`) and `ConnectionStatus`; `web/src/app/` has
`App`, `ClientProvider` (`useClient`, `useClientState`) and `engine.ts` (which server a
page talks to: its own, or the mock with `?engine=mock`). The store is vanilla zustand,
so `client/` has no React in it; React binds to it with `useStore`. The page's address
decides the socket (`ws:` or `wss:` on the page's own host) and a `#token=…` in it is
the token. `web/dist/` is cleaned by `scripts/clean-dist.mjs` instead of by Vite, to keep
its `.gitkeep`, which makes the folder exist in a checkout so that the program (which
embeds it) builds without Node.

**Serving it (`web::assets`).** `rust-embed` carries `web/dist`: in a release build the
files are in the binary (`cargo build --release --features web` after `npm run build`:
nothing warns if the interface was not built first, and the program then serves a
placeholder that says how), in a debug build they are read from the disk, so rebuilding
the interface needs no rebuild of the program. Only the interface's files are served
(`/` is `index.html`; no hidden file, no `..`, no listing, a 404 for the rest, no SPA
fallback until the app has routes). The page carries a content security policy (own
scripts and styles, images and fonts, a connection to its own server; no inline script
or style, no eval, no framing) and is looked at again each time; the files in `assets/`,
which Vite names by their content, are kept for ever.

**As built (step 12).** `client/` gained `frameloop.ts` (the page's one
`requestAnimationFrame`), `drive.ts` (the controls that are turned), `readers.ts`
(a reader of one telemetry slot), `modulation.ts` (what the engine's laws make of the
cables that end at a parameter), `selectors.ts` (a node, a parameter or a gain of the
session by id) and `stress.ts` (the big session of the performance test). `ui/` has
`Knob`, `Choice` (a native select: it stands where this section said `EnumStepper`),
`LiveBar`, `LiveTrace` (step 27), `LiveEnvelope` (step 29), `Meter` and `ClipLight`, each a function of its props that draws what changes
every frame by writing to the DOM (or to a canvas); what was listed here as `Slider`, `Node`, `Port` and
`Cable` is in `app/patch/` (`nodes.tsx`, `edges.tsx`), since it is made of React Flow;
`PianoKey` is not built. `app/` has `PatchView`, `controls.tsx` (the controls of a node,
bound to the session) and `toFlow.ts`, which is pure; the outline, the inspector, the piano
and the toolbar of §9.3 are not built. `ClientProvider` hands the views the client, the frame
loop and a `Drive`; `useClientSelection(select, equal)` is for a part of the state that is
an object made anew. `src/test-setup.ts` makes up what jsdom lacks for React Flow (sizes
and a resize observer), so that the canvas renders in the ordinary tests.

### 9.3 Patch view (track A)

Layout: outline (the TUI's chain tree) | canvas | inspector (the TUI's parameter
pane), with the piano along the bottom.

```text
┌─────────────┬───────────────────────────────────────┬──────────────┐
│ Outline     │ Patch canvas                          │ Inspector    │
│             │  [LFO]~~~~┐                           │              │
│             │  [ENV]~~┐ │                           │              │
│             │         ▼ ▼                           │              │
│             │  [Osc]──[Filter]──[Reverb]──▶ out     │              │
├─────────────┴───────────────────────────────────────┴──────────────┤
│ Piano: keys, scale strip, held notes / chord                       │
└────────────────────────────────────────────────────────────────────┘
```

- Each lane is a strip: instrument → serial effects → group / master. Groups
  are containers (bus nodes) that member strips feed into.
- Modulators are nodes with free-form cables to knob ports. Audio cables are
  drawn but derived: you reorder by moving a node along the strip.
- Nodes show their pinned parameters as knobs (the first eight until something is pinned); everything else is in the
  panel. A cable's target is shown while the cable is there.
- The limits are the engine's (§2): serial audio path, block-rate lane-global
  modulation.

**As built (step 12).** `PatchView` draws `toFlow(graph)` with React Flow: a pure
function of the graph that gives each instrument a strip (the instrument, then its
effects, in columns of 376 px), its modulators above the strip over the node each drives
(side by side when they drive the same), each group to the right of the longest lane level
with the middle of its members, and the master at the end in the middle of it all. Nodes are
304 px wide, a header is 44 px and a row of four controls 108 px, and a modulator's node lists the cables that start at it, 8 px of room and
20 px for each (step 36; `CABLES_GAP`, `CABLE_ROW_HEIGHT`); the layout assumes those and
`patch.css` keeps to them. A node shows its plugin's first eight parameters and every one a
cable ends at (until step 30, when the session began to keep pins), then its own gain; each control
has a port on its top edge, quiet until a cable ends at it. An audio cable is grey, a
modulation cable magenta, dashed and thicker the deeper it is; (a cable that ends at a
modulator's rate or stage has had a handle since step 26, below, and one that ends at the depth of a cable since step 36, below: a
port on the row of that cable in the node of the modulator it starts at). The canvas is laid out
again when the structure of the session changes (`sameStructure`: nodes, their places in the
tree, names, the parameters of their plugins, cables, and where the session has placed a node) and not for a value.
Nodes cannot be selected or cabled by hand; they can be moved since step 25 (below). React Flow gives a node that can be neither dragged nor selected
`pointer-events: none`, which put its pane over every knob; `patch.css` takes the pointer
back for the nodes (a browser found that; jsdom has no CSS).

**As built (step 14).** A modulator is turned on the canvas. An LFO's node has a knob for its
rate and a choice for its waveform, an envelope's a knob for each stage, and a MIDI control's, which has
no settings, says what it is bound to as before; each is a control like a parameter's (`Drive`:
shown as set until the engine has answered, one intent a frame, a gesture's end at once), reading
its own value, and the node renders from what it is made of (its kind, and what a control is
bound to) and not from its settings. A rate and the times of an envelope are turned over the
decades (`Knob scale="log"`: each decade is the same share of the turn), a level evenly; the ranges,
the defaults a double click goes back to and the order of the waveforms are the controller's,
generated into `constants.ts` from the Rust that clamps them (D19) and not copied. Every modulation
cable has a slider at its middle for its depth, from -1 to 1 in hundredths, with the percentage
beside it and a name that says what the cable joins (`Depth of LFO → Filter: cutoff`); it sits in
React Flow's layer for what labels a cable, which is over the nodes (at the middle of a long cable
that is sometimes over a node's edge). A cable's depth is a value, not part of the layout: it is
left out of `sameStructure` and the cable reads it from the store for its thickness, so that
turning it renders that cable and not the canvas. The layout allows a modulator with settings 188
px (a row of controls more than the 140 of a control without; 184 measured against the real
program), and a strip leaves room above it for the tallest of its modulators and a gap for the
cables. Adding and removing modulators and cables, and moving nodes, are not done.

**As built (step 15).** Modulation is built on the canvas, by the controller's four structural
intents (§5) through `Editor` (`web/src/client/editor.ts`: what the user does to the structure,
as one place; `addModulator` resolves with the id of the node it made, and every method says,
when the server refuses, what it could not do and why, as a notice in the names the canvas
shows). The header of every instrument and group node has a "+" that opens a menu of an LFO, an
envelope and a MIDI control (`MenuButton`, the WAI-ARIA menu-button pattern; the menu is drawn
in the page and not in the node, where it would go under the next node and move with the
canvas); a modulator's header ends in a "×" that takes it out (the focus goes to its lane's "+",
asked for a frame at a time, since a node that has not been measured again is not visible and
takes no focus), and each cable has a "×" beside its depth (the focus goes to the list of its modulator, where
the next cable is drawn from: the button went with the cable, and the focus with it, to the page). A cable is drawn by dragging it from the
output at the foot of a modulator to the port of a parameter: while it is dragged the ports that
take it are marked and no others, and a drop anywhere else asks for nothing. The port of a parameter
that takes the cable grows from its dot to the whole cell of the control while the cable is dragged
(CSS, `.tang-port--takes`), so a drop anywhere over the knob is a drop on that parameter: the review of
step 15 measured that the radius of 40 px, which this said was enough, reached only the top half of a
knob, and at the foot of its cell took the port of the knob below it. The ports are the controller's rule read from
the graph (`reachFrom`, `drivablesOf` in `client/reach.ts`, in the data of each modulator's
node: D21). For a keyboard, and for the parameters a node does not show, a modulator has a list at
its foot ("Add a cable from LFO 2"), grouped by plugin, of everything it can still drive; a cable to
a parameter makes the node show it. The list is a `MenuButton` and not a native select, whose arrow
keys change its value, and so would draw a cable on each key, on Windows and Linux (the review of
step 15 suspected it; macOS opens the list instead, so it could not be seen here). `MenuButton` has
items in groups (a heading for each, a group of the menu for a screen reader), a height of at most
360 px past which it scrolls, a letter (or the first letters of a name, typed together) that goes to
the item that starts with it, and keeps the focus on its item when the items change while it is open;
the keys it uses are not heard by the page, whose keys play notes. What could not be done is on the `NoticeBoard` under the bar
(a polite live region; it does not go by itself): the refusals of these and the knobs' (a
`console.warn` before). Names that more than one node has are numbered (`client/names.ts`, shared
by the canvas and the notices). The layout allows a modulator with settings 218 px and one
without 128 (the list is 29 px, measured in a browser). A new node goes where the layout puts it
(a modulator with no cable above the first free column of its strip, and over the node it drives
once it has one); the canvas does not move to show it, and nodes still cannot be moved.

**As built (step 19, part 4).** Beside the "+" the header of every lane and group has an "fx" that opens a
menu of the effects that are installed (`AddEffectMenu` in `nodes.tsx`, over the same `MenuButton`): the
catalog the server told (`ClientState.catalog`) without the instruments, under a heading for each format,
the built-ins first and then CLAP, LV2 and VST3, in the order the server gave within each (`effectItems`,
`client/catalog.ts`). Choosing one asks for `AddEffect` (`Editor.addEffect`); the node is at the end of the
strip when the snapshot comes, and the focus is back on the button. The list follows the catalog while it is
open, as the scan finds more; with nothing to choose from (no catalog yet, or nothing but instruments) the
button is disabled. There is no sign yet that the scan is still going on.

**As built (step 20, parts 3 and 5).** Beside "fx" and "+" the header of every lane has a "♪" that opens a menu of
the instruments that are installed (`ChangeInstrumentMenu`: `instrumentItems`, the same order and headings as the
effects'), which asks for `ReplaceInstrument`; and the bar of the page has a "+ Instrument" that asks for
`AddInstrument`. The canvas moves to a node the page has just added when it is not all in the view (`Reveals`,
`app/patch/view.ts`, `ShowWhatWasAdded`): the node in the middle, at the zoom the person has or lower if it does not
fit, animated over 250 ms; a node that another front-end adds is not shown this way. (Step 31: while a move is going the next
node is judged against the view the canvas is moving to, which is remembered for the 250 ms the move takes, and not against the
point it is passing, where a node that the layout has moved since was "all in the view" and the canvas ended where it was not.)

**As built (step 21).** On an instrument, after the "♪", and on an effect of a lane, before the "×", a "≡" opens a menu of
the plugin's presets by name, in the plugin's order (`PresetMenu`: `presetsOf`, `client/selectors.ts`), which asks for
`LoadPreset` (`Editor.loadPreset`): the knobs take the preset's values when the server tells that it was loaded, or
when another front-end loaded it. The preset that was loaded last is marked (a check, bold, `aria-current`:
`MenuItem.current` of `MenuButton`) and the menu opens on it, so the keyboard is where the person left it. A plugin
with no presets has no button (the oscillator and the filter), and neither has an effect on a group's bus (the engine
loads a preset by a lane's slot). A plugin with hundreds of presets has one long menu (it has a height limit, and a
letter finds an item): the filter of the TUI's selector is not built.

**As built (step 22).** A node shows the first eight parameters of its plugin as knobs, and every one that a cable
drives; the rest are in the panel. A "⚙" (`ParamsButton`, "Parameters of <name>") beside the preset menu on every
instrument and every effect that has parameters (a group's bus effects too) docks a panel at the right of the canvas
(`ParamPanel`: the canvas holds which node is in it, `InspectorContext`; `.tang-patch` is a row of the canvas and
the panel): the plugin's name and format, a filter by name (`matchParams`: every word, any case, anywhere in the
name; focused as the panel opens), how many there are ("10 parameters", or "1 of 10 parameters" while a filter is
typed), a page of forty rows with a button for forty more, and a close button. A row (`ParamRow`) is the parameter's whole
name, a slider and its value in its unit (a choice if its values have names), and a bar of what the engine makes of
it while a cable drives it; it is turned like a knob, by the same `Control`, so the two agree (what is set shows
until the engine has answered, one intent a frame, a gesture's end at once), and its keys are a knob's. One panel at
a time: the button of another node puts that node's parameters in, with a filter of its own; the button again, the close
button and Escape (with the focus in the panel) close it, the focus going back to the button; a node that goes takes
its panel. Keys typed in the filter are text, not notes. Pinning a parameter to the node is step 30, below. Not built: a slider that
follows a parameter's steps or groups, a log scale for a range of decades.

**As built (step 23).** A lane's node has a cell called Keys after its volume, in the grid of its controls, with a
lowest and a highest key (`RangeCell`, `app/patch/controls.tsx`): each a choice of `any` (no end) or a note by name, which
asks for `SetRange` (`Editor.setRange`). The other end of the range stays; an end chosen past the other takes it
along, so that a choice is never refused for crossing; `any` at both ends is every note, which is no range. The lists
leave out the note that is any already (C-1 as a low end, G9 as a high one). What was chosen shows until the answer is in,
the last choice while earlier ones are answered, and then what the session has. The cell counts among a node's controls
for its height (`controlCount`: an instrument's are its parameters, its volume and its keys).

**As built (step 25).** A node is moved by dragging it by what is not a control (the controls, the menus and the
buttons are `nodrag`, as they were for the canvas not to pan under a knob): the header in practice, which has a hand for a
cursor. The drag begins when the pointer has gone three pixels (`nodeDragThreshold`; React Flow begins it where the pointer
is by then, so the node is a little behind it), a click is not a move, and a drag that is put down where it began says
nothing. The canvas holds the nodes it shows in a state of its own (`useMovableNodes`, over `applyNodeChanges`) and is told where a node is put down
(`onNodeDragStop`): the page sends `setNodeUi` once, at the end, with the node's position, and the cables follow the nodes as
they are dragged, as React Flow draws them. The session is laid out again whenever its structure changes, and a node must not
be taken out of a hand when that happens (a modulator added by another browser): `keepHeld` (`app/patch/moving.ts`) keeps the
nodes in hand, and those put down that the server has not answered for yet, where they are on the canvas, a count of drags
for each (`held`), and when the answer is in (or it is refused: the page says why, and the node goes back) the canvas takes
the session's place. The place is the server's: the event comes back with it tidied to a tenth, every other page draws the
node there, and a page that is opened again does. `toFlow` draws a node where the session says (`Node.ui`) and the others where
the layout puts them; `sameStructure` compares the places, so that a move from another page lays the canvas out again. A control
among those of the canvas, "Arrange the nodes again" (a `ControlButton` under the zoom's, disabled while no node has a place),
gives every node that has one back to the layout. Not built: moving a node with the keyboard (React Flow's own needs a node to
be selected, which they are not), snapping, a grid, a minimap, and several nodes at once.

**As built (step 26).** A modulator's numbers take a cable, from another modulator, at a port on the top edge of their knob, as a
plugin's parameters do (`CablePort`, which `ParamCell` uses too): an LFO's rate and the four stages of an envelope; not the
waveform, which is a choice, and not a MIDI control, which has no settings. The handle is named after the port (`rate`, `attack`,
`decay`, `sustain`, `release`; `handleOf`, `paramHandle` for a parameter's `param-N`), a cable to one is drawn from the
modulator's output to it and labelled for what it joins (`ADSR 2 → LFO 2: rate`, `portName`), and `drivenSettings` in the node's
data says which of a modulator's settings a cable ends at, for their ports to say so. A cable is dragged to one as to a parameter:
`cableOf` reads the connection into a modulator, a node and a `Port` (`param-N`, or a setting's name, and nothing else), and
`drawableFrom` says which ports take it, from `Drivable` in the data of the modulator's node (what it can still be drawn to, plugin by
plugin, `params`, then modulator by modulator, `fields`): the settings of the modulators it reaches and does not drive already, so
not its own, not another lane's (nor, for a lane's modulator, its group's), and the ports are marked while the cable is dragged.
The list at the foot of a modulator offers the same, after the parameters, under a heading for each modulator, and
`Editor.addCable(modulator, node, port)` takes a port, not a parameter's index. The rule is read from the graph by
`fieldsOf` and `modulatorsInReach` (`client/reach.ts`), pinned to the controller's by a fixture the controller writes
(`fixtures/fields.json`, as `reach.json` is for the parameters), and the mock engine draws a cable by it, with the controller's
refusals in its order and its words. The depth of a cable had no port on the canvas until step 36 (below): the controller accepts a cable to one (the
TUI's `t` makes them), and from then the page draws such a cable and offers it. **Layout.** A modulator that drives a
setting of another is a row above it, so that a cable always runs down, from the bottom of a node to the top of the one it
drives (`tiersOf`): a modulator is as many rows above its strip as the longest chain of modulators it drives (the cables the canvas
draws, within its rack); one that drives no other is in the row nearest the strip, over the plugin it drives, as ever; the
rows are placed from the nearest up, each modulator over the first column it drives that is free in its row, a row as tall as
the tallest in it, with room for the cables between the rows as there is between the rows and the strip. A rack in which none
drives another is one row, laid out as it was; a cycle is cut where it is first met, in the order of the graph; a cable to
a modulator of another strip (a group's to a member lane's) makes no row. A long chain is a tall canvas: the cascade is as
tall as it is long.

**As built (step 27).** A modulator's node shows what it puts out as a trace and not as a bar: the last four seconds as a line
that goes by from the right edge to the left and fades into the past, a dot at the newest value and, for a value that goes
both ways (an LFO, the pitch bend), a mark across the middle (`LiveTrace`, drawn by `drawTrace`). A rate that another modulator
drives (step 26) is seen to wander. The page keeps its own samples, as many as a ring of 1024 holds (`TraceBuffer`,
`client/trace.ts`, one for each node, taken on every frame from the telemetry slot the bar read: D30), `plotTrace` says where each
falls on a plot of the span (the newest at the right edge, the line coming in from the left edge, the newest value held to the
right edge when it is older than the frame), and the line is drawn on a canvas by the frame loop with no React state. A value that
has not moved for as long as the trace shows is drawn once (`TraceBuffer.changedAt`: the picture is the same whenever); the bitmap
is the size of the canvas on the page in the pixels of the device (`devicePixelRatio`), made again when either changes; a value that
goes (none, or not a number) clears the past. The trace is a `meter` to what reads the page, as the bar was (`aria-valuenow` and
`-valuetext` follow the value, the canvas is hidden from it). A modulator's node is 36 px taller for it (measured in a browser: 249
px with settings, 155 without; the layout's allowances are 254 and 164, `TRACE_EXTRA`). **Cables.** A cable is as bright as the
output of its modulator is high: `useCableLight` (`app/patch/cableLight.ts`) writes the output across its range (`outputRange`: -1
to 1 for an LFO, 0 to 1 for the rest) to `--signal` on a group around the cable, on the frames it moves in, and the style reads it
(`stroke-opacity: calc(0.3 + 0.7 * var(--signal, 1))`: dim at the bottom of an LFO's sine, bright at the top, and as bright as it can
be for a cable with no output to read). Not built: an envelope's curve drawn from its four stages (the trace shows the envelope
while notes are played; at rest it is a line at nothing), a trace of what a plugin's parameter is as modulated (its dot and ring
are the knob's own), the span to choose, and a trace that does not alias an LFO faster than half the rate of the telemetry (f85).

**As built (step 28).** Every effect, a lane's and a bus's, has two buttons in its header, before the preset menu: `‹` moves it one
place earlier in its chain (towards the instrument or the group) and `›` one place later (towards the master), each named for the
effect and the direction (`Move Filter 3 later in its chain`). The first effect of a chain has no place earlier and the last none later:
that button is dimmed and is `aria-disabled` and not `disabled`, so that it does nothing and keeps the focus a disabled button would
lose. Pressing one asks `MoveEffect` (`Editor.moveEffect(node, index)`, the place counted from 1 in the notice that says why it could not);
the answer is a snapshot, and the layout, which is a function of the graph's order, puts the chain out in the new order, the modulators
above it following the plugins they drive, and the cables, which name plugins and not places, still on the same ones. Where an effect is in
its chain is `chainPlaceOf(state, node)` (`{ index, count }`, null for what is no effect). A browser lets go of the focus of an element
whose node the page moves among its siblings (React moves the node of the effect that goes later, not the one it passes), so a button
that was pressed with the focus on it is given the focus again when the move is done (`keepFocus`, `app/focus.ts`: for twenty frames,
whenever the focus is found on the page itself; what a person has moved it to is left alone, and the latest request replaces the earlier;
found by a browser test, since jsdom keeps the focus). The canvas numbers the nodes of one name in the order of the graph, so an effect that
is moved past one of its name changes name with its place (the TODO in CLAUDE.md). Not built: dragging an effect to a place, a move that
reaches across lanes or from a lane to a bus, and moving a lane or a group.

**As built (step 29).** An envelope's node draws the shape its four settings make, between its knobs and the trace of what it puts out:
a line from nothing up to the top (the attack), down to the sustain (the decay), along it (the hold, which has no time: it lasts as long
as the key does) and down to nothing (the release), the area under it faintly filled (`LiveEnvelope`, `client/envelope.ts`). A stage's width
is a share of the shape by the logarithm of its time (`stageWeight`: log10 of the milliseconds plus one), since the controller lets a stage be
1 ms or 10 s and on a linear scale the short one would be a hair next to the long; the hold has the mean width of the three stages and never
less than one weight. React draws it from the four settings (`envelopeStagesOf`, `sameStages`: a knob that is turned draws the shape again
and nothing else of the node), and it is an image named for the node and its settings (`ADSR 2 shape: attack 10 ms, decay 300 ms, sustain
0.7, release 500 ms`). The node is 66 px taller for it (measured in a browser: 315 against an LFO's 249; the layout assumes 320:
`ENVELOPE_EXTRA` and `ENVELOPE_NODE_HEIGHT` in `toFlow.ts`).

A dot is on the shape where the envelope is while notes are played (D32). It is not React's: the frame loop moves it, on the frames its
level moves in, writing to the DOM only then, and hiding it while there is no level to follow. (A write is a change of the DOM and a test's
`waitFor` runs its callback again on every change, so a dot written on every frame, in a callback that ticks frames and whose assertion keeps
failing, never times out: the first version did, and a test run hung at 100% CPU.) The level alone does not say which stage that is (the same
level is on the attack, the decay and the release), so it is read with a course (`courseAfter`): which way it was last going, the level that
settled that, and the time. The telemetry comes thirty times a second and the page draws sixty, so every other frame finds the level exactly where
the frame before did, and that is no news: the level goes on the way it was going. (The first version took it for the level standing still,
which below the sustain is by its place the release: a slow attack was drawn on the attack on one frame and on the release on the next, thirty
times a second, until a browser test that asserted the attack found it: f90.) `envelopeDot` takes the level and whether it is rising: rising is
the attack, through the level of the sustain too (the attack goes through it on its way up, and a note that comes while the envelope is dying away
goes on from where it is); not rising, the level at the sustain is the hold, at the top the top of the attack, above the sustain the decay and
below it the release; at rest the start. The level is at the sustain when it is within a hair of it (1e-3) and not only when it is it, since the
telemetry holds 32 bits and the sustain of the session does not (f91); for a sustain of 1 the attack ends in the hold. A rise that has stood
still for 100 ms, three readings, is steady and no longer a rise: the mock's envelope, and a pluck whose attack and decay took less than a reading,
come to the sustain in one step and are in the hold. A move is more than a rounding (1e-5) from the level that settled the course. The course
survives a change of the settings (the next frame is not a rise that begins again) and is forgotten across a gap in the telemetry, after which the
first level is told by itself alone. Not exact: the first reading of a note is a rise whichever stage it is on (the next reading says), a note that
ends during the decay is drawn on the decay until the level is below the sustain, and the first reading after a gap is told by its place. Not built:
dragging the corners of the shape to set the stages, and the exact stage (the telemetry has the level and no phase: a channel for it would put the
dot exactly where the engine is).

**As built (step 30).** What a node shows as knobs is the session's to say. A plugin has a list of the parameters pinned to it (`Plugin.pinned`,
the plugin's own indices in the order they were pinned); `client/pinning.ts`, a pure module that the canvas, the panel and the editor all ask,
turns it into what is drawn. `shownParams` is the pinned ones, or the first eight (`DEFAULT_KNOBS`) of the plugin's if none are (a pin that the
plugin has no parameter for is left out, and if that leaves none it is the first eight), and `knobsOf` is a node's knobs: those and, after them in
the plugin's order, every parameter a cable ends at that is not among them, for as long as the cable is there (the cable is not a pin). The
canvas calls them `knobs` (`PatchNodeData.knobs`; it was `pinned`, which is the server's list now), the layout knows a node's height from them, and
a change of the pins is a change of structure (`sameStructure`): without that a node that gained a row would not have been laid out again.
`pinnedWith` is how a pin changes what a node shows: a node with nothing pinned has the first eight as its list, so pinning a ninth keeps them and
taking one off leaves the other seven, which is the case a person is in when they first open a panel; pinning what is shown or taking off what is
not changes nothing; a parameter the plugin has not, and a list longer than the server takes (`PINNED_LIMIT`), are refused before it is asked; and
taking off the last pin leaves an empty list, which gives the node back to the first eight. `Editor.pinParam(node, index, pinned)` works the list
out and asks `SetPinned`, resolving true without asking when it is so already and saying what it could not do in the page's own words.
In the panel each row has a pin, a button named `Show <parameter> on the node` with a tooltip, pressed (drawn filled) when the node shows the
parameter and open when it does not (`isKnob`, read from the session, so a pin another page makes shows); while anything is pinned a button under
the count, `Show the first eight again`, gives the node back to them (`hasPins`). Not built: a node with no knobs at all (an empty list is the
default, as D33 says), pinning from a knob on the node itself, and ordering the pins by dragging.

**As built (step 31).** The groups are in the page. *The group of a lane* is a cell on its node after its keys (`GroupCell`): a native
`select` (the keyboard and the screen readers have it for nothing) of none, each group of the session by the name the canvas gives it
(`groupChoicesOf`: the lane's group and the groups in the order of the graph; `sameGroupChoices` keeps the cell from rendering for what it
does not show), and a new group. What is chosen is shown until the server has answered, as the keys are, and goes back to what the
session says if it was refused (the page says why, among its notices); the tooltip says which group the lane is in by its whole name, which the
narrow choice may cut. None asks `SetLaneGroup` with null, a group asks it with the group's node, and a new group asks `AddGroup` with the lane
as its only member, so that a group is made and filled by one intent. A group that a lane is put in is shown to the canvas
(`Editor.setLaneGroup`, `Reveals`), since the layout moves a group to where its members are. *"+ Group"* in the bar (`AddGroupButton`) asks
`AddGroup` for an empty one and shows it. *Naming*: the ✎ of a group turns its name in the header into a text field (`GroupNameField`), with the
name selected and as long as `NAME_LIMIT` allows; Enter and leaving the field ask `RenameGroup` (nothing, if the name is the one it was), Escape
asks nothing, and Enter and Escape give the focus back to the ✎. The field takes the Enter itself (`preventDefault`): the focus goes to the button
while the key is being handled, and a browser gives the rest of the key, its keypress, which presses a button that has the focus, to whatever has
the focus then, which opened the field again; a test in jsdom cannot see that, and the browser test found it. *Taking out*: a "×" on every lane and
every group, but not on the only lane of a session (`RemoveLaneOrGroupMenu`), opens a menu of two items, "Keep X", where the focus starts, and
"Remove X for good: its 2 effects and 1 modulator go with it, and its 3 lanes stay, in no group" (`removalOf` counts the children of the node
in the graph; `removalWords` says it). There is no undo, so the server is asked nothing before the second is chosen, and afterwards the focus goes
to "+ Instrument" or "+ Group", which is what is left to do. The mock engine does all of it as the controller does (`addGroupOutcome`,
`setLaneGroupOutcome`, `renameGroupOutcome`, `removeLaneOutcome` and `removeGroupOutcome`, pinned by `fixtures/groups.json`); the one place it
works by a rule of its own is the default name of the groups after one that is taken out ("Group 3" becomes "Group 2"): it renumbers the groups
that are called by their place, and one that was given the name "Group 2" at its place is renumbered with them, since the page's graph has the
name and not whether it was ever set (the controller knows). Not built: undo of this (the page has it for what is set, step 32), putting a lane in a group by drawing its audio to it (the menu is
what every keyboard has; a cable is for the mouse), and taking a lane out of its group by dragging.

### 9.4 Performance rules

- Telemetry never passes through React state: a `requestAnimationFrame` loop
  reads typed-array buffers and writes canvas / SVG attributes / CSS variables
  directly.
- Optimistic local updates for user-driven parameters; server echoes are
  reconciled, and ignored for the control currently being dragged.
- At most one intent per frame per parameter.
- Long lists (parameters, catalog) are virtualized; the graph renders only what
  is visible.
- Budgets, to be validated and enforced by a Playwright perf test from phase 0:
  60 fps with 32 modulators and 64 live knobs; drag → engine intent within one
  frame plus a loopback round trip; zero React renders caused by telemetry.

**As built (step 12).** One `FrameLoop` (`client/frameloop.ts`) serves everything that
draws a live value: a knob's dot, a modulator's bar (a trace since step 27, and the dot on an envelope's shape since step 29), the master's meter and CLIP light
subscribe a callback, which reads its slot (`slotReader` finds the slot once for each
layout, not for each frame) and writes SVG attributes or CSS variables; it writes only when
the value moved, and no React state is involved (tests count React's commits over a
hundred frames and find none). The loop runs only while somebody listens. A control that is
turned is a `Control` of the `Drive`: `set` shows the value at once and sends the latest
value on the next frame, `commit` (a gesture is over: a key, a double click, a choice) sends
at once unless this control has already sent in this frame, and what the control shows goes
back to the session's value only when everything it sent has been answered, so that the
server's echoes of a drag never pull the knob back under the pointer. Knobs are controlled
components: whoever holds the value decides what they show. Each control reads its own value
from the store (`paramOf`, `gainOf`), so a change renders that control and no other; the
ring is `modulationExtent`, the engine's own laws (LFO ± depth × range, an envelope from
depth × (base − min) up to the base, a CC up from it, the pitch bend both ways) summed and
clamped, and the dot is the telemetry's effective value.

Measured by the browser tests (`web/e2e/tracer.spec.ts` against the real program on the
null backend, `stress.spec.ts` against the mock engine serving eight lanes, 32 LFOs and 64
driven parameters), headless Chromium on the development machine: with 242 telemetry frames
in four seconds the tracer page ran at 60.0 fps (95th percentile frame 16.7 ms) and React
committed 0 times; a drag of 121 pointer moves sent 100 intents, never two in a frame, the
latest 1.5 ms after a pointer move, the slowest answered in 1.4 ms on the loopback; fifty
moves in one task sent one intent, the last; the big scene ran at 60.0 fps (16.8 ms) with 0
commits, and at 60.0 fps while a knob was dragged. The tests enforce 55 fps and a 34 ms
95th percentile (50 fps and 40 ms during a drag), which leaves room for a slow runner and
still fails for a frame that takes 25 ms. The commit count is React's Profiler, which only
a development build has, so the browser tests run on the Vite dev server (`Instrumented`,
`window.tangCommits`).

**As built (step 27).** The traces are canvases drawn on the frame loop, and the cables are written a CSS variable: both are
telemetry that does not pass through React. Measured again at the end of the step with 32 traces and 64 cables lit by them in the
big scene: 60.0 fps (95th percentile frame 16.8 ms, worst 16.8 ms) and 0 React commits in four seconds, and 60.0 fps (16.8 ms) while
a knob is dragged; the tracer scene 60.0 fps (16.7 ms) over 366 telemetry frames with 0 commits.

### 9.5 Piano

**As built (step 24).** The TUI's Piano tab is on the page, below the canvas (`app/piano/`: `Piano`, `PianoBar`,
`PianoKeys`; `client/pianoLayout.ts`, `client/pointerPiano.ts`, `client/holds.ts`). Its place is there from the start, empty
until the session is, and as tall as it is with the keys in it (149 px): the canvas is fitted to the room it has when it is
first drawn, and a piano that came a moment after took some of it (the Oscillator's node ended up partly under it).

- **The keys.** The white and the black keys of as many whole octaves as the width has room for at 30 px a white key (`octavesFor`:
  one to six; the width of the keys is watched by a `ResizeObserver`), starting where the notes that the keys of the computer play are
  in the middle (`firstBlock`: they play 29 semitones from the C of the octave that the bar's `Keys: octave N` says, and `[` and `]`
  move the window as they move the keys). The white keys are a row that shares the width; a black key is placed over the boundary
  after as many white keys as come before it (`--after`, with `--whites` on the row). A key is a button named by its note as the TUI
  names it (`C4` is 60), with the letter of a white key on it and the octave as well on a C; it is not a tab stop (the computer's
  keys are the keyboard's way to play, and sixty-odd tab stops are no way to move through a page).
- **Colours** are the TUI's. A stripe along the bottom of a key says what it is in the scale the server tells (`PianoMeta`'s root
  and pitch classes: the page holds no table of scales and works none out): gold for the root, teal for another note of the scale,
  none for the rest. A key that is held sinks a shade; one that is held and not in the scale goes red, the wrong note. When the
  piano is locked a key that is not in the scale is dimmed: it is not played.
- **Held keys** come from the `held` telemetry channel (§6). A callback on the page's frame loop writes `data-held` and
  `aria-pressed` to the keys that changed, whoever holds the note (the page, a MIDI keyboard, the TUI's virtual piano, another
  browser), and no frame makes the piano render again. What is lit is read off the elements when the keys are drawn again (React keeps
  the element of a key that is in both windows when the octave moves, and a note that ended meanwhile must not stay lit).
- **Playing.** A pointer plays the key it is on while it is down (`PointerPiano`): it slides from key to key as a glissando, the note
  ends where it goes off the keys and plays again where it comes back to one, and several pointers play a chord. The keys' row asks to
  keep the pointer (it plays on over the edge of the keys) and the page lets go of it wherever it goes up, when the window loses
  the focus, when the page is hidden and when the piano is taken off. A press neither moves the focus nor selects, so a knob that has the
  focus keeps the keys of the computer. The notes of the computer's keys and of the pointer go through one count of who holds each note
  (`NoteHolds`, the provider's `useNotes`), so that a note both hold ends when the last lets go.
- **Scale and lock.** The bar has the scale as two choices, a root and a kind (`SCALE_ROOTS` and `SCALE_KINDS` of the generated
  constants), and a toggle, "Lock to scale". They ask `SetScale` and `SetLocked`; like the keys of a lane they show what was chosen until
  the answer is in (the last choice while the earlier ones are answered), and after it what the session has. The lock is the sender's:
  the server drops a note that is not in the scale from every source (a MIDI keyboard, the computer's keys, the pointer), so the page
  only shows it. Both are saved with the session (`[piano]`), and the TUI's `k` and `l` ask the same intents, so each sees the other's.
- *Not built:* a picker that filters as the TUI's does (two plain choices do), the notes of the scale named on the keys, the chord readout and
  the scale strip of the TUI's tab, a velocity from where on the key the pointer is, the labels of the computer's keys on the piano.

## 10. Testing and CI

- **TUI safety net first:** golden-render tests through ratatui's `TestBackend`
  (`insta`) before any refactoring starts.
- **Rust:** controller tests against a stub command receiver; golden session
  round trips; telemetry tests that drive `AudioGraph::process` without cpal (as
  the `chain.rs` tests already do); golden JSON for the protocol; a freshness
  check on the generated TS types; web integration tests (WebSocket client ↔
  controller ↔ null audio).
- **Null audio backend** (`--audio-device none`, built in step 9): a timer-driven
  `process()` thread, so `tang serve`, CI and end-to-end tests run without audio
  hardware.
- **Web:** Vitest for the client layer, Storybook for components, Playwright
  end-to-end against `tang serve --audio-device none`.
- **CI:** a new job (Linux first, macOS later) with Node: `npm ci`, build,
  `npm test`, then `cargo clippy` / `cargo test --features web`. The existing
  matrix, including Windows' `--no-default-features --features vst3`, is
  untouched; whether `web` joins it is open.
  **As built (steps 10 and 11):** the matrix also runs clippy (`--all-targets`) and
  the tests with `--features web` on every platform; the job `typescript` checks the
  generated types; and the job `web` runs, on Linux, `npm ci`, lint, types, unit
  tests, the build, the Storybook build, then builds the program (without the plugin
  formats), installs Chromium and runs the browser tests, keeping the report when they
  fail.

## 11. Phases

Each phase moves a feature's logic out of the TUI into the controller and
exposes it to the web in the same step, so nothing ever has two
implementations. The TUI golden tests are the regression net.

0. **Foundations:** controller and graph model, protocol, telemetry v1,
   `tang serve`, web skeleton, mock engine, CI. *Exit:* an LFO visibly drives a
   filter-cutoff knob in the browser, matching what you hear, and the TUI and
   browser reflect each other's changes. (Built, steps 1 to 13, and checked except
   for "what you hear": the browser tests and the parity check ran on the null
   backend, and the dot is the engine's own value as the audio thread sets it.)
1. **Session parity:** outline and inspector, chain editing, presets, pickers,
   save, undo / redo, off-thread plugin loads, parameter metadata for the
   CLAP / VST3 / LV2 backends. *Exit:* build a session in the browser and load
   it in the TUI unchanged.
2. **Piano parity:** scale / lock, held notes, chords, BPM. *Exit:* play from
   the browser keyboard; held notes and chords match the TUI. (The computer keyboard
   plays notes in the page since step 16, ahead of this phase, so that what is built
   can be tried by ear without a MIDI keyboard; step 24 built the scale and the lock, the
   held notes and an on-screen keyboard, §9.5; still to do: chords, BPM.) (Moving the nodes of the canvas and
   keeping where they are in the session, §4.4, was done in step 25, ahead of the rest of phase 1.)
3. **Modulation, groups, patterns, MIDI-learn on the canvas.** *Exit:*
   everything the TUI's `t` / `m` / `g` / `l` flows do, by drawing cables. (Turning
   a modulator's settings and a cable's depth was done ahead of this phase, in step 14,
   D18, and adding and removing modulators and cables in step 15, and cables between modulators, to the rate and
   the stages, in step 26, and groups (adding, naming, putting a lane in one and taking one out, from the page) in step 31, and cables to the
   depth of a cable in step 36; still to do: an audio cable that puts a lane in a group, patterns, MIDI-learn.)
4. **Extras:** mixer view, session browser (needs runtime session loading),
   Web MIDI, touch layout.
5. **Engine track B** (separate, §13).

## 12. Phase 0 step list

Each step is a reviewable change; the TUI must not regress at any point.

- [x] **1. TUI safety net.** `draw(frame, state)` split out of `render` (the
  frame-drawing is now backend-agnostic, so it runs against ratatui's
  `TestBackend`); `State::new` extracted from `run` (pure construction — the
  initial transpose / pattern push stays in `run`); `insta` added as a
  dev-dependency. `src/tui/golden_tests.rs` builds its sessions as TOML text and
  loads them through the real loader (`app::load`, since step 3; at first they
  were assembled by hand from builtin plugins) and drives them with real key
  events through `process_event`:
  - 15 golden renders (`src/tui/snapshots/*.snap`) of `examples/split.toml`, a
    group scene (members, bus effect, group LFO + ADSR with a cross-mod target,
    a filtered bass), the selector / target / modulate / value / group-assign
    popups, and the Piano tab. Snapshots hold the characters **and** a per-cell
    style map, because the piano paints its keys with background colour on blank
    cells.
  - 17 scenario tests: key sequence → commands sent to the audio thread + the
    tree the user sees, including the index fix-ups (bus-effect / group-modulator
    / group-member removal, member join) that steps 2–6 move.
  - Mutation-checked: disabling a fix-up, changing a default mix, a one-unit
    colour change and a dropped `BOLD` each fail the expected tests.
  After an intentional UI change: `INSTA_UPDATE=always cargo test golden`, then
  review the snapshot diff. *Done:* `cargo test` and clippy green, no behavior
  change.
- [x] **2. Extract the model** into `src/app/model.rs` (not `core`: a module
  named `core` shadows the std crate). Moved: `PluginSlot`, `ParamSlot`,
  `ParamKind`, `PatternState`, `InstrumentNode`, `GroupNode`, `ModSourceSlot`,
  `ModulatorSlot`, `ModTargetSlot`, and the pure helpers: `default_effect_mix`,
  `format_from_id`, the modulator row helpers (`modulator_param_len` /
  `_range` / `_is_enum`, `modulator_row_to_cross_mod`),
  `mod_source_slot_to_graph`, the five index fix-ups and `lane_member_ordinal`
  (the `tui_` prefix is gone from their names). `to_plugin_slot` /
  `to_modulator_slot` stay in the TUI for now: they convert the `Loaded*`
  structs, which step 3 deletes. 14 unit tests cover the helpers (the fix-ups
  had none), each mutation-checked. *Done:* snapshots unchanged; `cargo test`
  and clippy green on both feature sets.
  Noticed while moving, not done: the fix-ups exist twice with the same logic
  (on `ModulatorSlot` here, on the audio `Modulator` in `plugin::chain`), and
  the modulator row tables (`modulator_param_range`, `modulator_edit_state`,
  `modulator_row_to_cross_mod`, the rows built in `render_session`) repeat the
  same ranges. Both are natural to unify once the controller owns edits
  (steps 5-6).
- [x] **3. Extract load** into `app::load::load_session` (two commits). The
  group, instrument, effect, pattern and transpose loading moved out of
  `run_session` with `load_lane_modulators`, `load_group_modulators`,
  `build_mod_source` and `member_mod_field_info`. `load_session` returns the new
  `SessionModel { instruments, groups }` (in `app/model.rs`) and the
  `tui::Loaded*` structs, `to_plugin_slot` and `to_modulator_slot` are gone;
  `tui::run` and `State::new` take the model. `session::parse(&str)` is split
  from `session::load(path)` so tests load TOML text. The splash and `play` both
  follow progress through a callback (`Progress::{Loading, Loaded}`); `main.rs`
  went from 1376 to 509 lines. The loader sends commands over a channel it is
  handed, so the tests give it a stub receiver and assert on the model **and**
  the command stream (`app::testutil::describe` prints one line per command).
  - 13 loader tests: `basic.toml` and `split.toml`, groups before instruments,
    group modulators resolved against their own group's members, modulators
    nested under effects migrating into the lane, unbound MIDI-learn re-arming,
    command order per lane, locked VST3 parameters hidden and labelled ones
    turned into enums, unresolvable targets skipped, an unknown plugin and a
    closed channel failing the load.
  - The golden tests now feed the real loader and all 15 snapshots are
    **unchanged**.
  - Mutation-checked: seven injected loader bugs each fail a test. Three
    survived the first pass (values not starting from the baseline, group
    modulators resolving against every grouped lane's chain, unsorted overrides)
    and got stronger tests.
  Two behaviour changes, both on purpose: parameter overrides are now applied in
  name order (they came out of a `HashMap`, so the command stream differed from
  run to run), and the loader always sends `SetPatternEnabled` (commit
  `43f7fdc`: `tang play` was enabling patterns the session marks disabled).
  Unchanged on purpose: the splash counts instrument and effect slots only, not
  group bus effects. *Done:* `cargo test` and clippy green on both feature sets.
- [x] **4. Extract save** into `app::save::to_save(instruments, groups)`. The
  123-line body of `State::save_session_to` moved out of the TUI (which now
  builds the piano config, calls `session::save`, clears the dirty flag and
  logs); it returns the `Save*` structs `session::save` already took. The plan
  said `Session::to_save()`, but the TUI still holds lanes and groups as two
  fields, so it is a function of those until step 5 gives the controller a
  `SessionModel` to own. `session::save` is split into `to_toml(..) -> String`
  and the file writer, which now creates the missing directory (the TUI used
  to).
  - `app::save::tests`: eight scenes (`basic.toml`, `split.toml`, and six built
    from builtin plugins: groups, legacy nested modulators, MIDI-learn, lane
    settings with remap / presets / pattern, cross-modulation of every kind, a
    disabled pattern) are loaded, saved and loaded again. The engine commands
    match, the models match (compared through their `Debug` dumps, so a field
    added later is covered without touching the test), and a second save writes
    the same text. The model types derive `Debug` for this; the loader's scenes
    moved into `app::testutil` to be shared.
  - Mutation-checked with a small runner (inject, test, restore): 45 injected
    bugs in `to_save` (each field dropped or reset), all caught. Two survived
    the first pass (an instrument's preset, a forced `enabled`) and got a test
    each.
  - Before the TUI switched over, a throwaway test compared the old inline body
    with `to_save` + `to_toml` on ten scenes at four edit stages (40 saves, each
    edited one different from the unedited): byte-identical.
  - Found on the way and fixed first, in its own commit: the Piano tab's lock
    (`[piano] locked`) was read at startup but never written back.
  Noticed while reading the output, fixed right after in its own commit: saved
  files carried f32 noise (`depth = 0.03999999910593033` for a source `0.04`),
  inline remap tables lost the space before `}`, and `mod_depth` was written as
  a multi-line array.
  *Done:* `cargo test` and clippy green on both feature sets, snapshots unchanged.
- [x] **5. Controller v0.** `app::Controller` owns the session and the engine
  handles and does the housekeeping between frames; `State` keeps the view and a
  `ctl: Controller`. Done in eleven commits (tests, moves and two fixes), none of
  them changing a snapshot:
  - **Lifecycle.** `poll()` reaps `return_rx` and polls MIDI hot-plug, in the TUI
    and in `play` (the two bugs of §2; both had been missing from the TUI).
  - **Session and handles.** `Controller::new(model, EngineHandles, session_path)`
    holds the model, session path, dirty flag, tempo, command channel, plugin
    runtime, notification receivers and the `Arc`s shared with the note senders.
    The fields are public for now, so the TUI keeps editing through disjoint field
    borrows; step 6 closes them behind intents. `State::new` took fourteen
    arguments and `tui::run` thirteen; now two and one. The plugin runtime is
    dropped after the last reap at shutdown (the TUI used to drop it first).
  - **Notifications and catalog.** `poll()` folds finished pattern recordings and
    MIDI-learn captures into the model and the plugin scan into
    `app::catalog::Catalog`, and returns `Polled { model_changed,
    catalog_changed }` instead of the `bool` sketched in §3.1: a bool cannot tell
    the TUI whether to rebuild the tree or refresh an open selector. `tick` is
    now `poll`, then those two reactions. `play` does not scan the catalog.
  - **Cursor-free methods.** `save`, `save_as` (with its rules: the session's
    directory, `.toml` appended once, trimmed, a failed save changes nothing),
    `load_plugin` and `set_instrument` moved; `load::fresh_slot` replaced the
    TUI's near-copy of the loader's slot construction. The TUI keeps the popups.
  - One type for where a modulator lives (the engine's `ModulatorLocation`
    replaced the TUI's `ModScope`), and `SessionModel::modulators_mut`.
  - **Tests.** Golden tests were added before each move for what the TUI did
    (notifications, the catalog scan, Ctrl+S and Save As, `n`, a plugin picked
    from the selector); the controller got its own tests (`stub_engine`, tempo,
    reaping, every drain, `Polled`, saving, loading) and `Catalog` its own. The
    moved code was mutation-checked (about 160 injected bugs in all); the
    survivors were test gaps and got tests.
  - Found and fixed on the way, each in its own commit with the test first: a
    finished recording did not mark the session changed (f12), and a MIDI-learn
    capture with no modulator to bind did (f13).
  *Done:* `cargo test` and clippy green on both feature sets, snapshots
  unchanged, `poll()` reaps returned plugins (tests), the TUI and `play` start
  and quit cleanly under a pseudo-terminal. **Not yet done by hand:** hot-plug
  with a real MIDI controller while the TUI runs (it needs a device).
- [x] **6. Intents v0.** Ten commits (three tests-first pins, four features,
  three fixes), none of them changing a snapshot:
  - **Node ids.** Every node of the model has a `NodeId`: groups, bus effects,
    lanes, effects and modulators (a lane's instrument plugin shares its lane's
    id). `SessionModel::alloc_node` hands them out, never reusing one during a
    run; they are not saved. The loader and every place that adds a node
    allocate; `SessionModel::{nodes, locate}` resolve an id to a `NodePath`, the
    positional address the engine uses. The golden harness checks the ids after
    every key press (unique, allocated here, instrument = lane). **The TUI tree
    entries do not carry ids:** they keep their positional `TreeAddress`, and the
    TUI reads the id from the model at the cursor when it builds an intent. A tree
    keyed by id is for when the TUI builds more intents (`TreeAddress` and
    `NodePath` are nearly the same type).
  - **Intents.** `app::intent` has `Intent`, `Event`, `Applied { rev, events }`
    and `IntentError { NotFound, Invalid }`; `Controller::apply` (`app::apply`)
    resolves the node, edits the model, sends the engine command and answers with
    the events, the revision going up by one per applied intent and a failed one
    changing nothing. `SetParam` clamps to the parameter's range and rounds enums
    to a whole value; `SetGain` covers instrument volume, effect and bus-effect
    mix, group volume and pan, each with its own range; `LoadPreset` takes the
    preset's id and reports the values it set, matched to rows by the plugin's own
    parameter index (not by position: f16). The TUI's `adjust_param`,
    `set_param_value`, `set_group_bus_param`, the value popups and the preset popup
    build intents and react to the events (`State::apply` / `react_to`); the
    clamping, the enum rounding and the preset-value mirroring now live in the
    controller.
  - **`Applied` has `rev` and `events` but no `undo` yet** (§5 sketched
    `{ events, undo }`): the undo UI is phase 1. The old values `apply` needs for
    the inverse are at hand where it overwrites them, so adding `undo` touches
    `app::apply` only. `Busy` and `Io` errors come with the intents that can
    produce them. (Step 32 kept the old values, and kept them in a history of the controller's
    own and not on the answer: D35.)
  - **Tests.** Golden tests pinned what the TUI did before each move (parameter
    edits, gain popups, presets); `app::apply` has unit tests for each intent
    (model, commands, events, unknown and wrong-kind nodes, no change on error).
    About 170 bugs were injected into the new code; the survivors were test gaps
    (given tests) or equivalent mutants.
  - Found and fixed on the way, each with the test first: the group header kept
    its old volume after edits (f15), preset values were matched to rows by
    position, wrong for a plugin that hides parameters (f16), and a group
    modulator's target on a member's plugin did not follow that plugin when the
    member's effects were inserted, removed or moved (f08: the engine in one
    commit, the model and the TUI in the next; `SessionModel::remove_effect` /
    `move_effect` are where the Effect intents will land).
  *Done:* `cargo test` and clippy green on both feature sets, snapshots
  unchanged. Manual run under a pseudo-terminal: the same keys (oscillator
  detune and volume, a preset load and an edit after it, a group's volume and pan,
  the volume and mix popups, a bus-effect parameter) against the binary of step
  5's last commit and this one save byte-identical session files; the TUI (with a
  session, and the default one) and `play` start and quit cleanly. **Not
  checked:** `LoadPreset` on a plugin that hides parameters, and by ear.
- [x] **7. Engine telemetry.** `Telemetry` (atomic slots); the audio thread
  publishes modulator outputs, effective values and per-lane / group / master
  peak and clip. The TUI gets the `CLIP` indicator from `CLAUDE.md` as the first
  consumer, which validates the plumbing and closes a TODO. Done in nine commits
  (§6 describes what came out); the snapshot suite gained one file
  (`status_clip.snap`) and no existing snapshot changed:
  - **Allocation first.** A test-only counting allocator (`src/alloc_guard.rs`,
    `allocations_in`) lets a test say that processing a buffer allocates nothing.
    It found that the engine did (f18: two allocations per buffer for a lane
    with a modulator on a parameter, two more per group, from the vectors that
    collected the cross-mod and parameter writes). Scratch vectors kept between
    buffers fixed it; 14 injected allocations were all caught. Patterns, remap,
    transpose and the built-in plugins were already clean, and tests now pin
    that; each piece of telemetry code below has a no-allocation test too.
  - **`Telemetry`, `Meter`, the master meter.** `src/telemetry.rs`: the cells,
    `Slot`, `MeterSlots`, `SlotAllocator` and `Meter` (a held peak falling 20 dB
    in half a second, a clip count per buffer with |s| > 1.0). `AudioGraph`
    takes a `Telemetry` (`set_telemetry`) and publishes the master meter. 31
    injected bugs, all caught.
  - **`CLIP`.** `src/tui/clip.rs`: a bold red `CLIP` in the status bar, left of
    the BPM, for two seconds after the master meter's count last moved (`!=`, not
    `>`: the count wraps). Tested alone, through the golden harness and end to
    end with a real graph; 20 of 21 mutants caught, the survivor is equivalent.
  - **Modulator outputs and effective values.** Each modulator publishes what it
    put out and each parameter target the value its parameter was set to (base
    plus every modulator on it, clamped; the same number for every target on one
    parameter), for lane and group modulators. 22 injected bugs; the five
    survivors showed the fixtures had one parameter and one slot each, so two
    tests with two-parameter plugins were added.
  - **The controller makes what has slots.** The TUI had seven places that made
    a modulator or bound a target, each with its own copy of the lines. Golden
    tests pinned them first (19 mutants, 17 caught, two tests strengthened), then
    `Controller::add_modulator` and `add_target` took them over: 158 lines fewer
    in `tui/mod.rs`. `add_lane` and `add_group` followed for the same reason.
  - **Slots in the model and the loader.** `SessionModel` owns the
    `SlotAllocator`; `ModulatorSlot.output`, `ModTargetSlot.effective` and the
    nodes' `meter` hold what was handed out, by the loader and by the controller.
    `check_ids` verifies them (present, unique, allocated here, not the master's)
    and the golden harness runs it after every key press. End to end, a real graph
    fed the loader's commands publishes into exactly the cells the model names.
  - **Lane and group meters.** Measured where the buffer already is, so nothing
    is copied. 25 injected bugs; two survived (a `SetGroupMeter` that always hit
    group 0, a check that looked at only a meter's first slot) and got a test
    each.
  - Found on the way and fixed first, each with the failing test first: f18 (the
    allocations above) and f20 (targets declared in a session file were centred
    on the plugin's default instead of the value the session sets).
  - **Differs from the sketch above.** The slots do not ride in `AddInstrument`,
    `AddGroup`, `InsertModulator` and `InsertGroupModulator`: a follow-up command
    (`SetLaneMeter`, `SetGroupMeter`, `SetModulatorOutput`) carries them, so
    those commands are unchanged; a target's slot is a field of the `ModTarget`
    that `AddModTarget` / `AddGroupModTarget` already carried. A cross-mod target
    (one that drives a modulator's own field) has no effective-value slot.
    `Telemetry::load` and `Slot::index` have no reader outside the tests yet; the
    layout and the wire of steps 8 and 10 are theirs.
  *Done:* `cargo test` and clippy green on both feature sets. `chain.rs` tests
  drive `process()` and assert slot values for an LFO with a target, an envelope,
  a MIDI-learn control, group modulators and every meter; the audio path does
  relaxed atomic stores and nothing else. **Not checked then:** the `CLIP` light in
  the real binary (the audio callback does not run in the sandboxed
  pseudo-terminal sessions used for the manual runs); step 9 checked it.
- [x] **8. Graph projection and protocol types.** `SessionModel::graph()`;
  serde types for `Snapshot`, `Event`, `Intent`, `TelemetryLayout`; the
  `ParameterInfo` v2 fields filled in for built-ins; `ts-rs` export (feature `web`)
  into `web/src/protocol/generated/`. Done in nine commits (§4.2, §4.3, §6 and §7
  say what came out); no TUI snapshot changed:
  - **The `web` feature and the TypeScript check.** `web` is off by default and
    only adds `ts-rs` 12 for now (`serde_json` joins the dev-dependencies).
    `app::protocol` has the freshness test and the one command that rewrites the
    directory; mutation-checked by hand (an edited, an extra and a missing file
    each fail it). CI has a `typescript` job.
  - **A wire format for intents and events.** Camel-case JSON, tagged objects, a
    `StampedEvent` that carries the revision; `PresetLoaded` reports `{param,
    value}` objects instead of tuples. Tests pin the JSON of every variant and what
    a client may not send (no type, a Rust-cased type, a missing field, null for
    NaN, a negative node, an unknown gain). 10 injected bugs, all caught.
  - **Parameter metadata v2** (`unit`, `group`, `step_count`) for the built-ins and
    in `ParamSlot`. 13 injected bugs, all caught.
  - **Edge ids** for modulation targets (`EdgeId`, a series of its own).
    9 injected bugs, all caught.
  - **The graph** (`app::graph`): nodes, ports, edges, containment, the master
    node; a refactor on the way gave groups one name (`GroupNode::display_name`,
    three copies in the TUI). 59 injected bugs, all caught; every scene and the
    examples project into a closed graph.
  - **The telemetry layout and the snapshot** (`app::layout`, `app::snapshot`):
    `Controller::snapshot()` is the revision, the graph, the layout and what
    belongs to the session as a whole (dirty, the file *name*, the tempo, the
    piano's scale and mode). 26 injected bugs, all caught.
  - **The golden JSON** of the snapshot of the tracer scene (a saw
    oscillator through a filter with an LFO on its cutoff, volume 0.1: step 12's
    scene, added here), through the text the wire carries.
  - Found on the way and fixed with the test first: f21 (a group named `""` in a
    session file showed a blank header, the rename popup stores no name for an
    empty one).
  - **Differs from the sketch.** `Session::graph()` is `SessionModel::graph()`.
    `hidden` / `read-only` flags are not in `ParameterInfo` (nothing produces them
    before phase 1) and `stepped` is `step_count > 0`. Ports are one flat enum and
    only modulation edges have ids (§4.2). `TelemetryLayout` is two lists and its
    `rev` a fingerprint (§6). Patterns are not nodes, `ui` layout fields are not
    in the session format, and `Hello` / `Request` / `Response` wait for step 10;
    the types, the events for structural edits and `Applied.undo` follow the
    intents of phase 1. Everything not yet read by the web runtime carries a
    `dead_code` expectation that names step 10.
  *Done:* `cargo test` (541 tests, 545 with `--features web`) and clippy green on
  all three feature sets; the golden JSON test passes and the TypeScript is
  checked in the test suite and in CI. **Not checked:** the TypeScript compiles
  and means the same in a browser (there is no `web/` toolchain until step 11);
  the CI job has not run (it runs once the branch is pushed).
- [x] **9. Null audio backend.** `--audio-device none` starts a timer-driven
  thread that calls `AudioGraph::process` at the buffer cadence; shared by
  `serve`, tests and CI. Done in two commits:
  - **`audio::Renderer`.** What the cpal callback does (take the MIDI that
    arrived, render the graph into one buffer per channel, interleave) is a struct
    of its own, so that a second backend runs the same work. It had no tests; it
    has seven now (silence, a note and the channel order, any block length, MIDI
    taken off the channel once, a block rendered from zero, a failing graph).
  - **The backend.** `audio::NULL_DEVICE` (`none`) opens no host and no device;
    `AudioEngine` is an enum of the cpal stream and the null backend with the same
    `build` / `play` / `stop` (`play` takes `&mut self` now). A thread named
    `tang-null-audio` renders a block every `buffer / sample rate` seconds and
    throws the samples away; the cadence does not drift, and a stall of more than a
    period starts it again instead of bursting through the missed blocks. `stop`
    joins the thread, and the plugins are dropped by the caller. Normal priority:
    no real-time promotion. 13 injected bugs, 12 caught (the other was a no-op).
  - **Checked in the real binary**, which step 7 could not: `tang --audio-device
    none` on a session of three oscillators at four times the volume, with a note
    from a virtual MIDI port, logs "Audio callback running (null backend, 256
    frames every 5.333333ms)" and shows `CLIP` in the status bar. No sound card was
    opened, so nothing was heard.
  *Done:* `cargo test` and clippy green on all three feature sets; the tests run the
  engine without an audio device, send it a note and read the telemetry
  (`a_lane_that_is_too_loud_clips_and_the_clips_are_counted` and friends).
  **Not checked:** the null backend on Linux and Windows, and its timing under a
  heavily loaded CI machine (the pace test allows 15 to 60 blocks in half a second
  of 10 ms blocks).
- [x] **10. `web` feature and `tang serve`.** The axum / tokio runtime; `/`
  (embedded placeholder page) and `/ws`; `Hello`, `Origin` / `Host` checks,
  token for non-loopback; hub fan-out with bounded per-client queues; snapshot
  on subscribe; requests into the owner loop (headless loop = `select!` over
  intents, controller notifications and a tick); the telemetry pusher;
  `NoteInput` (adopted by `MidiManager` and `VirtualPiano` too) with
  per-connection note-off on disconnect. *Done when:* integration tests
  (WebSocket client ↔ controller ↔ null audio) cover connect → snapshot →
  `SetParam` → event + engine-side effect, `Origin` rejection, slow-client
  eviction and stuck-note release.
  - **Built, in fourteen commits** (each with its tests first, and each
    mutation-checked; the sections above describe the result). The parts:
    1. `NoteInput` (`src/note_input.rs`): the Piano tab's filter, the held-notes
       bits and the channel to the audio thread in one place, which the
       `MidiManager`, the `VirtualPiano` and the web runtime share
       (`Controller::note_input()`). It says what became of a note (`Delivery`:
       sent, blocked by Locked mode, not a note, channel full).
    2. `Controller::subscribe` / `unsubscribe` and the `EventSink` trait; `apply`
       publishes what it did.
    3. The protocol messages (`app::protocol`: `ClientMessage`, `ServerMessage`,
       `WireError`, `telemetry_frame`) and their TypeScript.
    4. `web::security`: the `Host` / `Origin` policy and the token.
    5. `web::hub`: clients, bounded queues, eviction, telemetry channels.
    6. `web::connection`: the protocol's side of one connection (token, limits,
       notes held, validation).
    7. `web::owner`: requests answered on the controller's thread; the cut between a
       snapshot and the events after it; layout changes told to the clients.
    8. `web::server`: axum, the guard, one task per connection with a seat, the token
       timer, the write timeout, shutdown.
    9. `web::pusher`: the telemetry frames.
    10. `web::runtime`: everything built in one place, and the headless loop, with
        in-process integration tests against a stub engine.
    11. `tang serve [session.toml] [--web-bind ip:port] [--web-origin origin]...`
        and `tests/serve.rs`, which runs the real binary on `--audio-device none`.
    Three more commits: the two bugs found on the way, each test first (`subscribe` and
    `telemetry` messages were not rate limited, so a client could make the owner build
    snapshots in a loop; and the session flags only worked before the subcommand,
    so `tang play x.toml --audio-device none` failed, and they are `global` now), and
    the move of the tests' WebSocket client into a module of its own.
  - **`tang serve`.** Loads the session as every mode does, prints
    `Web interface: http://127.0.0.1:8765/` (the address; with the token in the
    fragment beyond loopback) on stdout, logs on stderr, and stops on Ctrl+C /
    SIGTERM / SIGHUP (the `ctrlc` crate; closes the browsers with 1001). Without a
    MIDI subsystem (a container, CI) it carries on with a warning. A build without
    the `web` feature knows the subcommand and says how to get it.
  - **Checked.** 711 tests with `--features web` (597 without), among them 5 that
    run the real binary on the null backend (`tests/serve.rs`: nothing opens a
    sound card or a MIDI port, nothing is heard): a note played in the browser
    sounds and dies away in the master's meter, the oscillator's volume set to 0
    through `SetParam` silences the same note and set back brings it up (so the
    request reached the audio thread and the peak came back through the telemetry
    frames), a second browser hears the first one's change, a browser that vanishes
    with a key held does not leave the note sounding, foreign pages and hosts are
    refused and the session is untouched, and Ctrl+C closes the clients (1001) and
    exits 0. In-process (`web::runtime`, a controller over a stub engine): the
    command the engine receives, ordering of one client's requests, the late client's
    snapshot, the loop reaping on the owner's thread. Mutation checks per commit
    (the few mutants that survive are named in the commit messages).
  - **Slow-client eviction** is tested at three levels but not as one flow: the hub
    (a full queue evicts, the others do not notice), the owner (with a real
    controller) and the socket (a client that stops reading is given up on after the
    write timeout, and its seat is free again). A flow through a real socket would
    need megabytes of events: the sockets' buffers absorb everything until then.
  **Not checked:** the `web` feature on Windows, and the end-to-end tests on Linux
  CI (the branch has not been pushed, so the new CI steps have not run); the server
  against a real browser (step 11 brings one); the null backend's pace under a
  loaded machine; `tang serve` with a plugin that takes seconds to load (the
  runtime starts after the session is loaded).
- [x] **11. `web/` skeleton and CI.** Vite + React + strict TypeScript +
  ESLint; the `client` layer (connection, store, reducers, telemetry buffers)
  and the mock engine; `ui` tokens; Storybook; Vitest; the Playwright harness;
  `.gitignore` entries; a Linux CI job that builds `web/`, runs the web tests,
  then `cargo clippy` / `cargo test --features web`; `rust-embed` wiring.
  *Done when:* CI is green, and `npm run dev` against both the mock engine and
  `tang serve` shows the connection state and the raw session.
  - **Built, in nine commits** (the sections above, §9.2 and §10 especially, describe
    the result):
    1. `web/`: Vite 8 + React 19 + TypeScript 6 in its strictest settings
       (`strict`, `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`,
       `verbatimModuleSyntax`, no unused anything), ESLint 10 with typescript-eslint's
       strict and stylistic type-checked rules, Vitest 5 (jsdom for components);
       `package-lock.json` committed, Node 22 or later.
    2. The tracer's snapshot as a fixture (`web/src/client/fixtures/`), kept current
       by a Rust test the way the generated TypeScript is
       (`TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture`).
    3. `client/reducers.ts` (events applied to a snapshot, exhaustive over the event
       types, structure shared) and `client/telemetry.ts` (frame decoding, the index of
       what is where, buffers written in place for a drawing loop).
    4. `client/connection.ts` (`Client`: a vanilla zustand store and the calls a view
       makes; hello, token, subscribe, snapshot, events; resync on a gap; reconnection
       with a delay that doubles; requests that resolve with the revision and fail with
       the server's code), `client/websocket.ts` (the browser's transport) and
       `client/mock.ts` (`MockServer`: the same protocol in the page, with intents
       applied as the controller does and telemetry made up by the engine's formulas).
    5. `app/` and `ui/`: the first view (the connection state in words, the reason the
       page cannot go on, the session as JSON kept up to date), `ClientProvider` and
       `useClientState`, `?engine=mock`, and the design tokens as CSS variables
       (`ui/tokens.css`, with the TUI's colours for held, root, scale, modulation, group).
    6. Playwright (`web/e2e`): Chromium against the mock engine and against the real
       program on the null backend (through Vite's proxy, and directly).
    7. Storybook 10: the status in every state, the colours, and the whole page over a
       mock engine; every story is also rendered by `npm test`.
    8. `web::assets`: `rust-embed` serves `web/dist` (see §9.2).
    The ninth fixed what the work showed: the end-to-end test of `tang serve` left the
    program running when it failed to start it.
  - **Dev loop.** `cd web && npm ci`; `npm run dev` serves on 5173 and forwards `/ws`
    to a `tang serve --web-origin http://localhost:5173` (or `$TANG_SERVER`);
    `http://localhost:5173/?engine=mock` needs no server at all. `npm run storybook`,
    `npm test`, `npm run lint`, `npm run typecheck`, `npm run build`,
    `npm run e2e` (`npm run e2e:install` once, for the browser; it needs
    `cargo build --features web` first).
  - **Checked.** 103 Vitest tests (reducers against the Rust-made fixture, telemetry
    against the bytes of the Rust test, the client against the mock and against a
    scripted server, the app in jsdom, every story), mutation-checked with a small runner
    (about 90 mutants; the survivors are named in the commit messages), and 5 browser
    tests: the mock; the real server through Vite; a change from another client
    appearing on the page; the page served by the program itself, under its content
    security policy, with no console error; its files and its 404s. A release build was
    run with `web/dist` emptied, and still served the page and its script from inside
    itself.
  **Not checked:** the CI job `web` (the branch has not been pushed), browsers other
  than Chromium (and so the policy's `connect-src 'self'` for a WebSocket in Safari and
  Firefox), and a release build made before the interface was (it serves the
  placeholder: nothing warns at build time).
- [x] **12. Tracer UI.** `examples/tracer.toml` (saw oscillator → filter, LFO →
  cutoff); a canvas with the lane strip, a modulator node and a cable; a `Knob`
  with value / ring / dot; telemetry-driven animation outside React's render
  path; parameter drag → `SetParam`; Playwright perf test v0. *Done when:*
  dragging the cutoff knob changes the sound, the LFO visibly drives the knob in
  sync with what you hear, and the perf test enforces the §9.4 budgets for that
  scene.
  - **Built, in 28 commits** (§9.2–9.4 describe the result):
    1. The pieces that draw: the frame loop, the `Knob` (the arithmetic of a sweep, an
       arc and a drag; value arc, modulation ring, and a dot written to the DOM on the
       frame), `LiveBar`, `Meter` with its CLIP light, and `Choice`.
    2. The pieces that hold state: `Drive` (a control shows what it was set to and sends the
       latest value once a frame, until the server has answered), slot readers over the
       telemetry buffers, `modulationExtent` (the ring, from the engine's laws), and
       selectors for a node, a parameter or a gain by id.
    3. The canvas: `toFlow` (the layout and the cables, pure), the node components, the
       cables, `PatchView` on React Flow, `ClientProvider` handing out a drive and the
       frame loop, and the page showing the patch with the session as JSON closed below it.
    4. Support: jsdom stubs for React Flow, a rig for testing a view over the mock engine,
       a development-build count of React's commits, and `stressSnapshot` (eight lanes, 32
       LFOs, 64 driven parameters), which the mock serves at `?engine=mock&scene=stress`.
    5. The browser: the tracer scene against the real program, the performance tests of
       the tracer and of the big scene (numbers in §9.4), a burst of pointer moves.
    Two fixes came out of the browser tests (below).
  - **Checked.** 428 Vitest tests (the layout against several session shapes with a
    no-overlap check; the controls over the mock engine, which count React's commits across
    frames of telemetry and across a sibling's change; the canvas in jsdom, including a
    group, the ports and the clip light; every story is rendered), and 16 browser tests.
    Every commit that has logic in it was mutation-checked with the small runner (about 600 mutants in all;
    the survivors, real and equivalent, are named in the commit messages), and the browser
    tests against the program: no dots, a bar that reads nothing, a drag that does nothing,
    intents sent as they are set, a knob that renders on a timer, nodes without the pointer,
    no telemetry asked for, and a frame that takes 25 ms were all caught.
  - **Found by the browser tests, and fixed.** React Flow gives a node that can be neither
    dragged nor selected `pointer-events: none`, which put the pane over every knob: the
    drag did nothing, and the 19 jsdom tests of the canvas passed because jsdom has no CSS
    (f25). Looking into a failure of the drag test also showed a hole in the rule of §9.4
    (f26): a gesture's end could send a second intent in a frame that had sent one. (The
    failure itself was a bug of the test, whose socket spy logged its own reset requests as
    the page's; the hole was real, and `commit` now waits for the next frame when this
    control has already sent in this one.)
  - **Not checked:** by ear (the dot is the engine's own value as the audio thread sets it
    on the plugin, read from the telemetry, and the null backend was used, so nothing was
    heard); the CI job `web` with the new specs (the branch has not been pushed), and
    whether a shared runner keeps the thresholds; browsers other than Chromium (React Flow
    under the content security policy was checked in Chromium only); a touch screen; a
    session whose canvas is bigger than what is on screen (`onlyRenderVisibleElements` is
    off, so every node is drawn: the big scene is that, at 60 fps); a screen reader beyond
    the roles and labels (sliders, a meter, a native select).
  - **Not done, on purpose.** Moving nodes and cabling by hand, the outline, the inspector, the
    piano and the toolbar (§9.3), persisted pinning and layout (§4.4); a modulator's
    settings are shown but not editable (there is no intent for them yet: step 14). The modulation
    cable crosses the header of the node it drives, because its port is on the knob below
    the header.
  - **Dev loop additions.** `?engine=mock&scene=stress` serves the big session from the mock.
    `npm run e2e` runs the browser tests one at a time (they share the real program's one
    session, and each puts the filter back as the example has it); the commit count needs the
    Vite dev server, which is what Playwright starts.
- [x] **13. TUI and web together.** `tang --web[=addr]`: the TUI loop reads
  terminal events on a reader thread and `select!`s over them, web requests,
  controller notifications and a tick. Changes in either front-end show up in
  the other. *Done when:* a manual parity check passes both ways and a test
  drives both paths against one controller.
  - **As built:** §3.4 says how; ten commits, in this order. `Snapshot::apply`, the
    Rust side of the client's reducer, written against the controller's own snapshot
    (it found f27 and f28, which were fixed on the client with their own tests first);
    `app::subscribers` (the sinks, moved out of the controller); `Controller::resync`,
    which finds what no intent made by comparing what the subscribers were told with the
    session; the web side of it (`HubSink::resync`, and an owner that can serve a foreign
    loop); the TUI loop on a reader thread's channel; the TUI loop serving the web
    runtime, with the tab bar and the Help tab; the flag; and the warnings for an
    address beyond this machine.
  - **Numbers.** 642 Rust tests in the default build (598 before the step), 777 with the
    web feature, 436 Vitest tests (428); the 16 browser tests are as they were. 103
    mutations of the step's code were tried; every one that could change behaviour is
    caught (those that survived at first, a failure that comes behind other events in
    the loop and the arithmetic of the tab bar's label, led to stricter tests; two were
    equivalent, an arm that does what the fallback does and a guard that was there
    twice, which is gone).
  - **Both paths against one controller:** `tui::golden_tests::with_web` runs the real
    `event_loop` with a real server, and a WebSocket client on a thread of its own that
    presses keys by sending them to the loop (§3.4 says what it checks).
  - **The manual parity check**, scripted and run once, not part of CI: the real
    program on the null backend (`tang --web --audio-device none`, nothing heard), the TUI
    in a pty read through a small terminal emulator, and a real Chromium (Playwright) on
    the page it serves. 18 checks, both ways, all passed (§3.4). The scripts are not in
    the repository: a pty host in Python and a Playwright script of a few hundred lines.
  - **Not checked:** by ear (the null backend); `--web` on Linux and Windows (the
    terminal reader and `crossterm` on a console; CI has the tests, not a terminal);
    several browsers at once in the TUI (one was); a session with plugins that take
    seconds to load, whose load a browser asks for; a terminal that is not a pty.
  - **Open, noted on the way:** f29 (a taken port is found after the session has
    loaded; fixed since, after step 15) and f30 (the events of one intent share its revision). `tui/mod.rs` is 5,100
    lines and `golden_tests.rs` 2,650: the TUI wants splitting (the loop and the web
    glue, the status bar, the popups) before it takes more.

- [x] **14. Modulation from the browser (ahead of phase 1, D18).** A modulator's settings
  and a cable's depth are intents, the TUI edits them through the same intents, and the canvas
  has the controls. *Done when:* a browser turns an LFO's rate and waveform, an envelope's
  stages and a cable's depth on the real program, the TUI shows each at once, and the TUI's
  own edits show in the browser, both ways.
  - **As built:** `SetModulatorField { node, field, value }` and `SetEdgeDepth { edge, depth }`
    (§5), with `ModulatorFieldSet` / `EdgeDepthSet` events, `Snapshot::apply` and the client's
    reducer following them, and the mock engine answering them as the controller does. The
    ranges (`Gain::range`, `ModulatorField::range`, the depth's -1 to 1), what a new modulator
    starts as and the order of the waveforms are generated into `constants.ts` (D19), which
    removed the two copies of the gains' ranges the client had. The TUI's four modulator edit functions (a step and
    a typed value, for a lane's modulators and again for a group's, about 420 lines) are one
    `step_modulator_row` and one `set_modulator_row` over one table of what each row of a
    modulator's pane is (`modulator_row`), through `ModulatorSlot::row_intent`; `tui/mod.rs` is
    323 lines shorter (5,111 to 4,788). The Type row, which makes an LFO an envelope, is still the TUI's
    own edit (the browsers are given a snapshot). On the canvas: `Knob scale="log"`, the
    controls on the nodes, the depth slider on each cable, the layout's heights, and a cable's
    depth as a value (§9.3).
  - **The review of step 13** (an independent agent, read-only, on its own worktree) came in
    during the step: a NaN typed into the tempo popup or a modulator's value, or written in a
    session file, made every `resync` a full snapshot to every browser (NaN is not equal to
    itself; f31: fixed in three commits, the comparison, the popups, the file); a client that
    sent half a request and never the rest kept the web server from stopping for ever, in `tang
    serve` and in `--web` (f32); and a test of the loop's wake did not observe the wake (f33).
    Each was checked against the code before it was changed, and has its test first.
  - **Found on the way, and fixed:** the tree did not show a target's new depth after it was
    edited (f37); the structure of the canvas counted a cable's depth, which would have laid it
    out sixty times a second under a slider (fixed before the slider existed, `sameStructure`);
    a modulator's name carried its settings (`LFO 0.5Hz sine`), which change under a client.
  - **Numbers.** 679 Rust tests in the default build (643 before the step), 820 with the web
    feature (778), 502 Vitest tests (436), 22 browser tests (16), some 190 mutations of the step's
    code (the survivors, real and equivalent, are named in the commit messages). 146 local
    commits since `feat/midi-learn-mpe-expression`, none pushed.
  - **The manual parity check**, scripted and run once, not part of CI, as in step 13: the real
    program on the null backend with the TUI in a pty and a real Chromium; 14 checks, both
    ways, all passed: the rate, the waveform and the depth turned in the browser are in the
    TUI's tree at once, and stepped or typed in the TUI are in the browser's knob, choice and
    slider; a NaN typed into the TUI's popup changes nothing.
  - **Not checked:** by ear (the null backend); a touch screen; browsers other than Chromium;
    several modulators of one cable path overlapping their sliders (two cables with the same
    middle put their sliders over each other).
  - **Open, noted on the way:** f34 (a browser test measures milliseconds between sends where
    it means frames, and failed once under load); f29 and f30 as before; adding and removing
    modulators and cables, and the Type row, are still direct edits (the first two are step 15).

- [x] **15. Building modulation in the browser (ahead of phase 1, D18).** A modulator is
  added to a lane or a group, a cable is drawn from it to a parameter, and both are taken out,
  by intents; the TUI does the same through the same controller methods; the canvas has the
  controls. *Done when:* a browser adds a modulator to a lane or a group, draws a cable from
  it to a parameter (by dragging it, or from a list), and takes the cable and the modulator out,
  on the real program; the TUI shows each at once; and the TUI's own `m`, `t` and `d` show in
  the browser.
  - **As built, in the order it was done:** (1) *What the model knows*: `SessionModel::param_targets`
    says which parameters a modulator can drive (a lane's: its lane's chain; a group's: its
    members' chains and its bus effects), which the TUI's two target selectors use and the
    controller's `AddModEdge` is the intent of (D21); removal moved from the TUI into
    `Controller::remove_modulator` and `remove_edge`, which take with them the cables that point
    at what goes and, in turn, the cables that drive their depths, and the engine's
    `RemoveModTarget` and `RemoveGroupModTarget` were made live (with the fix-ups of the cables
    that address the later targets by place). (2) *The first structural intent, `RemoveEdge`*,
    and what it needs: `Applied.restructured` and the snapshot that follows (D20). (3) *The
    other three intents*, `AddModulator`, `AddModEdge` and `RemoveNode` (§5), each with the
    mock engine doing the same to a snapshot, and, for the rule of where a cable may end, a
    fixture the controller writes (`web/src/client/fixtures/reach.json`: a session with two
    groups and lanes in and out of them, a lane with nothing in it and six modulators, and what
    the controller accepted of every plugin for each) that the client's `reachFrom` is tested
    against. (4) *The client*: `client/names.ts` (the canvas's numbered names, shared), `Notices`
    and `NoticeBoard`, `Editor`. (5) *The canvas* (§9.3): the "+" menu (`MenuButton`), the "×" on a
    modulator and on a cable, the list at the foot of a modulator, the dragged cable. (6) *Both
    ways*, in browsers and in the real program (below).
  - **The review of step 14** (an independent agent, read-only, on its own worktree) came in during
    the step, and each of its eight findings was checked against the code, then fixed with its test
    first (f41 to f48): modulators of a lane laid over the groups and the master (the layout, with a
    seeded fuzz of 400 graphs where no two rectangles overlap); a TOML number beyond 32 bits that
    became infinity in the model and `null` on the wire, which blanked the page (the loader refuses
    it, and the page has error boundaries); **after Shift+Up/Down on an instrument in the TUI the
    model and the audio thread had different lanes**, grouped or not (CLAUDE.md said ungrouped ones
    were safe: the TUI swapped whole nodes, `SwapInstruments` only the plugin, and `ClearInstrument`
    cleared modulators the model kept; now both move whole lanes, with a test of the model against a
    real audio graph); a browser's edit that cleared the TUI's parameter filter; turning a modulator's
    setting drew every knob it drives again; nodes of one name had sliders of one name; the "one table"
    of a modulator's rows was true of stepping and not of drawing; and the mock's numbers differed from
    the controller's. Found by me on the way: f40 (a cable driving the depth of a cable that was gone
    after its modulator was removed), f50 (the mock dropped the slot of a value another cable still
    drove).
  - **Numbers.** 788 Rust tests in the default build (679 before the step), 932 with the web feature
    (820), 686 Vitest tests (502), 35 browser tests (22); about 220 mutations of the code written
    in the second half of the step (the survivors are named in the commit messages: two equivalent
    ones in the client's reach rule and its list, an id that is the same function at both ends, a
    refused add that reads the session) and some 70 of the groundwork before it, and more than a
    hundred for the fixes of the review below. `tui/mod.rs` is 4,681 lines (4,788). 187 local
    commits since `feat/midi-learn-mpe-expression`, none pushed.
  - **Both paths and both front-ends.** The browser tests: seven on the mock engine (a drag, the
    ports marked while dragging and only the ones that take the cable, a drop where it does not belong
    and a drop out of reach, a modulator added and dragged from at once, the list, the buttons), two on
    the real program (a modulator added and a cable dragged to the resonance are in the server's
    session, read by another client, and the engine applies them: the envelope at rest takes the
    resonance from 2 to 1.25 and the knob's dot is there, and goes when the modulator is taken out).
    **The manual parity check**, scripted and run once, not part of CI, as in steps 13 and 14 (a real
    TUI in a pty, a real Chromium, the null backend): 17 checks, both ways, all passed: an envelope
    added in the browser, a cable dragged, its depth set, the cable and the modulator taken out are in
    the TUI's tree as they happen (also with the TUI's cursor on the modulator that goes: the TUI
    carries on), and a modulator added with `m`, a cable with `t` and the modulator taken out with `d`
    in the TUI are on the canvas.
  - **Found by the browser tests, and fixed:** giving the focus back after a modulator is taken out did
    nothing in a browser (the canvas has not measured the node again, so it is not visible, and an
    invisible element takes no focus), though the jsdom test passed: the focus is now asked for a
    frame at a time (`focusWhenPossible`).
  - **Not checked:** by ear (the null backend); a touch screen; browsers other than Chromium; a screen
    reader beyond the roles, names and live regions (the menu follows the authoring practice; no
    reader was run); two browsers adding to one lane at the same moment (each may be given the
    other's node: the intent does not name what it made).
  - **Not done, and open.** Cables that drive a modulator's rate, stages or depths cannot be drawn on
    the canvas or by `AddModEdge` (they can be taken out by `RemoveEdge`, and the TUI makes them); the
    Type row; undo (a removal is final, as the TUI's `d`); the canvas does not move to show a node it
    has added; **telemetry slots are never reused (4,096 of them): each modulator and each driven
    parameter uses one, so a very long session of building runs out and later cables have no live
    dot**; f30, f38 and f39 as before; the TUI's removal of an effect, a bus effect
    or a member still drops the cables that pointed at it with `retain`, which leaves the depth cables
    of f40 pointing at nothing (phase 1: they are the controller's then).
  - **Found by me on the way, and fixed: f49.** A lane with no instrument plugin lost its modulators
    when the session was saved and loaded: the saver wrote them under the lane's `[[instrument]]`
    table, but the loader read them from the instrument *plugin's* config, which such a lane has
    none of (reachable in the TUI, by `d` on the instrument of a lane that has effects, and by
    `AddModulator` on an empty lane). The session config now holds a lane's modulators on the slot
    (`InstrumentSlotConfig.modulators`) and not on the plugin, with a test of the loader and one of
    a save and a reload, each seen to fail first. A cable of such a lane that was on the cleared
    instrument's own parameters (slot 0) has no parameter to land on, and the load skips it.
  - **Fixed after the step: f29.** A port that is taken (or an address beyond loopback that
    the policy refuses) is found out before the session is loaded, in `tang serve` and in
    `tang --web`: `web::preflight` tries the address and lets it go. Tests: the check and
    the server's own refusal say the same words, and the check does not keep the port; the
    real program, given a taken port and a session that is not there, says the port first
    (it said the session before); six mutations, all caught.
  - **The review of step 15** (an independent agent, read-only, on its own worktree; each of its
    ten findings was checked against the code before anything was changed) found one serious bug
    and several that a person meets with a keyboard or in a real browser and the browser tests had
    not: **f54, taking a cable or a modulator out left what it drove at the value the cable last
    wrote**: the plugin kept 0.7 where the knob and the model said 0.5, until someone touched the
    knob (the TUI's `d` had always given the value back, and the browser's "×" was the new way to meet
    it). The controller gives the user's value back now for whatever the removed cables drove and
    nothing else drives (`Driven`, `SessionModel::driven_by`; a first attempt in the audio thread was
    dropped, as it wrote one cable's value into another's place). **f55**, the TUI addresses its tree
    by position, so a node that a browser added or took out above its cursor left the cursor, a value
    being typed and the open popups on whatever had come to their place: the cursor follows its node
    (by id and kind of entry), and a popup that holds a place is closed when the tree changes. **f56**,
    a cable let go over the lower half of a knob drew nothing, and at the foot of a cell went to the
    knob below. **f57**, Tab out of the "+" menu went to the end of the page. **f58**, the focus fell
    to the page after the "×" on a cable. **f59**, two nodes could have one name on the canvas.
    **f61**, CLAUDE.md said the pusher was not built. **f62**, a native select draws a cable on every
    arrow key on Windows and Linux (suspected; macOS opens the list instead, so it could not be seen
    here): the list is a menu now, with a heading for each plugin, a height limit and a letter that finds
    a parameter. **f60** (the mock's telemetry layout is not the controller's: harmless, open, low),
    and one that was true and had been fixed already (f49). Its own checks of 360 random sequences
    of intents, the order of the protocol, the reach rule and the layout found nothing. Each fix has
    its test, seen to fail first.
  - **Found by me while fixing them.** f63: the loader centred a cable from one modulator to another's
    rate, stage or depth on a default and not on what the file gives that field, so a saved session
    came back with an LFO at 2 Hz swept around 1 Hz (fixed). Open, for phase 1: **f64**, loading a preset
    may leave the audio thread's base values of the cables stale (from reading `load_preset`, not
    tested: it did, and step 21 fixed it), and **f65**, a lane that leaves a group may leave what the group's cables drove at their
    last value (the same class as f54, not tested: the TUI's `g` is not an intent yet). f52 (a flaky
    browser test of the stress scene) and f53 (the TUI's piano), found in step 16, are fixed.
  - **Found by me on the way, open: replacing a lane's instrument keeps the cables on the old one.**
    `Controller::set_instrument` swaps the plugin and leaves the lane's modulators as they were, so a
    cable on slot 0 keeps the old plugin's parameter *index*, range and label: with a different
    plugin it drives whatever has that index there, until it is drawn again (the TUI's `i` does this
    today). Phase 1's replace-instrument intent has to decide: re-resolve a cable by the parameter's
    name, or take the cable out. (Step 20.1 took them out: fixed.)

- [x] **16. The keys of the computer play in the browser (a first piece of phase 2, ahead of
  phase 1, D18).** The page could build modulation but not make a sound: a note came only from a
  MIDI keyboard, so a person without one could not try what they drew by ear. The page now plays
  the TUI's tracker layout with the keys of the computer. *Done when:* in a browser on the real
  program, holding a key raises the master's meter and letting go lowers it, and the octave keys
  change what a key plays.
  - **As built.** `web/src/client/keyboard.ts` (framework-agnostic, like the rest of the client
    layer): `noteOf(code, octave)` is the layout of `src/piano.rs` by the *physical* key
    (`KeyboardEvent.code`: Z S X D C V G B H N J M , L . ; / from the octave's C and Q 2 W 3 E R 5 T
    6 Y 7 U I 9 O 0 P an octave above), `[` and `]` change the octave from 0 to 8, and
    `KeyboardPiano` turns key events into `noteOn` (velocity 100, as the TUI's) and `noteOff`. It
    remembers the note each key began and ends *that* one (the octave can change while a key is held),
    counts the keys that hold a note (two keys can come to the same one), plays a key that repeats
    once, leaves keys with Control, Command or Alt to the browser, and leaves every key to a place
    that takes text (a field, a text area, a list, anything editable). `app/useKeyboardPiano.ts`
    listens on the window (a key pressed with the focus on a knob or a button is the piano's too),
    lets go of what is held when the window loses the focus or the page is hidden, and when it is
    taken off; the header says `Keys: octave 4`.
  - **Tests.** 27 Vitest tests (the layout key by key against CLAUDE.md's table; the octave range
    and the notes MIDI can hold; every case above; the page's four; the hook's five over a client that
    writes down what it is asked, which includes that every listener put on the window and the
    document is taken away again) and a browser test on the real program (the null backend: the
    master's meter is below -50 dB, rises while `z` is down, falls when it is up, and the same at the
    next octave after `]`). 35 mutations of the module, the hook and the page: 31 caught at once, four
    that were not and showed a real gap (a key that repeats counted twice would leave its note on
    for ever, and the test looked only at the first `noteOn`; a `contenteditable="false"` element
    was taken for a place that takes text; and two listeners that were not taken away again), each
    now tested and caught. One mutation of the browser test survived and is not a gap: the
    oscillator does not scale its level with the velocity, so the meter cannot tell it (the
    velocity is checked in the unit tests).
  - **Found on the way (f53), fixed after the step.** The TUI's virtual piano (`src/piano.rs`) had the
    bug the page's piano was written not to have: a key was released as the note it *now* maps to,
    so a key held while `[` or `]` was pressed ended a different note than it began and left the
    first one on, and `all_notes_off` did the same (the existing octave test did exactly this and
    threw away what it sent). It remembers the note each held key began now, and a key that is not
    down (no press, or one that Locked mode refused) ends nothing: a note-off for it would have
    ended a note a MIDI keyboard was holding. Three tests first, all failing before; six mutations,
    all caught; 767 Rust tests in the default build and 911 with the web feature. The TUI cannot play from the computer keyboard outside `play` at all (CLAUDE.md's
    TODO of the Piano tab), so in `tang --web` the browser is the way to play with the keys.
  - **Not done.** An on-screen keyboard (a touch screen); showing the notes that are held and the
    piano's scale in the page; chords; BPM; Web MIDI. The keys are named by position, as a US
    keyboard has them, not by what is printed on them: on another layout `;` is the key right
    of `L` and `/` the key right of `.`, whatever they print there. By ear, on a real device,
    nothing was checked beyond the meter.

- [x] **17. The controller takes things out of the chains (phase 1, first piece).** The TUI's
  `d` on an effect, a bus effect, a lane or a group did its work by itself: it sent the engine's
  removal and dropped the modulator targets that pointed at what went with `retain` (the audio
  thread did the same), which left the cables that drive the depth of a cable that is gone pointing at
  nothing, and, when a group was removed, left its members' parameters at the last value the group's
  cables had written (f65). `Controller::remove_effect`, `remove_bus_effect`, `remove_lane` and
  `remove_group` do it now, and the TUI asks them. *Done when:* each leaves the model and a real
  audio graph with the same lanes, groups, chains and cables, whatever is removed in whatever order,
  and nothing points at nothing.
  - **As built.** Every removal takes the cables that point at what goes out first, one at a time and
    by the path a cable always goes (`take_out`: the engine is sent `RemoveModTarget`s in the order
    the model makes them, and the cables that drive the depth of one that goes go before it): for a
    plugin, the cables that drive its parameters (`SessionModel::cables_into_plugin`: a lane's own,
    and a group's to a member's plugin or to a bus effect); for a lane, its group's cables to its
    plugins and its modulators (`group_cables_into_lane`; its own modulators' cables go with it, a
    lane's modulator reaching only its own chain); for a group, all of its cables. Then the plugins
    are handed back by the audio thread and the model drops them (`SessionModel::remove_bus_effect`,
    `remove_lane` and `remove_group` mirror the audio thread's `RemoveGroupEffect`,
    `RemoveInstrument` and `RemoveGroup`, which renumber what comes after). What the cables drove
    goes with the plugin or the lane, so nothing is given back, except what a group's cables drove
    in the lanes that stay: a member's parameters, its modulators' rates and stages and the depths of
    their cables have the value the user set again (f54's `give_back`), unless a cable of the lane's
    own still drives them. A session keeps a lane: the last is not taken out.
  - **Tests.** A scene with a cable of every kind that can end at a plugin and cables that drive the
    depth of those (`testutil::CHAINS`), compared with a real audio graph (`AudioGraph::effect_names`
    and `groups` for it): every removal on its own, in sequence and in both orders, over four scenes,
    leaves the same lanes, groups, chains and cables and no cable pointing at nothing; the commands of
    four removals are pinned (the depth cable goes before the cable it drives, then the group's, then
    the plugin; what a removed group's cables drove is given back and what its members' own cables
    drive is not); after a group goes, a member's detune and the rate of its LFO are what the user set,
    in the audio thread. 39 mutations of the code written, 38 caught, and the one that is not is equivalent.
  - **Not done.** The intent: `RemoveNode` still refuses a node that is not a modulator, since the
    browser has no button for one and no undo (a click on a "×" there would be final), and the mock
    engine would have to derive the audio path to do the same. `clear_instrument` (the first `d` on an
    instrument) leaves the cables to the plugin's parameters pointing at nothing (f66, with f51: what
    to do with them when an instrument is replaced). (Both are settled in step 20.1: the cables are taken out.)

- [x] **18. Saving from the browser (phase 1).** The page could build modulation and play it, but
  nothing it did was kept unless the TUI was there to press `Ctrl+S`. *Done when:* on the real
  program, a change made in the page is written to the session's file by a button and by
  `Control+S` (`Command+S`), and the page says what is unsaved.
  - **As built.** `Intent::Save` (§5) and the event `Saved { file }`, the error code `io`
    (`IntentError::Io`), `Controller::session_file_name` (the snapshot's `meta.file` and the event
    use it). `Snapshot::apply` and the web client's `applyEvent` follow the rules of the controller
    (a save is no change of a node; nothing is unsaved after it, and any other intent leaves the
    session unsaved). A save that the TUI makes reaches the browsers as a snapshot at a revision of its
    own (`resync`: the comparison includes the meta). `Editor.save()`; the mock engine saves, and
    refuses a session with no file with the controller's words. `app/SessionBar.tsx`: the file's name,
    "Saved" or "Unsaved changes" (a polite live region, not a `role="status"`, which the connection's
    is), a "Save" button that is disabled while there is nothing to save or it is saving, and the
    chord, which is listened to on the window and keeps the browser from offering to save the page
    even when there is nothing to save; Shift+Control+S is left alone (Save As is the TUI's, and the
    page's when there is a validated name for it, §8).
  - **Tests.** Rust: the intent over a file in a temporary directory (the file is the session as it
    is; a session with no file, and one whose file cannot be written, are refused, with no revision
    and the session still unsaved), the event as a subscriber hears it, the snapshot that follows the
    controller through a save, a save the TUI makes told as a snapshot, the TUI's tab (the mark goes
    when a browser saves) and the real program (a client saves and the file has the change; a client
    that arrives after is given a clean session). Web: 27 mutations of the reducer, the mock, the
    editor and the bar, 24 caught at once and three that were not and showed gaps, covered now (a key
    that is not S, a save of a clean session, the text of a refusal); 22 Vitest tests (708 in all); a browser test on the
    real program, which serves a copy of the tracer (`web/.e2e-session`, so that a test that saves
    does not write the example).
  - **Not done.** Save As (a validated file name for both front-ends, §8); the page does not warn
    before it is closed with changes that are not saved.

- [x] **19. Effects from the browser (phase 1).** The page could build modulation, and take
  things out, but the chains themselves it could not touch: it did not even know what is installed.
  *Done when:* on the real program, a browser puts an effect that is installed into a lane and into a
  group, hears the difference, takes it out again, and the file the session is saved to has what it did.
  In parts, each its own commit: **19.1** the catalog on the wire (and f67, fixed first); **19.2** `AddEffect`:
  in the controller, which the TUI's selector uses too (2a), and as an intent with the client's `Editor`
  and the mock engine (2b); **19.4** the "fx" menu on a lane and a group; **19.5** `RemoveNode` for effects and
  bus effects, the mock's version of it and a "×" on every effect. (There was no 19.3: the client and the
  mock came with the intent.) The browser test on the real program is in 2b (a filter put in the chain and
  heard in the meter) and 19.5 (a reverb put in from the menu, saved, taken out, saved). *Not done:* an
  undo, and instruments, presets, ranges and groups, which the following steps are.
  - **19.1 as built.** `ServerMessage::Catalog { rev, scanning, entries }` (§7) and the
    `CatalogEntry` type, generated for the client like the others; `Catalog::rev` (goes up with each
    batch of plugins), `entries` and `ensure_scan` (`tang serve` starts the scan, since there is no TUI
    to; the TUI starts its own, and a second one would load every plugin twice). The owner tells a
    subscribed client the catalog right after its snapshot and tells all of them again when
    `(rev, scanning)` has changed since it last did (`Owner::refresh`, which the headless loop and the
    TUI's loop call every turn). The client keeps it in its store (`ClientState.catalog`, null until
    the server says; the one of the connection before is kept while a new one is made good, and the new
    server says it again) and the mock engine serves the built-ins (`web/src/client/fixtures/
    builtins.catalog.json`, written by Rust: `the_web_clients_fixture_of_the_builtins_is_current`) and
    can announce more (`MockServer.announceCatalog`).
  - **19.1 tests.** Rust: the entry's JSON, with a plugin whose path is a secret and a test that
    the secret does not come out; the revision (a batch is one, an empty batch none); `ensure_scan`
    leaves a scan that is under way, or done, alone (what arrives from the scan that was under way
    still arrives); the owner's tell-with-the-snapshot, tell-again-as-it-grows (including the end of the
    scan, which adds nothing), tell-nothing-more-when-nothing-changed and not-to-a-client-that-has-not-
    subscribed; the real program (`tests/serve.rs`: the built-ins come, with no key but the six, and
    none of the machine's paths). Eight tests of the web runtime and the TUI that took the snapshot
    and then expected an event now take the catalog in between, through one helper
    (`testclient::subscribe`). Web: seven Vitest tests (nothing until the server says; the server's
    own catalog and whether it is scanning; each telling replaces the one before; the session is left
    alone; kept over a reconnection and told again; not told to a connection that has not subscribed;
    entries that pile up if they are not replaced). 26 mutations (12 in the client and the mock, 14 in
    the Rust), 25 caught at once; the one that was not showed that nothing noticed a new scan
    taking the place of one under way, and the test now sends a batch down the old channel.
  - **19.2 (first part) as built.** `Controller::add_effect(inst, id)` and `add_bus_effect(group, id)`
    put a plugin at the end of a lane's chain or a group's bus, as the controller's own
    methods, which the TUI's selector asks now (it did the loading, the command and the model by
    itself); the intent that wraps them is the next part. The plugin is loaded here, and only from the
    catalog: an id or a path the catalog does not name is not loaded, however it is spelled, and an
    instrument is not an effect. It gets a node of its own, and the mix that suits it
    (`default_effect_mix`: a filter fully wet, the rest half). The chains are stereo, a constant now
    (`plugin::chain::CHAIN_CHANNELS`), and an effect with other outputs is refused before the audio thread
    is asked: the audio thread turns it away (it logs why and hands the plugin back) and the TUI's model
    kept the effect all the same, which is f68 (below) for what is left of it. Why a plugin did not load
    goes to the log, not to the client: what a loader says can name where a plugin is on this machine. A
    refusal changes nothing, not even the ids (the node is made last), and the TUI leaves the session
    unmarked.
  - **19.2 (first part) tests.** Nine in the controller (the end of a lane's chain and of a bus, in the
    model and in the commands, with the node it returns; the chains and the cables that were there are as
    they were, and the engine is told nothing but the insertion; the model and a real audio graph agree
    after each of an effect of each kind in every lane and bus of three scenes, and again after taking them
    out from the end; an id or a path the catalog does not name, an instrument, a plugin that the catalog
    names and that does not load (what the error says names no path), a stub with one output (turned away
    before anything is sent, with no id used), a lane or a group that is not there), and two in the TUI
    (a plugin that has gone leaves the tree, the commands and the unsaved mark as they were, for a lane
    and for a bus). 24 mutations, 20 caught at once; two that were not were equivalent (a check that the
    next function makes again: removed) and two showed that nothing pinned the TUI's refusal (covered,
    for a lane and for a bus).
  - **19.2 (second part) as built.** `Intent::AddEffect { parent, plugin }` (§5) wraps the two methods, with
    the node of a lane or of a group; a change of structure, so the answer is a snapshot, like the four of step
    15. In the TUI's loop a browser's effect is in the tree at once and the cursor stays on its node. The page:
    `Editor.addEffect(parent, plugin)` (the node it made is the last effect of the parent that the session did
    not have before; a refusal says "Could not add Reverb to Oscillator: …", the plugin by the catalog's
    name), and the mock engine does what the controller does: the catalog's gate, the order of the nodes of a
    strip (the lane or group, its effects, its modulators), the audio path through the new node (the link that
    left the chain, to the master or, for a lane in a group, to the group, leaves from it now), the mix it starts at,
    and a node of the controller's own making for each built-in effect (`builtins.effects.json`, written by Rust,
    `the_web_clients_fixture_of_the_builtin_effects_is_current`: the controller adds each to a lane and the
    fixture is what it made, without the ids). A plugin the catalog names and the mock has no node for (a
    VST3, say) is refused as one that does not load.
  - **19.2 (second part) tests.** Rust: the intent over the stub engine (a lane's chain, a group's bus, a lane
    with no instrument; the graph has the node in the lane's strip and the audio path through it; a cable can be
    drawn to it at once, and the engine addresses it by the right slot; refused for an effect, a bus effect, a
    modulator, a node that is not there, the master, an instrument, and a path and an id the catalog does not name,
    with nothing changed), the wire, and in the TUI's loop a browser that adds an effect (the tree, the browser's
    snapshot and the refusal's words) and one that adds it above the cursor. Web: nine on the mock (the order of
    the nodes, the audio path, a member lane's link to its group, an empty lane, a cable at once, the ids, every
    refusal, the other clients, the fixture's agreement with the catalog's) and five on the editor; and on the real
    program a browser puts a second filter at the end of the lane's chain, closes its cutoff down and hears, in
    the master's meter, that the note is hardly there (the meter falls 40 dB a second, so two seconds on it shows
    what is there), and the file it saves has both filters and the cutoff. 31 mutations (22 in the mock and
    the editor, 6 in the intent, 3 in the engine's side of it), all caught.
  - **19.4 as built.** The "fx" menu on every lane and group (§9.3): `effectItems` (the catalog's effects, by
    format) and `AddEffectMenu`; a style for the button (`.tang-node__fx`) and a wrapper for the two buttons of a
    header (`.tang-node__actions`). Checked in a browser on the mock engine: the buttons, the menu with its
    heading, and the new node at the end of the chain with the audio cable through it.
  - **19.4 tests.** Four of `effectItems` (instruments left out, the formats in order and the server's order within
    them, a format this page does not know of last, the list given left alone) and eight of the patch (the button on
    every lane and group, named for whose it is, and on nothing else; the menu, by format, without the
    instruments; the menu following the scan while it is open; the request that is sent, the node that comes with
    its controls, the focus back on the button; the audio cable through the new node; a group's bus; a button with
    nothing to open; a refusal on the notice board by the catalog's name for the plugin). 16 mutations, all caught
    (one was equivalent as I first wrote it and was written again).
  - **19.5 as built.** `RemoveNode` takes an effect or a bus effect out, by the controller's `remove_effect` and
    `remove_bus_effect` (step 17: the cables that drive the plugin go first, one at a time and with what drives
    their depths; nothing is given back, since what they drove goes with the plugin), and says "only a modulator or
    an effect can be removed so far" for a lane or a group. The mock engine does what the controller does, the
    audio path closing up (the link into the effect and the link out of it are one, of the kind of the second:
    the link that ends a member lane's chain goes into its group). `Editor.removeNode` (it was `removeModulator`),
    and a "×" in the header of every effect, which the modulators' has become one with (`RemoveNodeButton`): the
    focus goes to the button that adds one like it to its lane or group ("fx" for an effect, "+" for a
    modulator). There is **no undo**: a click on a "×" is final, and an effect forgets its settings. Undo is
    planned for all the operations of this phase together, since each would need its own inverse (§5, "Undo / redo"):
    an effect would be loaded again from its id, its values and its mix put back, and the cables that were
    driving it re-drawn by the nodes of the modulators and the names of the parameters, since the ids of a new node
    and of new cables are new. Lanes and groups have no "×" until then.
  - **19.5 tests.** Rust: the intent over a lane's chain (the commands, in the order of the controller's own test of
    the removal: the cable that drives a cable's depth, the cable, the group's cable, the effect), a bus (the
    group's cable, the bus effect), the audio path closing up (the one in the middle, and the last of a member
    lane's chain, which ends in its group), an effect added and taken out leaving the graph as it was, taking an
    effect out twice (not found, no revision), and a lane, a group, a ghost and the master refused; in the TUI the
    cursor staying on its node when a browser takes an effect out above it, and the popups closing when it takes out
    the one the cursor was on. Web: seven on the mock (the tracer's filter with its cable, the layout of the
    telemetry, the audio path in a chain of three and in a member lane, a bus, the cables to it whoever's and
    what drove their depths, added and taken out, the other clients, the ids) and four of the page's buttons (an
    effect and a bus effect, the focus, a refusal); and in a real browser on the real program, a reverb put in
    from the menu, saved, taken out by its button and saved again. 25 mutations (19 in the page and the mock, 6 in
    the intent), all caught (one was equivalent, an error that could not be reached after the node had been found,
    and the code was removed).
  - **f68 (open).** A session file that names an effect with other outputs than the chain has loads, and
    its model keeps the effect that the audio thread dropped (the loader sends the command and pushes the
    slot without looking). CLAUDE.md has the TODO: the loader should refuse such a session.
  - **Found on the way (f67), fixed first, in its own commit.** The browser test that saves failed
    once `tang serve` scanned the machine's plugins: the file it read had not changed, though the
    program said it had saved. A plugin (a ROLI one, here) changes the working directory of the whole
    process while it is scanned, and the session's file was kept as it was given on the command line,
    relative, so the save went to `~/Library/Application Support/ROLI/ROLI Connect/.e2e-session/`,
    creating the directory. The TUI has always scanned in the background, so `tang session.toml` and
    `Ctrl+S` were exposed to the same thing. The session's file is an absolute path from the start now
    (`session::absolute_path`, taken before anything is loaded: the plugins a session names relative to
    its own directory are found by it too), and a test on the real program starts it in a directory with
    a relative name and reads which file it says it saved. Four mutations, all caught. (The directory
    the test runs had made in the ROLI folder was taken away.)

- [x] **20. Instruments from the browser (phase 1).** The page could put effects in a chain, but the
  instrument of a lane it could neither choose nor change. *Done when:* on the real program, a browser puts
  an instrument that is installed in a lane (replacing the one it has) and in a lane of its own, hears the
  difference, and the file the session is saved to has what it did. In parts, each its own commit: **20.1**
  the controller's `put_instrument`, which the TUI's selector asks (and the two findings it settles, f51 and
  f66); **20.2** `AddInstrument` and `ReplaceInstrument` as intents, with the client's `Editor` and the mock
  engine; **20.3** the menus on a lane and in the bar; **20.4** a browser test on the real program (in the
  commit of 20.3); **20.5** the canvas moving to what the page added, which the first look at 20.3 showed it
  needed. *Not done (later steps):* presets, ranges, groups, taking a lane out, undo.
  - **20.1 as built.** `Controller::put_instrument(inst, id)` puts an instrument in a lane, replacing the one
    it has (a lane that the TUI emptied with `d` is filled), as `add_effect` does an effect: loaded here, and
    only from the catalog (an id or a path the catalog does not name is not loaded, an effect is not an
    instrument), refused with nothing changed and nothing sent if the lane is not there, if the plugin does not
    load, or if it has no audio output (one is enough: the audio thread plays a mono instrument on the left).
    The plugin has the lane's own node, as the lane's instrument always had. **The cables that drove the old
    plugin's parameters, a lane's and a group's, go first**, each by `take_out` with what drives their depths:
    a cable names its parameter by the plugin's own *index*, so one left in place would drive whatever the new
    plugin has at that index (f51, found in step 15: the TUI's `i` did this). `clear_instrument` (the first `d`)
    takes them out too (f66: it left them pointing at a slot 0 that was not there). A **note remap** goes with
    the plugin: the audio thread drops the remapper when the plugin it was for goes, and the model said the lane
    still had one (the file saved would have had a remap the audio thread no longer had), so `set_instrument`
    and `clear_instrument` forget it (`InstrumentNode::forget_remap`: no notes, the usual pitch bend range). The
    TUI's `i` asks `put_instrument` now; its `n` (a new lane with an oscillator in it) still loads without the
    catalog, which may not have been scanned when the key is pressed. What a person of the TUI sees: a
    vibrato LFO on a lane's `detune` loses its cable when the instrument is replaced (it would have driven
    whichever parameter has that index in the new plugin); the modulator stays, ready to be drawn again.
    Not done: keeping the cable of a parameter the new plugin has under the same name (CLAUDE.md, a TODO).
  - **20.1 tests.** Twelve in the controller: an instrument put in a lane in the model and in the commands,
    the cables of the lane's own modulator and of the group's going first (`RemoveModTarget …` and
    `RemoveGroupModTarget …`, then `SwapInstrument`), the parameters the new plugin's own and the lane as it
    was; an empty lane filled, keeping its filter and volume; the remap (and the pitch bend range) forgotten, in the model and in
    the file; the model and a real audio graph agreeing after an instrument in each lane of four scenes; what
    is refused (no catalog at all, a path, an id and a name that the catalog does not have, an effect), a
    plugin the catalog names and that does not load (what the error says names no path), one with no audio
    output (before a cable is taken out), one with a single output (accepted), a lane that is not there; that
    no id is used up for the plugin; and `clear_instrument` taking the cables out first and the remap with it.
    In the TUI, one new golden test (the vibrato's cable goes, the other lane's stays) and three that pinned
    command lists (two of a replace, one of the two `d`s), which have the cable's removal in them now. 19 mutations of
    the code written, 16 caught at once; the three that were not showed a remap that `clear_instrument` kept, an
    instrument with exactly one output that nothing said was accepted, and an id used up for a plugin that has
    the lane's: covered. Rust: 861 tests (848 before), 1008 and 10 on the real program with the `web` feature.
  - **20.2 as built.** `Intent::AddInstrument { plugin }` (a lane at the end of the session, full range, in no
    group, with a meter of its own, by `Controller::add_instrument_lane`) and `Intent::ReplaceInstrument { node,
    plugin }` (by `put_instrument`; `node` is the lane's, which an empty lane has too), both changes of structure,
    so the answer is a snapshot (D20), and both refused with nothing changed (no lane made, no cable taken out, no
    revision) for a node that is no lane (`an instrument goes in a lane`: the node is looked for first), a plugin the
    catalog does not name, an effect (`Filter is an effect, not an instrument`), a plugin that does not load, and
    one with no audio output. The page's side: the generated types (and `NoteRange` exported), `Editor.addInstrument`
    (resolves with the node of the lane) and `Editor.replaceInstrument`, and the mock engine does what the
    controller does: the lane goes before the master in the graph, its link to the master after the links of the
    others and before the modulation cables, its peak and clip count at the end of the meters on slots that nothing
    has used and a layout of a new revision; a replace keeps the node, the range, the transposition and the volume,
    and takes out the cables that end at the lane with those that drive their depths and the slot of the value each
    drove (the same `without` as the removals). The node it makes for each built-in instrument is
    `web/src/client/fixtures/builtins.instruments.json`, which Rust writes
    (`the_web_clients_fixture_of_the_builtin_instruments_is_current`).
  - **20.2 tests.** Rust: seven on the intents over the stub engine (the engine's commands in order, the graph with its
    node, its link and its meters, a modulator and a cable on a lane that was just added, a replace that takes a
    group's cable out first, an empty lane filled, every refusal with the model compared whole), the wire (both
    names), four on the TUI with a browser (a lane at the end of the tree and the cursor staying on its node, the
    vibrato's cable gone from the tree, the selected row of a pane whose cable went, and a browser on the real loop that
    adds and replaces and is refused in the controller's words), and two on the real program: a lane added is heard in
    the master's meter (far louder than the tracer's quiet lane) and the file saved has both lanes; an instrument
    replaced after its volume was turned to nothing and a cable drawn on it, so that the same note is heard again,
    the cable is gone, and the file has no trace of the saw. 13 mutations of the Rust, all caught at once; six on the
    real program, five caught (the sixth changed what only the model has, which the unit tests pin). Web: 14 on the mock
    (every case above, the fixture's agreement with the catalog's included), 7 on the editor; 35 mutations of the client and the menus, all caught (one only by the type checker: a test now pins
    that the links of the audio path come before the cables).
  - **20.3 as built.** The catalog's instruments as the items of a menu (`instrumentItems`, in the order of
    `effectItems`: the built-ins first, then CLAP, LV2 and VST3, a heading for each), a "♪" button beside "fx" and "+"
    in the header of every lane (`ChangeInstrumentMenu`: "Change the instrument of Oscillator", and for an empty
    lane "Choose an instrument for (empty)"), which asks for `ReplaceInstrument`, and a "+ Instrument" button in the
    bar of the page (`app/AddInstrument.tsx`), which asks for `AddInstrument`. A refusal is among the notices, in the
    catalog's names for the plugins ("Could not change the instrument of Oscillator to Reverb: ..."); with nothing
    to choose from the button is disabled; the menu follows the scan while it is open.
  - **20.3 and 20.4 tests.** On the page: the button on every lane and on nothing else, the menu without the
    effects and following the scan, the request that is sent, the new instrument's controls on the lane that kept
    its node (the saw becomes a sine), an empty lane filled and renamed, the cables to the old instrument off the
    canvas, nothing to open, a refusal; the same for the bar. In a browser on the real program: the first lane's saw
    changed to a sine from its menu with its filter and its LFO still there, a lane added from the bar and heard,
    and the file with both lanes and without the saw. A lane cannot be taken out from the page, so this test starts a
    program of its own (`e2e/own.ts`: a copy of the tracer in a directory that is thrown away, and its log kept with a
    test that fails) instead of the shared one.
  - **20.5 as built.** A lane added from the bar went below the one there was, out of the view: nothing seemed to have
    happened (the old "the canvas does not move to show a node that was added"). The editor now says which node it
    added through `Reveals` (`client/reveals.ts`; the provider hands it to the editor and to the canvas), and the canvas
    (`ShowWhatWasAdded` in `PatchView`) takes the node's rectangle from the layout (`rectOf`) and, if it is not all
    in the view with a margin of 32 px, moves the view so that the node is in the middle of the screen, at the zoom the
    person has, or lower if it does not fit (`viewToShow`: never higher). It waits (three seconds at the most) for the
    layout that has the node, moves once, and only for what this page added: a node another front-end adds is left
    where it is, since the canvas moving under a person who is turning a knob is worse than a node out of sight. All
    three kinds of node the page adds are shown this way: a lane, an effect, a modulator.
  - **20.5 tests.** The arithmetic (thirteen: every edge and its margin to the pixel, the middle, the zoom out and its floor,
    never in, no size), the layout's rectangle, the editor's side, the provider's, the canvas's glue over a made-up size
    (seven: it moves, it does not when all is in the view, it waits for the layout, once, not for another front-end's, it
    gives up, no size) and the browser test above, which was red first (the new lane was 17% in the view). 29 mutations,
    all caught in the end (two were not: a screen with no width, and a canvas stand-in whose object changed every
    render, which hid that a new layout is looked at).
  - **Found on the way.** `examples/tracer.toml`, which the tests took for the known scene, was written over by a Save
    from the page of a person who ran the program on it (f69: fixed, the tests have a copy of their own,
    `web/src/client/fixtures/tracer.toml`, and the saved scene is kept as `examples/cool-sound.toml`); and a bug I
    thought I had found, that a letter typed in a menu plays a note, was not one (the menu keeps its letters from the
    page: the test I wrote for it only failed when two defences were both taken away, so I took mine out).
  - **Numbers at the end of the step.** Rust 872 tests (848 before the step), 1020 with the `web` feature and 12 on the
    real program; web 818 (752 before); 38 in browsers (37).

- [x] **21. Presets from the browser (phase 1).** The TUI has loaded presets since step 6 (`p`: the `LoadPreset`
  intent), but the page offered none: a plugin that has presets could only be turned knob by knob. *Done when:* on the
  real program, a browser loads a preset into a plugin that has some, the knobs take its values, the menu marks it and
  the file the session is saved to names it. In parts, each its own commit: **21.0** f64, which the engine's `LoadPreset`
  had (below), fixed first, and shown on the real program; **21.1** the editor's `loadPreset` and the mock engine's,
  with what the controller says each built-in preset does as a fixture that Rust writes; **21.2** the "≡" menu on a
  node, with a browser test. *Not done (later steps):* a preset for an effect on a group's bus (the engine), a filter
  for a menu of hundreds of presets, the ranges and groups of a lane, undo.
  - **21.0, f64 fixed first.** The finding of step 15 was true. A cable modulates around its base value and puts the
    result on its parameter at every buffer; `SetParameter` refreshes the base and the engine's `LoadPreset` did not,
    so the plugin took the preset's values and the first buffer after it the cables that drive a parameter put the old
    base back (plus their offset): a preset was heard for everything but what was modulated, while the model, and so
    the page, showed the preset's value. After the plugin has loaded the preset, the cables of the lane's own
    modulators that drive that slot, and those of the group's modulators that drive it as a member
    (`update_group_member_base`), take the value the plugin reports (`b1e7adf`). Two tests over a real audio graph
    (after "arcadia" the reverb's damping was 0.30 and both cables on it were centred on 0.35), and six mutations, all
    caught; and one on the real program (`tests/serve.rs`): a reverb in the tracer's lane with the lane's LFO on its
    decay at a depth of 0.1, the "room" preset (decay 0.55) loaded, and the effective value in the modulation frames
    moving around 0.55 and not around the 0.88 it had, which the one mutation (the lane's cables left alone) fails. That test
    first failed once in a loaded run, with 0.969, the old centre at the LFO's peak: the first frame after an
    acknowledgement can be older than the audio thread's reading of the command, so the test throws away what the
    connection has already received before it reads.
  - **21.1 as built.** `Editor.loadPreset(node, preset)` asks for `LoadPreset` and resolves true when it was loaded; if the
    server refuses, a notice says what could not be done in the names the canvas shows ("Could not load Cathedral into
    Reverb: ..."), the preset by the name the plugin's own list gives it (its id if the list is not known). The mock
    engine does what `load_preset` does: it refuses a node that has no plugin, a bus effect, a lane with no instrument
    and a preset the plugin does not have, in the controller's words, and answers with the preset's name and the values
    the plugin has for its parameters after it. For a built-in plugin they are the controller's own, a fixture Rust
    writes (`web/src/client/fixtures/builtins.presets.json`, kept current by
    `the_web_clients_fixture_of_the_builtin_presets_is_current`; only the reverb has presets: five, ten values each); for
    a plugin of the machine's the mock knows none and says so. Until now the mock said no values at all, so a page that
    shows what a preset does could not be tested against it.
  - **21.2 as built.** The "≡" menu of §9.3 (`PresetMenu`, `MenuItem.current`, `presetsOf`): a selector that says which
    presets a node's plugin can be given (an instrument's, a lane's effect's; none for a node with no plugin or a bus
    effect), with the one loaded last by its name; the menu is a `MenuButton` that marks the current item and opens on it
    whatever its place, an arrow key opens it at the arrow's own end as before. A refusal is among the notices.
  - **Tests.** Web: nine on the mock engine and the editor (each refusal, the values, the name, the other clients being
    told, a plugin the mock has no values for, the fixture's own keys), six on the selector, five on the menu with a
    current item, seven on the canvas (the button on what has presets and on nothing else, the menu in the plugin's order,
    the request, the mark and where it opens, a preset another front-end loads, a refusal); 18 mutations of the client and the menu,
    all caught. One in a browser on the real program (a program of its own, as step 20's): a reverb is put in the lane, "Small
    Room" is loaded from the menu and its decay, size and pre-delay are what the preset says, the menu marks it and opens on it,
    "Cathedral" is loaded by the keyboard alone, and the file saved names it and keeps none of its values as the user's own.
  - **Found on the way.** f64 (above). A test of step 20 (a lane that could not be added: the page's notice) failed in two
    full runs on a machine with a load of 20 and never alone: its client reconnects after 5 ms, as the other tests of the page
    do, and a click that came late found the connection up again and the lane added; a click delayed by 50 ms failed it every
    time, and its client waits a minute now (`09d6c0e`). CLAUDE.md said the TUI's `p` on a group's bus effect only lacked a key
    ("the `LoadPreset` intent already reaches those"); the controller refuses it, as above (the TODO says so now).
  - **Numbers at the end of the step.** Rust 875 tests (872 before the step), 1023 with the `web` feature and 13 on the
    real program (12); web 845 (818 before); 39 in browsers (38).

- [x] **22. All the parameters of a plugin from the page (phase 1).** A node shows the first eight parameters of its
  plugin as knobs, so the rest could not be set by hand: found by trying the page against this machine's plugins, which
  are not the built-ins (below). *Done when:* on the real program, a browser finds a parameter that its node has no knob
  for by typing some of its name, turns it, and the file the session is saved to has it. In parts, each its own commit:
  **22.1** finding a parameter by name and the list of a plugin's parameters (`matchParams`, `paramListOf`,
  `paramCountOf`); **22.2** a parameter as a row of a list (`ParamRow`); **22.3** the "⚙" button and the panel, with a
  browser test. *Not done (later steps):* pinning a parameter to the node (it needs the `ui` layout of §4.4), a slider
  that follows a parameter's steps and groups.
  - **The check that led to it.** The page had only been tried on the built-ins and a copy of the tracer. This machine has
    two CLAP synths of the user's own (an Acid 303 with ten parameters and a sine synth with six) and four VST3
    instruments (Pianoteq 9 with 2,202 parameters listed, the ones it marks "(locked)" being hidden as in the TUI, and 128 presets; two ROLI players; Traveler with 2,450), so the page was run
    against them, on the null audio backend: the Acid 303 comes into a lane from the "♪" menu in about 0.1 s and looks like
    any node (the "≡" is not on it: it has no presets), Pianoteq 9 in about 0.8 s with a snapshot of 302 KB, and neither
    makes a long task in the browser or a line in its console; the preset menu of Pianoteq (128 "Program N") opens in
    under 50 ms, on the preset that was loaded when it is 100 entries down (the item is scrolled into view), and the
    list that draws a cable has 2,205 items and opens in 55 ms. What was wrong: a node has eight knobs, so the Acid 303's
    "Slide Time" and "Tuning" and nearly everything of Pianoteq's could not be turned by hand (a cable to one pins it,
    which is a cable, not a setting).
  - **22.1 as built.** `matchParams(params, query)`: the parameters whose name has every word of the query, whatever the case
    and wherever in the name (as the TUI's `/`, which takes one word), as the plugin's own indices in order; the
    words are text, never a pattern. `paramListOf(state, node)`: every parameter of an instrument's plugin or any effect's
    (a group's bus included), by name and own index, with the name the canvas gives the node and the plugin's format, and no
    values, so that `sameParamList` finds it the same while they change (a plugin that numbers its parameters with gaps,
    as Pianoteq does, is listed by its own numbers: the row's index is what the intent names). `paramCountOf`: how
    many, for what only asks whether there are any.
  - **22.2 as built.** `ParamRow` (`app/patch/controls.tsx`): the whole name (the tooltip has it when it does not fit),
    a native slider over the parameter's range with no step of its own (`step="any"`: the browser's step on a slider
    with none is the whole range) and the value in its unit, a choice for named values, and, while a cable drives
    the parameter, a `LiveBar` ("<name>, as modulated") drawn on the frame from the same telemetry slot as the knob's dot.
    Turned by the same `Control` as a knob: shown as set until the engine has answered, one intent a frame, a gesture's
    end at once (the pointer up, the focus lost, a key, a choice); the keys are the knob's (`afterKey`) and kept from the
    browser; a double click goes back to the default. What a knob and a row both read of a parameter is `useParam` now.
  - **22.3 as built.** §9.3 above. `PatchView` holds the node in the panel and `PluginNode` asks for it through
    `InspectorContext`; `ParamPanel` renders a page of rows (forty, so a plugin with thousands costs forty), keyed by
    the plugin's own index, and closes itself (asks `onClose(false)`: no focus to give back) when its node, or
    its plugin, is gone.
  - **Tests.** Web: six on the search, nine on the list and the count, twenty on the row (in the order of the
    knob's: range, unit, choice, nothing for what is not there, somebody else's change, shown at once and sent on the
    frame, the end of a gesture, not pulled away, the keys and those that are not its own, the double click, the bar and
    when it comes and goes, no render for telemetry or for another parameter), twenty-one on the canvas (the button
    where it is and is not, the list, the controls and their agreement with the knobs, the reverb's last two, a plugin with
    gaps in its numbers, the filter, paging, closing four ways, one panel at a time, a panel whose node is gone), one that
    typing in the filter plays no note; in a browser on the real program, one: a reverb is put in the lane, "lowcut" is
    found by typing "low", turned by the keyboard to 500 Hz, and the file saved has it. 56 mutations of the code of the step,
    all caught in the end (five were not at first: a plugin whose indices are not its places, a longer list taken for the
    same, a key that waited for the frame, the tooltip, blanks taken for a filter).
  - **Found on the way.** `Param` has a `group` and a `stepCount` that the page does not use: the plugins of this machine
    report neither (the Acid 303's waveform is a number between 0 and 1 with no steps, Pianoteq has no groups), so
    it was left as a TODO rather than built blind. The panel does not move the canvas: its nodes that are under it are
    reached by panning.
  - **Numbers at the end of the step.** Rust 875 tests (as at the end of step 21, 1023 with the `web` feature and 13 on the
    real program); web 902 (845 before the step); 40 in browsers (39).

- [x] **23. Key ranges from the page (phase 1).** A lane takes every note unless it is given a range, and the page could
  not give one: a split or a layer was a thing for the TUI's `R`. *Done when:* on the real program, a browser chooses the
  keys of a lane, a note outside them is silent and one inside is heard, and the file the session is saved to has them. In
  parts, each its own commit: **23.1** `SetRange` as an intent, which the TUI's `R` asks too, with the client's reducer,
  the mock engine, `Editor.setRange` and the names of notes; **23.2** the Keys cell and a browser test.
  *Not done (later steps):* groups (the TUI's `g` is not an intent yet), taking a lane out.
  - **23.1 as built.** `Intent::SetRange { node, range: Option<NoteRange> }` and `Event::RangeSet`. `Controller::set_range`
    does what the popup did (the model's range, the audio thread's `SetInstrumentRange`, which lets go of the notes it
    counted under the old range) after the checks the popup did not need: the node is a lane, the ends are notes (0 to
    127) and the low one is not above the high one. The snapshot a client keeps applies the event
    (`Snapshot::change`, which the invariant test `the_events_of_an_intent_are_everything_it_changed` pins), and the
    TUI's loop rebuilds its tree for it (the lane's label has the keys). The TUI's `R` asks the intent now, and its
    golden test, which pins the command the audio thread gets, did not change. On the page: the reducer, the mock engine
    (the controller's rule and words), `Editor.setRange` and `client/notes.ts` (`noteName`: sharps, `C4` is 60, `C-1` is 0;
    `NOTE_NAMES`; `rangeLabel`: `C2-C6`, `-B3`, `C4-`, `all keys`), which the notice uses: "Could not set the keys of
    Oscillator to F#6-E2: ...".
  - **23.2 as built.** §9.3 above. `RangeCell`: two native choices, labelled `Lowest key of <lane>` and `Highest key of <lane>`
    (and a tooltip each), in a cell called Keys. The choice shows at once through a state of its own (`pending`) and not
    through `Drive`: a choice is one request, not a stream; a counter of requests keeps the answer to an earlier choice from
    clearing a later one (a key held down on a closed choice makes many a second). It is drawn for every node and is
    nothing for one that is no lane.
  - **Tests.** Rust: five on the controller, the intent and the event on the wire (two intents, two events), three
    more intents in the snapshot invariant, one on the TUI's loop with a browser, and on the real program one: a note
    below the lane's keys is silent in the master's meter (the peak a moment after, below 0.001) and one inside is heard,
    the file saved says `range = "C4-C5"`, none is every note again and the file has no range, a range that is not
    one is refused in the controller's words. 14 mutations of the Rust, all caught, and three of them by the
    real-program test alone. Web: 36 (902 before the step): the reducer, the mock engine, the editor, the notes, the
    selector, and fourteen on the canvas, among them the held-back answers (`holdable`: a connection whose messages
    to the client are let through a few at a time); in a browser on the real program one: C4 to B4 are chosen on the
    node, the computer's Z (a C below them) makes nothing of the master's meter and its Q (C4) does, the file has the keys,
    and any at both ends is every note again. 32 mutations of the page, 28 caught at once; one test was missing (any at the
    high end) and was written, two are the same page (a value that has no option shows the list's first, which is any:
    the explicit `""` is for the reader), and one was a guard the cell has anyway, taken out.
  - **Found on the way.** The shared test rig reconnected after a quarter of a second, and a test of the canvas that drops
    the connection and then clicks failed once in a full run (the canvas takes longer to draw with every control a node
    has): the rig keeps a dropped connection down now (`6bf6795`; a click 400 ms late failed it every time before and
    passes now). My first refusal for a range with its ends crossed named the notes the wrong way round; the test
    for it said so before it was committed.
  - **Numbers at the end of the step.** Rust 880 tests (875 before the step), 1029 with the `web` feature and 14 on the
    real program (13); web 938 (902); 41 in browsers (40).

- [x] **24. The piano in the page (phase 2, ahead of phase 1's end).** The TUI's Piano tab was the one view of the
  session the page did not have: the notes that are held, the scale, the lock. *Done when:* on the real program, a browser
  plays notes with the pointer on a keyboard under the canvas, the keys show what is held (by the page, by a MIDI keyboard,
  by another browser), the scale colours them and is chosen on the page, and a lock to the scale drops the notes outside it,
  whoever plays them; the file the session is saved to has the scale and the lock. In parts, each its own commit: **24.1**
  the held notes as a telemetry channel (the pusher, the client's buffers, the mock engine); **24.2** `SetScale` and
  `SetLocked` as intents, which the TUI's `k` and `l` ask too, with the roots and the kinds of scale in the generated
  constants; **24.3** the piano. *Not done (later steps):* groups (the TUI's `g` is not an intent yet), taking a lane or a
  group out with a confirmation (there is no undo), the nodes' layout (`ui` tables, pinning), undo; and on the piano the
  chord readout, the scale strip and BPM of the TUI's tab (§9.5).
  - **24.1 as built.** §6 above: `TelemetryChannel::Held` (kind 2): 128 values, 1 for a held note, no layout, every tick, from
    `NoteInput::held()` (the same bits the Piano tab reads); `web::pusher` makes a frame for each of three channels. The
    client's `TelemetryBuffers` has a 128-value array from the start that a layout does not reset, takes a frame of it at
    any time if it is 128 long, and says `isHeld(note)`; the mock engine tells the notes that any of its connections holds; the
    page asks for the channel (`createConnection`). A real-program test has a browser hear a note that another holds.
  - **24.2 as built.** §5 above: `Intent::SetScale { scale }`, `SetLocked { locked }`, `Event::ScaleSet { scale, root, kind,
    notes }`, `LockedSet { locked }`; `PianoMeta` in the snapshot has `scale`, `root`, `kind`, `notes` and `locked`; the controller
    sets the shared `PianoFilter` (what the note senders read) and the TUI's picker and lock key ask it (a revision and a change
    to save, as every intent; the scale that is set chosen again is no change). `ScaleSetting` says its pitch classes
    (`pitch_classes`, lowest first), its root name and its kind. `constants.ts` has `SCALE_ROOTS` and `SCALE_KINDS`
    (the long name, the short name and the intervals of each kind). On the page: the reducer takes the two events, `Editor.setScale`
    and `setLocked` (notices say what could not be done), and the mock engine reads a scale as the controller does
    (`client/scales.ts`: flats, long names, any case), from the intervals of the constants. A real-program test sets a scale
    and the lock from a browser, plays a note outside the scale (silent and never held) and one inside (heard and held), saves (the
    file has `[piano]`, the scale and `locked = true`), unlocks, and is refused a text that is not a scale.
  - **24.3 as built.** §9.5 above. Besides the piano: `NoteHolds` (`client/holds.ts`, the provider's `useNotes`), through which
    the computer's keys and the pointer play, so that a note both hold ends when both let go; the client forgets which notes are
    held when its connection is lost; the shared test rig has `holdable` (a connection whose answers can be held back), which the
    keys cell's tests had of their own; the tests' jsdom has a hit test that finds nothing.
  - **Tests.** Rust: 13 more tests (893 against 880; with the `web` feature 17 more, 1046 against 1029, and two more on the real
    program, 16 against 14): four on the scale (text that is not a scale, the pitch classes of every root and kind, the names),
    six on the controller (the two intents, what they refuse, the lock in the filter, the file), two on the TUI (the picker and the
    lock key are intents), one on the note path's held bits; and with `web` two on the generated constants and two on the pusher's
    held channel. The wire lists, the bad-input list and the snapshot invariant have the two intents and the two events. On the
    real program: the held notes that one browser plays and another sees, and the scale, the lock and the note that is dropped.
    37 mutations of the Rust (8 of the held channel, 29 of the scale and the lock), all caught against green baselines. Web: 100
    more tests (938 before the step): the held notes in the buffers and through the mock, the reducer, the editor, the scale reader,
    the layout, the pointer piano, the note holds, the provider, 33 of the component and 4 of the page; in a browser on the real
    program one: the keys, a key by the mouse and a glissando, the computer's keys, the window, the scale, the lock (a note outside
    the scale heard and lit unlocked, silent locked), and the file. 45 mutations of the held notes (15), their forgetting (5) and the
    scale and the lock (25) on the page, and 59 of the piano, all caught in the end; four were not at first, and a test was made
    for each: the revision in the header of the mock's held frame, the words the editor sends, the keys that are kept through a move
    of the window, and the root chosen after the kind.
  - **Found on the way.** `ScaleSetting::parse` panicked on text with a letter that is not ASCII where a root's name ends
    (`"Cé"`: four of the eleven texts of the test did, measured by running the old slicing on each): a session file with it took the
    program down at load, and `SetScale` would have let a browser do it (`fe14830`). The verification matrix found a dead-code
    clippy error in the builds without `web` (`NoteInput::held`), fixed before it was committed. The page that lost its server kept
    the keys lit that were down when it went (`c86944e`). The first screenshot of the piano showed the bottom of a node under it: the
    piano came after the canvas had been fitted, which is why its place is reserved from the start (§9.5). In the
    docs: §5 said `SetPianoMode` (built as `SetLocked`), §6 a bitset on change (built as 128 values, D27), and CLAUDE.md said the
    TUI paints a held key red (magenta on the root) while the code sinks a held key in the scale and paints red only one that is
    not in it; they say what the code does now, and so does a comment in `piano_view.rs`. A refactoring that is worth doing:
    `client/notes.ts` keeps its own list of the twelve names (`LETTERS`) that `SCALE_ROOTS` of the generated constants is.
  - **Numbers at the end of the step.** Rust 893 tests (880 at the end of step 23), 1046 with the `web` feature and 16 on the real
    program (1029 and 14), 888 without the default plugin formats and 1039 with `web`; web 1038 (938); 42 in browsers (41).

- [x] **25. Moving nodes (phase 1, ahead of the rest of it).** The canvas laid itself out, and a person who wanted a node
  elsewhere could not put it there. *Done when:* on the real program a browser drags a node and it stays; a second browser sees
  it where it was put, and so does a page that is opened again; the file the session is saved to has it as `ui = { x, y }` on the
  node's table; a program started on that file draws every node where it was left; and the TUI loads the places, shows nothing of
  them and keeps them through an edit and a save. In parts, each its own commit: **25.1** the places in the session file and in
  the model (Rust only); **25.2** `SetNodeUi` as an intent, with `Node.ui` in the graph, the reducer, the mock engine and
  `Editor.moveNode`; **25.3** the canvas: nodes that are dragged, the places that come from the session, and a control that
  arranges them again. *Not done (later steps):* pinning (`pinned`, §4.4), groups (the TUI's `g` is not an intent yet), taking a
  lane or a group out with a confirmation (there is no undo), cables to a modulator's own settings, undo; moving a node with the
  keyboard.
  - **25.1 as built.** §4.4 above. `session::NodeUi { x, y }` (f32, serde, ts-rs), read by `parse` into the config structs
    (`ui` on a group, a lane, an effect and a modulator, `master_ui` on the session) and written by `to_toml`, one inline line
    on the node's table (`inline_ui_tables`: the tables the serializer writes are made inline, and the key told to forget it was
    a header); a whole number is written without a point and a decimal as typed (`coordinate`). The model's `ui` is a side table
    by `NodeId`, `app::load` fills it, `to_save(&SessionModel)` (it took the lanes and the groups before) writes it, and
    `Controller::save_to` gives `session::save` the master's place. A bad `ui` is ignored with a log line; a number that is not
    a number is refused like every number.
  - **25.2 as built.** §5 above. `Intent::SetNodeUi { node, ui: Option<NodeUi> }`, `Event::NodeUiSet`, `Node.ui` (absent on
    the wire for a node with no place, so no fixture changed; `ui?: NodeUi` in the TypeScript), `Snapshot::change` takes the
    event, the controller's rule is `NodeUi::tidy` (a tenth of a pixel, half away from zero, within 100 000, never `-0`;
    the limits are in `constants.ts`) and nothing goes to the engine. On the page: the reducer (a place given back leaves no
    `ui`), the mock engine (the same rounding, in 64 bits as the controller does it) and `Editor.moveNode`.
  - **25.3 as built.** §9.3 above. `toFlow` puts a node at its `ui`, nodes are `draggable`, `sameStructure` compares the places;
    `useMovableNodes` (with `keepHeld`) is the canvas's state of the nodes, which keeps a node in hand while the session is laid
    out again, sends the place once at the end of a drag and takes what the session has when the answer is in; "Arrange the nodes
    again" is a control of the canvas (`Editor.resetLayout`).
  - **Tests.** Rust: 21 more tests (914 against 893; with the `web` feature 22 more, 1068 against 1046, and one more on the real
    program, 17 against 16): five on the file (reading a place on every kind of node, a bad one ignored, what is written, a node
    with none written as it always was, decimals as typed), one on rounding, two on the loader, three on the saver, one on the
    controller's save, one on the TUI (it shows nothing of the places and keeps them through a delete and Ctrl+S), six on the
    controller's intent, two on the graph, and with `web` one on the generated constants; the wire lists, the bad-input list
    and the snapshot invariant have the intent and the event. On the real program: a browser places the filter and the master,
    a second browser is told as an event, a third is given them in its snapshot, a node that is not there is refused in the
    controller's words, Save writes them, a program started on that file draws them, and a place given back is gone from the
    file at the next save. 49 mutations of the Rust, 48 caught at once; the one that survived was a branch no saved session
    reaches, and it is gone. Web: 42 more tests (1080 against 1038, in two more files): the reducer, the mock engine (rounding,
    limits, `-0`), the editor, `toFlow` and `sameStructure`, `keepHeld`, and eleven on the canvas in jsdom, with real pointer
    events through React Flow's own drag (a drop sends the place and the node stays; sideways and up-and-down moves; a node that
    another browser moves; a node in hand through a new layout; two drags of one node while the first is unanswered, with the
    answers held back; a refused move goes back; nothing is sent for a click; the control); in a browser on the real program one
    more of the 43: a filter is dragged, a second page and a reloaded page have it there, a move on the first page is drawn on the
    second, Save writes the two places, arranging again removes them. 61 mutations of the page, 57 caught at once; four were not,
    and a test was made for each: `-0` (it cannot be seen through JSON, so the test looks at the mock server's own copy), the
    editor stopping at the first node it cannot give back, a drag along one axis, and the count of overlapping drags.
  - **Found on the way.** A dead branch in `inline_ui_tables` (a recursion into plain child tables: no saved session has a `ui`
    below one) showed up as the one mutant that survived, and was taken out rather than tested. My first test of "no place is
    written" looked for `ui` in the text, which `builtin:osc` has. React Flow begins a drag where the pointer is when it has
    gone `nodeDragThreshold` pixels, so a node lags the pointer by that much, which a test that moves the pointer 15 pixels in its
    first step sees; and in jsdom a canvas has no size, so every point is at its edge, where React Flow pans while a node is
    dragged: the tests give the canvas a size (a faked `getBoundingClientRect`), and set the `view` of a pointer event by hand,
    since jsdom takes no window of Vitest's as one. Vitest prints no `console.log` here, so tracing a test was done through a file.
    In the docs: §4.4 planned `pinned` inside `ui` (not built, a TODO), and CLAUDE.md said the layout "is to be stored".
    A refactoring that is worth doing: the `Save*` structs have no `Default`, so one more field (`ui`) was 41 edits in test
    literals; a `Default` (or a constructor for a lane) would make the next one cheap. A node that is placed by hand is a pin:
    the layout does not make room around it, so a node added later can land on one.
  - **Numbers at the end of the step.** Rust 914 tests (893 at the end of step 24), 1068 with the `web` feature and 17 on the real
    program (1046 and 16), 909 without the default plugin formats and 1061 with `web`; web 1080 (1038); 43 in browsers (42).

- [x] **26. Cables between modulators (phase 3, ahead of the rest of it).** A modulator can drive the rate of an LFO, a stage of an
  envelope or the depth of a cable (the TUI's `m` on a field of a modulator and its `t`), and the page could draw cables to
  plugins' parameters only: such a cable that the TUI had made was in the session and not on the canvas. *Done when:* on the real
  program a browser drags a cable from one modulator's output to another's rate or stage, or picks it from the list at the foot of
  the modulator; it is in the server's session, the saved file has it (`mod_rate = 0`), a program started on the file draws it and
  a second browser sees it; it has a depth to turn and a button to take it out; the ports that take it are marked while it is dragged;
  the modulators that drive others are drawn above them; and the TUI's target selectors and the page's choice of what may be drawn
  are one rule. In parts, each its own commit: **26.1** what a modulator can drive of the others' is the model's to say, and the
  TUI's two selectors ask it (a refactoring, Rust only); **26.2** `AddModEdge` takes a rate, a stage or a depth as its port;
  **26.3** the page's rule, the mock engine and the editor; **26.4** the canvas: ports, the cable dragged, the list; **26.5** the
  layout. *Not done (later steps):* the depth of a cable on the canvas (the controller takes a cable to one; the page has no port for
  it, and neither draws such a cable nor offers it: step 36), groups (the TUI's `g` is not an intent yet), taking a lane or a group out
  with a confirmation (there is no undo), pinning, undo, moving a node with the keyboard.
  - **26.1 as built.** `SessionModel::field_targets(at, mod_index)` lists what the modulator at `mod_index` of the rack at `at` can
    drive of the others': for each modulator in reach, its settings (an LFO's rate, an envelope's four stages), then the depths of
    its cables, as `FieldTarget { node, field, edge, kind, owner, field_name, label, base_value, param_min, param_max }`. The
    TUI had three near-duplicate loops that said the same, with the ranges written out again (for a lane's modulator, for a group's
    members' modulators and for a group's siblings), which are one function now; two golden tests of the selectors, written first
    against the old code, pinned what they list.
  - **26.2 as built.** §5 above. `modulators_in_reach(at, mod_index)` (a lane's: the others of its rack; a group's: its members'
    modulators, member by member, then the others of its own rack; never itself), `FieldTarget::port()` (`app::graph`), and
    `add_mod_edge_intent`, which goes by the port: `add_param_cable` (as it was) or `add_field_cable`. The cable is made by
    `add_target` like the TUI's; taking it out gives the setting the value the user set. A fixture the controller writes,
    `web/src/client/fixtures/fields.json` (a session of groups and lanes in and out of them, and for each of its nine modulators
    the node and port it was let to draw to).
  - **26.3 as built.** §9.3 below. `client/reach.ts`: `modulatorsInReach`, `fieldsOf`, `samePort`, and `drivablesOf` with `fields`
    for the modulators; the mock engine's `addModEdge` by the port (`addParamCable`, `addFieldCable`, `withCable`); `portName`
    (`client/names.ts`), which the editor's notices and `describeCable` use (each had a lookup of a parameter's name of its own);
    `Editor.addCable(modulator, node, port)`.
  - **26.4 as built.** §9.3 below. `CablePort`, the setting cells' ports, `drivenSettings`, `cableOf` / `keyOf` / `drawableFrom` /
    `canDraw` by port, and the list's settings; `e2e/canvas.ts` has what the tests of the canvas have in common.
  - **26.5 as built.** §9.3 below. `tiersOf`, `modulatorColumns` by row, `aboveOf` and `placeStrip` by rows.
  - **Tests.** Rust: 15 more tests (929 against 914; with the `web` feature 15 more, 1083 against 1068, and one more on the real
    program, 18 against 17): four on `field_targets` and two on the selectors it came out of (the old code's goldens); on
    `modulators_in_reach` one; on the controller's intent four (a lane's modulator to a sibling and to the depth of a cable of
    its own; a group's to its members' modulators and its siblings; both ways between two modulators; the table of what is refused
    and why) and two more (a setting driven already; taking the cable out), one on the controller's snapshot, and the fixture's;
    on the real program a browser draws a cable between two modulators, is refused the rest, saves it, and a program started on the
    file draws it. 47 mutations of the Rust (27 of `field_targets` and the selectors, 20 of the intent and the reach), all caught;
    three of the first 27 were not until the shared scene (`testutil::CROSS_MOD_RACKS`) was made richer (a second group, a lane in
    no group, an envelope with two cables). Web: 64 more tests (1144 against 1080,
    in one more file): the rule on the page against the controller's fixture, the mock engine against it (every modulator asked
    about every setting and every cable's depth: it allows exactly what the controller listed), the editor, `portName`, the ports of
    the controls (`ports.test.tsx`: a canvas of one node), the canvas (ports, driven, the list by modulator, a cable chosen, a
    refusal in words, a cable taken out), `cableOf` and `canDraw`, and the layout (a pair, a chain, two that drive one, two
    heights, a cycle, a control above an envelope, a group's rack, a cable that makes no row); in browsers 11 more tests (54
    against 43): a cable dragged to a rate and to a stage, the marks while it is dragged, drops that are refused, from the keyboard,
    taken out, on the real program, and a cascade of three with no node on another. 82 mutations of the page (38 of the rule,
    mock, names and editor; 23 of the canvas, 3 of its handlers by the browser tests; 18 of the layout), all caught in the end;
    two survived the first run of the layout's, a check that nothing read (taken out: the `drives` map is made from the rack's
    own modulators now) and a test whose driver came before what it drove.
  - **Found on the way.** The page's notices keep the last three (`MAX_NOTICES`), which my first test of six refusals did not
    know. The mock engine's telemetry bookkeeping (`without`, `paramKey`) needed no change for a cable that ends at no parameter;
    the test that asks every setting of every modulator looks at the layout of the telemetry after each cable it draws and takes
    out. Three conditions of the layout that looked needed were not (a modulator driving itself is cut by the cycle guard, the
    cables of a modulator are modulation cables, a cable from outside the rack is never looked up): the mutation run said so and
    they went. In the docs: §4.2 called the ports
    `ModField(...)` (built as `Port::Rate`... and `Depth { edge }`), §5 said a cable to a modulator's settings is refused "for now",
    §9.3 said the canvas has no handle for one, §11 had the cables to a modulator's rate and stages still to do, and CLAUDE.md had it
    as a TODO; they say what is built now. A refactoring that is worth doing: a node's data has `modulated` (a plugin's driven
    parameters, as numbers) and `drivenSettings` (a modulator's, as handle names), and `Drivable` has `params` and `fields`: two
    lists for what one list of ports and names would say.
  - **Numbers at the end of the step.** Rust 929 tests (914 at the end of step 25), 1083 with the `web` feature and 18 on the real
    program (1068 and 17), 924 without the default plugin formats and 1076 with `web`; web 1144 (1080); 54 in browsers (43).

- [x] **27. Modulators you can see (phase 3, ahead of the rest of it).** A modulator's node said what it puts out as a bar, where it
  is now and nothing of where it has been, and every cable was the same magenta whatever went through it: the modulation was
  there and hard to see (what a rate that another modulator drives looks like, since step 26, could not be told at all). *Done
  when:* on the real program an LFO's node draws what it has put out for the last four seconds as a line that scrolls, a second LFO
  that drives its rate makes it wander, the cables are bright when their modulator is high and dim when it is low, nothing
  renders in React for it, and the big scene still runs at 60 fps. In parts, each its own commit: **27.1** the recent past of a
  value in the client layer (`TraceBuffer`, `plotTrace`); **27.2** `LiveTrace`, a canvas drawn on the frame loop; **27.3** the
  node of a modulator uses it, in place of the bar; **27.4** the cables' brightness. *Not done (later steps):* an envelope's curve
  from its four stages, the span to choose, a trace for a plugin's parameter as modulated, and a trace that does not alias an LFO
  faster than half the telemetry's rate (f85).
  - **27.1 as built.** §9.3 above. `client/trace.ts`: `TraceBuffer` (a ring of the last N samples as 32-bit values with their
    times; what is not a finite number is not a sample; `changedAt`, the time the value last became what it is) and `plotTrace`
    (pure, into an array that is reused and grows, so that a drawing on every frame makes no garbage).
  - **27.2 as built.** §9.3 above. `ui/LiveTrace.tsx` with `ui/trace-draw.ts` (the drawing as a function of a context of its
    own small interface, so that a test gives it a recording one: `ui/testing.ts`), a story, and the test setup gives jsdom's
    canvases no context.
  - **27.3 as built.** §9.3 above. `ModulatorNode` has a `LiveTrace` where it had a `LiveBar` (the same slot, the same `meter`),
    and the layout makes room for it (`TRACE_EXTRA`).
  - **27.4 as built.** §9.3 above. `useCableLight`, `outputRange` (the one place that says what range an output goes over; the
    scale of the bar and of the trace is made of it), and the style.
  - **Tests.** Web: 53 more tests (1197 against 1144, in three more files): the buffer and the plot (20), the drawing (8), the
    component (18: the meter, the frame, the scroll, the pixel ratio and the size, a value that has not moved, a value that goes,
    no context, the subscription), the node (the trace is drawn on its canvas, at the frame, the newest value at the top for an
    LFO at its peak), the cables (an LFO's at its peak, a crossing and its trough, an envelope's over 0 to 1, two modulators, a
    cable to another modulator's setting) and the range by kind; in a browser one more (55 against 54): the trace has lit pixels
    and the cable's computed opacity swings between dim and bright over the LFO's 2 s. 72 mutations of the page (22 of the buffer
    and the plot, 32 of the component and the drawing, 5 of the node and the layout, 13 of the cables), all caught but one that
    cannot be told by the style (a value that is not known written as `1` where nothing is written). Rust unchanged (929 tests,
    1083 with `web` and 18 on the real program, 924 and 1076).
  - **Found on the way.** The telemetry is 30 values a second, so a trace of an LFO above 15 Hz aliases: a 50 Hz LFO is drawn as noise
    (seen in a screenshot of an LFO that another drove up to 50 Hz); what the page draws is what the telemetry carries, and the
    engine's block rate is much higher (f85). A modulator that a test puts into a snapshot by hand has no slot in the layout of the
    telemetry, so nothing can be read of it: the tests that need an output put one in. jsdom printed that it has no canvas for each
    canvas that asked; the test setup says there is none. A test of mine took the first frame that was drawn for the frame of the
    LFO's peak (the first frames had the value before the engine's was in); it waits for the value. The layout test of step 25 (a
    node dragged on the real program) failed four runs in five once the canvases were in the page: it read the place of the dropped
    node as soon as it had moved, which is where the pointer put it (579.888) for about 16 ms and then where the server kept it
    (579.9), which is what the other pages draw. It was a race the test had always had (the time to the server's place, measured
    from the pointer going up, is the same with the traces and with the bars they replaced), and it waits for the answer now
    (`6ea1f62`, its own commit). In the docs: §9.4 said a
    modulator's output is a bar, and the list of the displays in `ui/` (§9.2) did not have `LiveTrace`; the first says a trace
    since step 27 and the second has it.
  - **Numbers at the end of the step.** Rust 929 tests (the same as at the end of step 26), 1083 with the `web` feature and 18 on the
    real program, 924 without the default plugin formats and 1076 with `web`; web 1197 (1144); 55 in browsers (54).

- [x] **28. Reordering effects (phase 1).** The page could add an effect to the end of a chain and take one out, and nothing else: the
  order of the effects, which is the order of the sound, could only be changed in the TUI (Shift+Up and Shift+Down), by code of the
  TUI's own for a bus and a controller method for a lane that nothing in the page reached. *Done when:* on the real program an effect
  moves one place along its lane's chain, or a bus's, from a button in the page, the canvas and a second browser show the new
  order, the note is still heard, and the saved file has the effects in that order with the cables on the same plugins; and the TUI
  goes on doing it the same way, by the same two controller methods. In parts, each its own commit: **28.1** `Controller::move_effect`
  for a lane's chain, which the TUI's two sites call; **28.2** `Controller::move_bus_effect` (and `SessionModel::move_bus_effect`),
  which the TUI's two sites for a bus call in place of their own; **28.3** the intent `MoveEffect` in the controller, the mock engine
  and the editor, with a fixture; **28.4** the buttons, and keeping the focus on one. *Not done (later steps):* dragging an effect to a
  place and a move across chains; lanes and groups still cannot be moved or taken out from the page.
  - **28.1 and 28.2 as built.** §5. The TUI moved a bus's effects by its own code (it sent `ReorderGroupEffect`, swapped the two in the
    model and remapped the group's cables), which the intent could not have reached without a second implementation of the move:
    the controller's `move_bus_effect` does it, to the model first and to the audio thread only if the model did it, for any two places
    and not only neighbours (`SessionModel::move_bus_effect`, with `remap_group_bus_after_reorder`), and the TUI's sites call it.
  - **28.3 as built.** §5 and `app::apply` (`move_effect_intent`, `place_to_move_to`). The mock engine's `moveEffectOutcome` does what the
    controller does: the effects of the chain change places among themselves in the list of nodes, the links of the audio path are
    made again in the places of the old ones, the last of the kind it was (`member` into a group, `audio` into the master), and the
    cables and the layout of the telemetry are as they were. `fixtures/moves.json` is what the controller makes of it (a session with a bus
    of four effects, a lane in its group with four and a lane in none with three; each node, and each effect at each of six places: 74 rows,
    30 moves done and 44 refused), and the mock is tested against every row; writing it asserts that a move leaves the cables and the
    telemetry layout as they were.
  - **28.4 as built.** §9.3 above. `MoveEffectButtons`, `chainPlaceOf` / `sameChainPlace`, `keepFocus`, the style.
  - **Tests.** Rust: 18 more than at the end of step 27, in each configuration (950 against 932 with the default features), and one more on
    the real program: the model (a bus effect moves its cables, nowhere moves nothing, and a property test of every move of a chain of four, in
    a lane's chain and on a bus, that every cable still drives the plugin it drove), the controller (what the audio thread is told, the
    refusals, model and audio thread the same on a real graph, a move in the second of two groups, the snapshot the subscribers are given),
    the intent (a lane's chain and a bus, there and back, the refusals), the TUI (a browser's move: the cursor follows its effect and is left
    alone otherwise, a bus effect, a typed value goes on to the same effect, a selector that holds a place is closed; and Shift+Up and
    Shift+Down mark the session unsaved and the last effect does nothing) and `tests/serve.rs` (the real binary: a second browser told, the note
    still heard, the refusals with no revision used, the saved file in the new order and the cable on the new slot, a program started on it
    drawing that order). Web: 28 more tests (1225 against 1197, in two more files): the mock against the controller's fixture (5), the editor (4),
    the place in a chain (4), the buttons (10: on every effect and no other node, the ends dimmed, nothing asked by a dimmed one, one place
    later and earlier, a bus, the focus kept through three moves, given back when the browser lets it go and not given to a button that did not
    have it, a notice when it could not) and the focus helper (5); in browsers two more (57 against 55): on the mock the order, the cable, the
    buttons fitting the node and the keyboard, and on the real program the order after a reload and in the saved file, with the note heard
    through the reordered chain. Mutations: 29 of the Rust (18 of the two controller methods and the TUI, 11 of the intent), 17 of the mock and
    the editor, 16 of the buttons and the selector and 10 of the focus: all caught but one that cannot be told (the buttons rendered for an
    instrument too: the selector says it has no place, so nothing is drawn).
  - **Found on the way.** The TUI's bus reorder was its own code (above). A test I wrote for the mock engine's refusals read five notices
    and found three: the notices keep the last `MAX_NOTICES`, so each is read as it is said. The mutation of a test whose baseline was red
    "caught" everything: the check that the baseline is green comes first (a loop that keeps the focus was alive across two tests that
    have buttons of the same id). The first browser test went red where the unit test had been green: jsdom keeps the focus of an element
    that is moved among its siblings and Chromium does not, and React moves the node of the effect that goes later; the second try of the
    helper lost to the first, whose loop was alive when the second button was pressed (the latest request replaces the earlier now).
    A moved effect is renamed when it passes a namesake (the canvas numbers by the order of the graph): a TODO in CLAUDE.md.
  - **Numbers at the end of the step.** Rust 950 tests, 1104 with the `web` feature and 19 on the real program, 945 without the default
    plugin formats and 1097 with `web`; web 1225; 57 in browsers.

- [x] **29. Envelopes you can see (phase 3, ahead of the rest of it).** An envelope's node showed a trace of what it has put out, which is a
  flat line until a note is played, and its four stages had to be read off four knobs. *Done when:* on the real program an envelope's node
  draws the shape its stages make, draws it again when a knob is turned, and a dot follows the envelope round it while a note is held (up the
  attack, down the decay, along the hold, down the release) with nothing rendering in React for the dot. In parts, each its own commit: **29.1**
  the shape and the dot as pure functions (`client/envelope.ts`); **29.2** `LiveEnvelope`; **29.3** the node uses it and the layout makes room;
  **29.4** browser tests; **29.5** a fix that the browser test led to, the dot staying on the attack between two readings of the telemetry.
  *Not done (later steps):* dragging the corners to set the stages; a telemetry channel for the stage the engine is in.
  - **29.1 as built.** §9.3 above. `stageWeight`, `shapeOf`, `envelopePath`, `envelopeAreaPath`, `placeIn` (the one mapping from a share and a
    level to a place), `envelopeDot` (which took the level before it, until 29.5).
  - **29.2 as built.** §9.3 above. `ui/LiveEnvelope.tsx` with a story (a new envelope, one at rest, slow stages, a pluck, a pad, nothing to
    follow); the line and fill are React's, the dot is the frame loop's.
  - **29.3 as built.** §9.3 above. `EnvelopeShape` in `nodes.tsx`, `envelopeStagesOf` / `sameStages`, `ENVELOPE_NODE_HEIGHT`.
  - **29.4 as built.** `e2e/envelope.spec.ts`: on the mock, the shape, that it fits in its node and the node is not taller than the layout assumes,
    and a longer attack making a wider one; on the real program, a sampler in the page on every frame sees the dot on the decay, the hold
    (more than ten frames) and the release, in that order, and at rest after. The test was named for the attack too and did not assert it: 29.5.
  - **29.5 as built.** §9.3 above: `courseAfter` and `envelopeDot(shape, level, rising)`. The real-program test turns the attack up first, with
    the keyboard as a person would (a page key on the knob is ten steps, about two and a half times as long, until the shape's words say a quarter
    of a second or more), and sees the dot on the attack, the decay, the hold and the release, in that order.
  - **Tests.** Rust unchanged. Web: 54 more tests (1279 against 1225, in three more files): the shape and the dot (23, and 10 more with 29.5 for the
    course and the dot), the component (11, and 4 more), the selector (3) and the node (3); in browsers two more (59 against 57). 105 mutations of the
    page: 57 for 29.1 to 29.3 (30 of `client/envelope.ts`, and 27 of the component (15), the node (5), the selector (3) and the layout (4)) and 48 for
    29.5 (the course, the dot and the component). 100 were caught, six of them only after a test was made sharper (a time without an end, the rounding,
    the release's direction at a point that is not the middle, a new subscription that assumed the dot is hidden, and the two edges of a move). Five
    cannot be told: a weight of nothing for no time (it is nothing either way), a level before that is not a number (it compares false with everything,
    so the guard was redundant, and went with the argument), the shape asked of every modulator (it draws nothing for one that is no envelope), and two
    pieces of code that 29.5 had left dead (the guard for a sustain of 1 on the decay, which the hold's test makes unreachable, and the reset of "placed"
    after a gap, which a new course makes no difference to), which are gone.
  - **Found on the way.** The real-program test was named for the attack, the decay, the hold and the release and asserted the last three: the
    attack of a new envelope is 10 ms and the readings of the telemetry are 33 ms apart, so a sampler seldom sees the dot on it. Made to see the
    attack (by turning it up), it went red, and a probe of the dot's place on every frame showed that it flickered between the attack and the release
    thirty times a second (f90: the unit tests had fed a new level to every frame, and the frames between two readings find the level where it was).
    The probe showed a second thing, one frame at the start of the release at the end of every decay (f91: the telemetry's 32-bit sustain is a hair
    under the session's). The first fix, a course that never expired, was wrong for an envelope that comes to the sustain in one step (the mock's, and
    a fast pluck's), which the test of the node with the mock's envelope found at once: a rise that has stood still for three readings is steady.
    A test run hung at 100% CPU while the dot was written on every frame (f92: §9.3); the component writes only when the dot moves now, and the tests
    turn the frames by hand outside `waitFor` and compare floats to three digits. A time limit that kills `npx` (`perl -e alarm`) leaves vitest's
    workers spinning: the runner has to kill the process group.
  - **Numbers at the end of the step.** Rust 950 tests, 1104 with the `web` feature and 19 on the real program, 945 without the default plugin
    formats and 1097 with `web` (unchanged); web 1279 in 64 files; 59 in browsers.

- [x] **30. Pinning parameters to a node (phase 1).** A node showed the first eight parameters of its plugin and every one a cable ends at, and a
  person could not choose which: the panel turned any parameter and could not put it on the node. *Done when:* a pin on every row of a plugin's
  panel puts its parameter on the node as a knob, or takes it off; the pins are the session's (the program keeps them, tells every page, saves them
  by name and reads them back, and a plugin that is moved or replaced is as it should be); and the TUI and the engine are as they were. In parts,
  each its own commit: **30.1** the session file and the model; **30.2** the intent `SetPinned`, the event, `Plugin.pinned` on the wire, the page's
  reducer, editor and mock engine, and the fixture that pins the mock to the controller; **30.3** what a node shows (`client/pinning.ts`), the canvas
  and `Editor.pinParam`; **30.4** the pins and the button in the panel; **30.5** browser tests. *Not done (later steps):* a node with no knobs at all,
  pinning from a knob on the node, ordering the pins by dragging.
  - **30.1 as built.** §4.4 above. `pinned` in `session.rs` (`PluginConfig`, `EffectConfig`, `SaveInstrument`, `SaveEffect` and the structs that are
    written), `PluginSlot::pinned` with `pin_names` and `pinned_names`, the loader's three sites and the saver's two.
  - **30.2 as built.** §5 above. `Intent::SetPinned`, `Event::PinnedSet`, `Controller::set_pinned` (`app::apply`), `Plugin.pinned` in the graph,
    `Snapshot::apply`, `MAX_PINNED` and `PINNED_LIMIT`; the TUI's match on events has an arm for it and a test that a browser's pin leaves its tree
    as it was and the session unsaved. The page's reducer, `Editor.setPinned` and the mock engine's `pinnedOutcome`, which `fixtures/pins.json`
    pins to the controller (every node of a session, the master and one that is not there, each with nine lists: 72 rows, 12 taken and 60
    refused in the controller's own words); the mock has no refusal of a separator, which its graph does not have.
  - **30.3 as built.** §9.3 above. `client/pinning.ts` (`shownParams`, `knobsOf`, `pinnedWith`), `PatchNodeData.knobs`, `sameStructure`,
    `Editor.pinParam`, the selectors `isKnob` and `hasPins`.
  - **30.4 as built.** §9.3 above. The pin of a row (`PinButton` in `controls.tsx`) and the button of the panel (`ParamPanel.tsx`).
  - **30.5 as built.** `e2e/pins.spec.ts`: on the mock, a reverb (ten parameters: its node shows the first eight and not `lowcut`) has `lowcut` pinned
    from its panel, the first eight stay, the keyboard takes `width` off with the space bar and the focus stays on the pin, and the button
    gives the node back to the first eight; on the real program, a second page and a page that is opened again have what was pinned, what the
    second page does shows on the first, and the saved file has the pinned parameters by name, in order, and nowhere else (and none after
    they are given back).
  - **Tests.** Rust: 22 more than at the end of step 29 in each configuration (972 against 950 with the default features) and one more on the real
    program: the file's format and the loader and saver (12: the file read on each kind of node and leniently, written and read back, the names
    resolved, a separator, a name that two parameters share, an effect that is moved takes its pins) and the controller (10: what is kept and
    in which order, every refusal with nothing changed, the limit, what a session saves, an instrument that is replaced and a preset that is
    loaded, an effect that is moved, the graph, the TUI, the fixture), with cases in the property test of every intent against the snapshot,
    the wire shapes and the refusals of an event; and `tests/serve.rs` (the real binary: one browser pins, a second sees it, the controller's
    words for three refusals, the saved file, a program started on the file, and the node given back). Web: 46 more tests (1325 against 1279,
    in two more files): the reducer (6), the editor (9: `setPinned` and `pinParam`), the mock against the fixture (3), what a node shows (15),
    the selectors (3), the canvas and its layout (3 new, and three of the old ones moved into the module: which is why these add up to 46), the pin of a row (6) and the panel (4);
    in browsers two more (61 against 59). 103 mutations: 20 of the file format, the model, the loader and the saver, 34 of the controller, the
    snapshot and the page's mock, reducer and editor (16 and 18), 31 of what a node shows, the selectors, the editor's `pinParam` and the
    structure rule, 15 of the pin and the button, and 3 of the layout's use of the knobs. All were caught in the end: two only after a test was
    written (a name that two parameters share, and a mock that refuses a list of exactly the limit one place too soon, which the fixture
    had no list to show), and one did not build and was written again.
  - **Found on the way.** A browser assertion that no node is over another after a pin passed with the layout broken (a mutation that ignored every
    node's knobs): a pin on a built-in plugin cannot make its node a row taller (three rows at the first eight and at all ten), so it could not
    fail for the reason it gave. It was taken out, and the layout is tested where it can be seen, with a plugin of twelve parameters. A sentence of
    the 30.3 commit message said no built-in plugin has more than eight parameters; the reverb has ten, which the browser test of 30.5 uses (the
    message was amended: the commit was not pushed). The fixture showed how the controller answers for the master, which is no node of its
    model: `there is no node 4294967295`, which the mock now says too. A change of the pins had to be a change of structure for the canvas to be
    laid out again, which a test of `sameStructure` was written for first. The page's notices keep the last few only (the same as in step 28):
    each is read as it is said.
  - **Looked at.** On the real program with the user's own Pianoteq 9 on the null audio device (2,202 parameters once the plugin's locked ones
    are left out): the panel's first eight are filled and the rest open, the filter "Volume" finds nine, pinning three of them adds a third row of
    knobs to the node, and the button gives the first eight back.
  - **Numbers at the end of the step.** Rust 972 tests, 1126 with the `web` feature and 20 on the real program, 967 without the default plugin
    formats and 1119 with `web`; web 1325 in 66 files; 61 in browsers.

- [x] **31. Groups from the page (phase 1).** A browser could not make a group, put a lane in one, name one or take a lane or a group out: the
  TUI could (`g`, `R` and `d`), with code of its own. *Done when:* a person can, from the page, add a group, put a lane in a group and take it
  out of its group (a choice on the lane), name a group in place, and take a lane or a group out after the page has asked whether to; every
  browser and the TUI see it, the saved file has the groups, their names and which lane is in which, and the TUI does the same things through
  the same intents; and a lane that leaves a group lets go of what the group's cables drove. In parts, each its own commit: **31.1** the move of a
  lane between groups is the controller's (`set_lane_group`), a refactoring; **31.2** the fix for what a lane leaves when it leaves; **31.3** the
  intents `AddGroup`, `SetLaneGroup` and `RenameGroup`, the event `GroupRenamed`, the wire and the TUI's `g` and `R` onto them; **31.4** the page's
  mock engine, the editor and the fixture that pins the mock to the controller; **31.5** `RemoveNode` for a lane and a group; **31.6** the page;
  **31.7** the TUI's side of what a browser does to groups, as tests. Two commits of their own for what was found on the way: the program
  crashing on the way out, and the canvas's reveal of a node. *Not done (later steps):* undo, putting a lane in a group by drawing its audio to the
  group (the menu is what every keyboard has; a cable is for the mouse), and the TUI's `d` on a lane or a group asking `RemoveNode`.
  - **31.1 as built.** `SessionModel::set_lane_group` and `Controller::set_lane_group` (the command to the audio thread and the repair of the group
    modulators' cables in the model, which name a member by its place among the group's lanes: one that leaves takes its place with it and one that
    joins takes one, as the audio thread does it); the TUI's `g` popup asks it, and its three imports of the helpers are gone.
  - **31.2 as built.** §5 above and D34. `Controller::set_lane_group` takes the group's cables to the lane's plugins and modulators out first, each by
    `take_out` with the cables that drive their depths, and then moves the lane, and what they drove has the value that was set (`give_back`: unless a
    cable of the lane's own still drives it); a lane put in the group it is in has nothing taken out. Until then the TUI's "Ungroup" left what the
    group's LFO had driven where it had put it, for good (f65, open since step 15 for exactly this: it waited for the TUI's `g` to ask the controller).
  - **Found on the way, 0017d00 (f95).** The test of the real program that sends it Ctrl+C failed once with a segmentation fault: the program had
    been interrupted a moment after it started, while its scan of the plugins was still going in the native code of the plugins, and the process's
    ordinary exit ran what those libraries run at exit while that thread was in them (3 runs of 60 ended with signal 11). `main` calls `run()` and
    ends the process itself (`libc::_exit` on Unix), after flushing and after printing an error as returning it would (`tests/cli.rs`, 3 tests; the
    interrupt, 40 runs in `tests/serve.rs`, failed 3 times of 3 before and never after; 150 interrupted runs ended 150 times with status 0).
  - **31.3 as built.** §5 above. `Intent::AddGroup { members }`, `SetLaneGroup { node, group }`, `RenameGroup { node, name }` and `Event::GroupRenamed`
    (`app::apply`, `app::snapshot`), `MAX_NAME` and `NAME_LIMIT` (the page reads the limit and does not copy it), the generated types, the page's reducer
    and the editor's words for the three; the TUI's `g` is `SetLaneGroup` or `AddGroup` with the lane in it, `R` on a group is `RenameGroup`, the tree is
    drawn again when a group is renamed, by the TUI or by a browser, and the name popup takes no more than the longest name there is.
  - **31.4 as built.** §9.3 above. The mock engine's `addGroupOutcome`, `setLaneGroupOutcome` (with `lanePutIn`) and `renameGroupOutcome`, pinned to the
    controller by `fixtures/groups.json`, which the Rust test `the_web_clients_fixture_of_what_the_intents_of_groups_come_to_is_current` writes
    (a scene with two groups and four lanes, a third group, empty, being put in by 31.5, and 66 intents: a group added with no member, with each lane, with a lane that is in a group, with
    one twice and with what is no lane; each lane put in each group and in none; names with spaces round them, of nothing, at the limit and one past
    it, in letters with accents and in characters outside the first plane, with a line break, a tab, a bell and the spaces of other scripts); the
    mock matched every row the first time it ran. `Editor.addGroup`, `setLaneGroup` and `renameGroup`, each saying what it could not do in the
    words the canvas uses for the nodes.
  - **31.5 as built.** §5 above. `RemoveNode` takes a lane (`remove_lane`) or a group (`remove_group`, which the TUI's `d` has always called) and
    refuses the last lane (`a session keeps its last lane`); the mock's `removeLaneOutcome` and `removeGroupOutcome` renumber the groups that are
    called by their places, 13 more rows of the fixture (79) and `lastLane`.
  - **31.6 as built.** §9.3 above. `GroupCell` (`groupChoicesOf`, `sameGroupChoices`), `AddGroupButton`, `GroupNameField` and the ✎ button, and
    `RemoveLaneOrGroupMenu` (`removalOf`, `sameRemoval`, `removalWords`), `Editor.setLaneGroup` shows the canvas the group a lane is put in; browser
    tests (`e2e/groups.spec.ts`: on the mock engine, with a lane that is put in a group, named, and a group that is taken out after the menu has
    asked; the second lane that is added and taken out and the last that cannot be; and on the real program, with a second page and the saved file).
  - **Found on the way, 65edd7b (f96).** The browser test of 31.6 could not click the ✎ of a group it had just put a lane in: "element is outside of
    the viewport", for good. `ShowWhatWasAdded` judged a node it was asked to show against `getViewport()`, which while a move is going (250 ms) is a
    point on the way: the group had been added and, at once, a lane put in it, which moves the group's node; the second request found the node all
    in the view at the point the canvas was passing, did nothing, and the first move ended at a view worked out from the layout before. The canvas
    remembers the view it is moving to for the 250 ms the move takes and judges against that (two tests; 7 mutations).
  - **31.7 as built.** `browser_changes_structure` in `src/tui/golden_tests.rs`: 9 tests of what the TUI shows of what a browser does to groups (a
    header for a group that is added, a lane under its header or under none, a name in the tree and in the title of the pane, the name being typed in
    the TUI, a group taken out, the cursor, and every popup that holds a place closed by the group's intents). Eight were green when they were
    written (the TUI did what it should: no code changed); 7 mutations of the TUI, all caught.
  - **Tests.** Rust: 34 more than at the end of step 30 in each configuration (1006 against 972 with the default features), 3 of the shell's in
    `tests/cli.rs` and one more on the real program: the controller (31.1: 6; 31.2: 4 and the TUI's 1; 31.3: 8 of `apply` and 2 of the TUI, with cases
    in the wire shapes and the property test of every intent against the snapshot; 31.4: the fixture's; 31.5: 3), and 31.7's 9. Web: 58 more tests
    (1383 against 1325, in two more files): the reducer (4), the mock against the fixture and the editor (14 in 31.4), 1 in 31.5, the reveal (2),
    and 31.6's 37 (the selectors 10, the editor 1, the page 4 and the groups of the canvas, 22); in browsers 3 more (64 against 61). 184 mutations
    in all (31.1: 9; 31.2: 6; the exit: 5; 31.3: 15 of the Rust and 4 of the reducer; 31.4: 29; 31.5: 20; the reveal: 7; 31.6: 82; 31.7: 7). All were
    caught in the end but four that make no difference (31.5: a check of the cables that start in a lane, which all end in it as well, and a
    renumbering that starts at the place that is taken out; 31.6: a condition that is always true, since a lane has no lanes, which was taken out
    of the code, and an id that one function makes for both of its ends); some only after a test was written, and 31.6's 16 survivors of the
    first run are what its tests were strengthened for.
  - **Found on the way, in the page (not in code that was committed).** The Enter that ends the name of a group opened the field again in a real
    browser: the focus goes to the ✎ button while the key is being handled, and the browser gives the rest of the key, which presses a button that
    has the focus, to it. jsdom cannot see that, and the browser test did; the field takes the key itself (`preventDefault`), and a test says it was
    taken. A test of the page asserted that nothing had been asked of the server before a request could have left (a request leaves a moment
    after the click), and let a mutation that removed a group on "Keep" pass (f97): it and four like it say it after the page has had its turn. The
    message of 31.6 was written with counts that were not measured (three of the survivors were said to be of one kind, and seven of another: it
    was one, and five) and was amended (f98). A browser test found the node of the group by its name, which is not there while the name is a field.
    The list of the structure intents in §5 of this document called the intent that puts a lane in a group `AssignGroup`, which it was planned as
    and was not built as (`SetLaneGroup`: the intent says what changes, and none takes the lane out), and it and CLAUDE.md said that `RemoveNode`
    refuses a lane and a group; both say what is built now.
  - **Looked at.** The page on the real program on the null audio device, in a browser (screenshots of a flow like the browser test's): a group made
    from a lane's choice, the name field, the menus that ask before a group and before a lane are taken out, the page after a lane is taken out and
    after "+ Group". A group's choice on a lane is as wide as its cell (64 px) and cuts "Group 2" to "Group :": the tooltip says it whole; a wider
    choice is a layout of the node's controls that was not changed here.
  - **Numbers at the end of the step.** Rust 1006 tests, 1160 with the `web` feature and 21 on the real program, 1001 without the default plugin
    formats and 1153 with `web`, and 3 of the shell's in each; web 1383 in 68 files (1411: the working copy's Vitest cache hid 28 stories
    tests, which step 32 found, f101; the 58 more tests of this step are right, as both counts had the same 28 missing); 64 in browsers; 276 commits on the branch.

- [x] **32. Undo, the values (phase 1).** There was no way back from a change: a knob turned by mistake stayed turned, and a click on a "×" is
  final. *Done when:* a person can, from the page and from the TUI, put back what was set (a parameter, a gain, a setting of a modulator, the depth
  of a cable, the keys of a lane, where a node is drawn, the knobs a node shows, the name of a group, the scale and the lock of the piano, the
  place of an effect in its chain) and do it again, a drag being one step, with buttons and keys that say what they would do; every page is told
  what can be done, and what was set in one can be put back from another; and what cannot be put back yet (the structure) empties the history and
  is not put back wrongly. In parts, each its own commit: **32.1** the history in the controller (`Controller::undo` / `redo`); **32.2** the intents
  `Undo` and `Redo`, and the message `History`; **32.3** the mock engine's history, pinned to the controller's by a fixture; **32.4** the client's
  state; **32.5** the page; **32.6** the TUI's keys. Three commits of their own for what was found on the way: the mock engine's words of a refusal,
  the numbers of the built-ins' fixtures, and a Vitest cache that hid stories tests. *Not done (later steps):* undo of the structure (nodes added and
  taken out, presets, the instrument of a lane, a lane's group, a cable drawn or taken out: each empties the history), the TUI's own structure edits on
  intents (so that they are in the history), a line in the TUI that says there is nothing to undo, and the keys of the page for the canvas's own
  focus.
  - **32.1 as built (e057e09).** §5 above and D35. `src/app/history.rs`: `History` (the steps, `record`, `take_undo`, `kept_undone`, `take_redo`,
    `kept_redone`, `clear`, `state`), `Step` (the intents that put back and that do again, the words, the `Key`, when it was last changed), `Plan`
    (a step, a barrier, or no change) and `Controller::plan_history`, which reads from the model what the intent will overwrite before it is applied;
    `apply` is a recording wrapper round `carry_out`, and `Controller::clock` is the thing a test turns (a drag is a second). 48 tests.
  - **32.2 as built (ab12cf5).** §5 and §7 above. `Intent::Undo` and `Redo` (`{"type":"undo"}`), refused with nothing to do and no revision;
    `ServerMessage::History { undo, redo }`, told by `web::owner` after `apply`, after what a front-end of the program's own did and on `subscribe`;
    the generated types; a property test that what Undo and Redo do is events a client applies as any others, and a test of the real program (a
    browser's client turns a value, undoes it, redoes it, is told what it can do, and the saved file has what is done).
  - **32.3 as built (f1da2ad).** `web/src/client/history.ts` (`History`, `planOf`), the mock engine's `#carry`, `#backAndForth` and `#keep`, which do
    what the controller does, tell `History` as the server does, and answer `apply` with what was told (`Told`); `HISTORY_SAME_STEP` and
    `HISTORY_MAX_STEPS` are generated into `constants.ts` from `SAME_STEP` and `MAX_STEPS`. The controller writes the fixture
    `fixtures/history.json` (`the_web_clients_fixture_of_what_a_history_comes_to_is_current`): 21 scripts of 254 intents and 31 waits (285 rows) over a
    session of two groups, a reverb, a filter, two LFOs, an envelope and three cables, with a page subscribed (so that the revisions are a page's):
    for each intent the events it made, stamped, whether it changed the structure, the words of Undo and Redo after it or of its refusal (37), over all
    23 kinds of intent the history knows, 15 changes of structure; the scripts tell one knob, gain, setting or cable from another by the node and
    the kind, a drag from steps a pause apart (and exactly a second), and each barrier by itself. 86 mutations, 84 caught in the end: 8 survived the
    first run and are what the second LFO, the repeated move of one node, the tests of what a rename puts back and three of the connection are for;
    2 are equivalent as long as every step has one intent (the events of a step's intents are added up, and its `restructured`).
  - **32.4 as built (65acb7b).** `ClientState.history` (`{ undo, redo }`, the words of a button or null; `NO_HISTORY`): the `history` message replaces it
    whole, one that says what the client has changes nothing in the store, and it is forgotten when the connection is lost and told again by the server
    the connection comes back to.
  - **32.5 as built (2f46ef9).** `app/UndoRedo.tsx` in the bar of the session: two buttons named by what they do ("Undo set cutoff") with a tip that has the
    keys, disabled with nothing to do or while an answer is awaited, and the keys Control or Command+Z, Control or Command+Shift+Z and Control+Y
    (Command+Y is the browser's History on a Mac), not in a field that takes text (its own undo) and not when there is nothing to do (the browser's
    own is left alone); `Editor.undo` and `redo`; `takesText` is exported from `client/keyboard.ts`. 3 browser tests (`e2e/undo.spec.ts`: on the real
    program, with a second page and the saved file; the filter field of the panel and an effect that empties the history; the mock engine). 35 mutations.
  - **32.6 as built (6196f78).** The TUI's Ctrl+Z, Ctrl+Y and Ctrl+Shift+Z (the capital Z too), from any tab, the same history; the Help tab lists them;
    9 tests (`undo_and_redo` in `src/tui/golden_tests.rs`), 13 mutations.
  - **Found on the way, 9245e0f (f99).** The first comparison of the mock with the controller's fixture found that the mock refused in words of its own:
    "that node has no parameter 99" for the controller's "the plugin has no parameter 99", "mix does not apply to that node" for "Mix does ...",
    "Infinity is not a value" for Rust's `inf`, the number before the node for a gain, and one message for a node with no plugin, a lane with none and a
    plugin without the parameter, where the controller has three. The mock says what the controller does (18 mutations; one branch of `shown` was
    redundant and was taken out).
  - **Found on the way, 47e4243 (f100).** Three fixtures (`builtins.effects.json`, `.instruments.json`, `.presets.json`) were written through
    `serde_json::to_value`, which widens a 32-bit number to the 64-bit number it is (0.707 was 0.7070000171661377, 62 lines in all): a page on the mock
    was given numbers the program never sends. `testutil::wire` writes a value through its JSON text, as the wire has it. (The history's fixture is
    written so as well.)
  - **Found on the way, 9b06bf0 (f101).** The stories test rendered 35 stories in the working copy and 63 in a fresh one: Vitest keeps what it makes of a
    file, the expansion of `import.meta.glob` included, in `node_modules/.vitest-cache`, and the cached `src/stories.test.tsx` knew the seven story files
    there were then, so the stories of Piano, LiveEnvelope, LiveTrace, MenuButton and NoticeBoard (28 tests) had not been rendered since they were added,
    with no word. Every count of web tests taken in the working copy until then was 28 short. The test now asks the disk for the story files and fails,
    with what to do (`rm -rf node_modules/.vitest-cache`), when the glob does not know one. The cache was 190 MB in 4314 files: a mutation run fills it.
  - **Found on the way, in the work (not in code that was committed).** Held-back answers: three tests counted "an event and an answer" for a request, and a
    request is now answered by its events, what Undo and Redo say if that is new, and the answer, so `holdable` counts and lets through answers. A move of
    `groups_scene` into `testutil` that nothing used was taken back. A run of mutations that was interrupted left its mutation in a file
    (`history.ts`), found by checking that every `old` string of the next list occurred once; it was put back, and every list is checked so now. The two
    popup handlers of the TUI are given the key code and not the modifiers, so a Ctrl+letter typed in a popup is the letter (Ctrl+Z in a value being
    typed types a z): not changed here, and not new. §5 said `apply` returns `Applied { rev, events, undo }` with the errors `NotFound` and `Invalid`: it is `{ rev, events,
    restructured }` and `NotFound`, `EdgeNotFound`, `Invalid` and `Io`; CLAUDE.md's frontend line said steps 18 to 30, and it is to 32.
  - **Looked at.** On the real program on the null audio device and the mock engine, in a browser, by the three tests above: the buttons' names and
    tips, a drag of five presses put back by one Undo, the keys with Control and with Command (Playwright's `ControlOrMeta`), a second page told the history
    when it comes and doing a Redo the first sees. The bar was looked at in screenshots of three states (nothing to do; Undo to do, with Save; Redo to do, with the pointer over
    it): the two buttons are drawn as Save is, dimmed when disabled, with its outline on the pointer.
  - **Limits, as built.** An effect that was moved is put back to the place it was at, by its number in the chain: a TUI edit that adds or takes out an
    effect (which is not in the history, and does not empty it) between the move and its undo puts it at a place that is not the one it left. A step whose
    node a front-end took out is dropped when it is reached, with the reason, and the page says it in a notice. The page asks for one Undo at a time
    (a held key goes at the speed of the answers). The history is the program's, not a page's: another person's change is what Undo puts back.
  - **Tests.** Rust: 60 more than at the end of step 31 in the default configuration (1066 against 1006): 48 in 32.1, 2 in 32.2, the fixture's, 9 in
    32.6; with `web` 64 more (1224), the same 60 and 4 that only that feature has (3 of the owner's in 32.2 and the test of the constants in 32.3);
    the real program's, 22 against 21 (32.2). Web: 1457 in 70 files (1411 in 68
    at the end of step 31): the words of the mock (1), the history and the mock against the fixture (16), what the mock tells (4), the client's (6),
    the guard of the stories (1) and the page (18: 3 of the editor, 14 of the buttons and keys, 1 of the bar); 67 in browsers (64 + 3). 208 mutations in
    all (32.1: 45; 32.2: 11; the words: 18; 32.3: 86; 32.5: 35; 32.6: 13): all caught in the end but the two that make no difference.
  - **Numbers at the end of the step.** Rust 1066 tests, 1224 with the `web` feature and 22 on the real program, 1061 without the default plugin
    formats and 1217 with `web`, and 3 of the shell's in each; web 1457 in 70 files; 67 in browsers; 286 commits on the branch.

- [x] **33. Undo, what is added (phase 1).** The history of step 32 forgot everything at an add, so a click on "+ Reverb" could not be taken back and
  stopped the Undo of what was done before it. *Done when:* an effect (a lane's or a bus's), an instrument, a modulator, a cable, a group and a lane's
  group that were added or changed are put back by Undo, from the page and from the TUI, and done again by Redo **under the ids they had**, so that
  what was done to them in between (a gain on an effect that was added) is put back on them; the TUI asks by intents for what has them, so that what
  it does is in the history too; the mock engine does what the controller does, after every undo and redo, as far as the ids and the slots of its
  telemetry. In parts, each its own commit: **33.1** the controller (57bd956); **33.2** the mock engine and its fixture (a3be1e5); **33.3** the
  browser tests (47756e3); **33.4** the TUI's plugin selector and `d` (a6636da); **33.5** its moves, modulate popup, learn and target selector
  (fecdd32); **33.6** its `n` (43d9575). Three commits of their own for what was found on the way (a fix of the mock, 59ce606, the Help tab,
  e59d8c4, and the mark of a join, 9697bb4). *Not done (later steps):* undo of what is taken out (`RemoveEdge`, `RemoveNode`), of a preset, of another instrument in a lane, of a group
  made of a lane that is in another and of a lane that leaves a group that drives it (each empties the history), and a line in the TUI that says there
  is nothing to undo.
  - **33.1 as built (57bd956).** §5 above and D36. `NodeIds` has a queue of ids to hand out first (`reuse`, `drop_ids_to_put_back`) and a log of what it
    handed out (`begin_log`, `take_log`, `Made`); `Controller::carry_out` writes the log down (`made_last`, which a test reads as `last_made`); a
    `Plan::Added` becomes a step once the ids are known (`takes_out`: the one node or the one cable that was made, if it is there); `Step` has
    `undo_made` and `redo_made`; `carry_out_all` says them to the allocator, refuses ids in use (`node 7 is there already`, `cable 3 is there
    already`) and drops what was not used. `AddGroup` of lanes that are in no group and `SetLaneGroup` of a lane that its group does not drive
    (`group_cables_into_lane`) are steps, and the rest of the barriers stay; the instrument of a new lane no longer empties the history
    (`install_instrument`: nothing in it names the lane); `ModulatorKind::label` is the word of the button. 29 tests (7 of the allocator, 22 of the
    history, among them a session built by 14 adds that is undone through each structure it had and done again, graph for graph), 43 mutations and 8
    more for the plan of a lane's group, all caught. The fixture of the web client's tests kept to the barriers that are left (the mock was not
    changed yet).
  - **Found on the way, 59ce606 (f106).** Comparing the shape of the session after each op of the new fixture found that the mock engine took no
    telemetry slot for a cable to a parameter that another cable drives, where the controller's `add_target` takes one before it looks (the layout names
    the parameter once, but the slot is used): everything the mock made after one had a slot one lower. A test of the controller pins the rule (a
    target that takes none and one that takes two are caught), and one of the mock was red before the fix.
  - **33.2 as built (a3be1e5).** `client/counters.ts` (`Counters`, taken out of `mock.ts`: the ids that come next, the log; 8 tests),
    `client/membership.ts` (`cablesIntoLaneFromItsGroup`, which `lanePutIn` uses too; 5 tests), `planOf` with the catalog and `takesOut` in
    `client/history.ts`, `Step.undoMade` / `redoMade`; `shape_of` moved to `testutil` and `shapeOf` to `fixtures.ts`. The fixture of the history is 33
    scripts (it was 21) of 349 intents and 27 waits, 60 refusals, and each of the 81 ops that changed the structure says what the session comes to
    (the nodes with their ids, the audio path, the cables, the layout of the telemetry), so the mock's ids and slots are the controller's after every undo and
    redo. 32 more tests in Vitest (19 of the history), 52 mutations, all caught. Two web tests that used an add as the thing that cannot be undone use
    `replaceInstrument`.
  - **Found on the way, in 33.3 (f105).** The browser test of step 32 that used an effect that is added as its barrier was out of date from 57bd956 (the
    controller keeps an add as a step) and was not run until the browser suite was: the commits 57bd956 to a3be1e5 have one browser test that fails
    against the program. It is `e2e/undo.spec.ts` in 33.3.
  - **Found on the way, 9697bb4 (f108).** On a second look at `join_steps_since`: its mark was the number of steps there were, so a barrier in the middle
    of what is done as one step (which forgets the history) left a mark of a history that is gone, and with enough steps after it the join took the last two of
    them for the ones made as one. The mark knows how often the history was forgotten, and one from before a forgetting joins nothing. No key of the TUI has a
    barrier among its steps; it was a trap for the next caller. One test, red before; 4 mutations.
  - **33.3 as built (47756e3).** `e2e/undo.spec.ts`: the test that adds an effect and takes it out with its "×" (the barrier that is left); a new one
    on the real program that adds an effect, turns a knob of it, undoes both by the button and by the keys (the file saved between has no reverb), and
    redoes both, the knob where it was turned, which needs the id; the mock's test adds an effect and takes it out and puts it back. 68 in browsers.
  - **33.4 as built (a6636da).** The TUI's plugin selector asks `AddEffect` (a lane's, a bus's) and `ReplaceInstrument`, and `d` asks `RemoveNode` (every
    kind of node but a lane's instrument, which `clear_instrument` empties, and that now forgets the history, as putting another in does). 5 tests,
    10 mutations, 9 caught (the one that is not drops the early return after a refusal).
  - **33.5 as built (fecdd32).** Shift+Up and Shift+Down on an effect are `MoveEffect`; the modulate popup, learn and the target selector draw their
    cables by `AddModulator`, `AddModEdge` and `SetEdgeDepth`; `m` on a node is `AddModulator`. Everything of this and the last part goes through
    `State::apply`, which brings the tree up to date. `Controller::history_mark` and `join_steps_since` (gone in step 37, when `Modulate` made one intent of it) make one step of what one key made ("modulate
    cutoff", "learn cutoff"; a mark knows how often the history was forgotten, so a barrier in the middle joins nothing: found on a second look at the
    code, with its own commit); `SessionModel::endpoint_of` turns the positional kind that a popup lists into the port `AddModEdge` names (tested against
    every cable of five sessions). Two things differ from before, as in the browser: a second cable from a modulator to a parameter it drives already is
    refused, and a learned control's cable is drawn at 0.5 and set to 1.0 a command later. 18 tests, 24 mutations, 22 caught and 2 that make no
    difference (what the undo of a joined step makes, which nothing makes yet).
  - **33.6 as built (43d9575).** `n` asks `AddInstrument` for a lane with the oscillator in it (the catalog has to name it, as the scan has long before a key
    is pressed, and with none there is no lane and no selector: a lane with nothing in it would be silent); `Controller::load_plugin`, which was the TUI's
    loader, is for the tests. 2 tests, 4 mutations.
  - **Looked at.** The real program in a pseudo-terminal on the null audio device (a script): an effect added from the selector is saved (1 effect in the
    file, then 2), Ctrl+Z and a save (1), Ctrl+Y and a save (2), and the program ends with status 0; the page's words for an add ("Undo add Reverb", "Redo
    set damping") in the browser tests above. The page was not looked at in screenshots: no layout changed.
  - **Limits, as built.** Redo of an add loads the plugin again from the catalog (at once for the built-ins; a pause for a heavy plugin, on the thread
    that asked: the TUI's loop or the owner's). A modulator that is put back lands last in its rack, whatever place it had (an add is always last, so none
    of the adds shows it; the removals will). A node that comes back has new slots of the telemetry, and each undo and redo of a modulator or a cable uses some of
    the 4,096. `n` and then a choice in its selector is an add and a replacement, and the replacement empties the history, so the lane cannot be
    undone after it. A joined step is the TUI's: a browser draws a cable to a modulator it has just added with two clicks and has two steps. The TUI's
    own edits that have no intent (the order of the lanes, the pattern, the tempo, the Type row) are not in the history and do not empty it. A step
    that was undone and whose node the TUI's `clear_instrument` or a front-end's direct call took out is dropped, with the reason.
  - **Tests.** Rust: 56 more than at the end of step 32 in the default configuration (1122 against 1066): 29 in 33.1, 1 pin of the slot, 5 in 33.4 (4 of the
    TUI's, 1 of the controller's), 18 in 33.5 (11 of the TUI's, 6 of the join, 1 of `endpoint_of`), 2 in 33.6 and 1 of the mark; the same 56 in each of the
    others (1280 with `web`, 1117 and 1273 without the default plugin formats); the real program's, 22 as before. Web: 1491 in 73 files (1457 in 70): the slots (2), the counters (8), the cables of a group (5) and the
    history (19); 68 in browsers (67 + 1). 153 mutations in all (33.1: 51, the slot: 2, 33.2: 52, 33.4: 10, 33.5: 30, 33.6: 4, the mark: 4): 149 caught and 4 that make no
    difference.
  - **Numbers at the end of the step.** Rust 1122 tests, 1280 with the `web` feature and 22 on the real program, 1117 without the default plugin
    formats and 1273 with `web`, and 3 of the shell's in each; web 1491 in 73 files; 68 in browsers; 297 commits on the branch.

- [x] **34. Undo, what is taken out (phase 1).** The history of steps 32 and 33 forgot everything at a removal, so a click on "×" could not be taken back
  and stopped the Undo of what was done before it, which is why the page asks first before it takes a lane or a group out. *Done when:* a cable, a
  modulator, an effect (a lane's or a bus's), a lane and a group that were taken out are put back by Undo, from the page and from the TUI, **as they were**:
  under their ids and the slots of their telemetry, in their places (a modulator in its rack, an effect in its chain, a lane among the lanes), with their
  cables, what was set on them, and, for a plugin, the instance that was in the graph, which the audio thread hands back, so that undo is at once and
  does not load it again; done again by Redo; the mock engine does what the controller does, as far as the fixture can say. In parts, each its own commit
  (D37). *Not done in this step (step 35 did the first two):* undo of a preset (`LoadPreset`), of another instrument put in a lane (`ReplaceInstrument`), and not done at all yet: of the TUI's own edits that have
  no intent, and of cables that drive each other's depths.
  - **34.1 as built (95a9427).** The audio thread puts a cable at a place: `GraphCommand::InsertModTarget` and `InsertGroupModTarget`, the inverse of
    `RemoveModTarget` (the cables that drive the depth of a later cable of the modulator are told the place it has now, before the cable goes in; a place
    past the end is the last). 11 tests, 13 mutations all caught.
  - **34.2 as built (f184ce0).** The model hands back what it took out: `SessionModel::remove_edge` returns `RemovedCable`s (the cable, its place, what it
    was centred on, and `still_driven`) and `insert_target_at` is its inverse; `centre_of`. 10 tests, 17 mutations (16 caught, 1 that makes no difference).
  - **34.3 as built (bacec7c).** The controller's journal (`app/journal.rs`: `Journal`, `Inverse`, `Controller::rewind`), `Plan::Removed` and `Step::removed` (a step that holds
    what it took, replaced on each redo), the mock engine's removal steps (`client/cables.ts`, `Taken`, `removedStep`), the fixture (scripts of cables, a script with a session
    of its own for cables that drive each other's depths) and the browser tests. 13 Rust tests, 15 Vitest, 1 browser test; 24 + 15 mutations.
  - **Found on the way (34.3).** When the first of two cables on one parameter is taken out, the controller's layout of the telemetry names the next cable's slot
    for the parameter (it names the first cable's, whichever are left) and the mock engine keeps the old slot (the layout does not name the slot of a cable that
    is not the first, so the mock does not know it): nothing reads it, and the fixture keeps clear of it. TODO: the mock could keep each cable's slot.
  - **34.4 as built (9bb5a5a).** The audio thread puts a modulator at a place: `InsertModulator` and `InsertGroupModulator` take the place they are given, and a modulator that goes in
    before others tells the cables that address them the places they have now, before it goes in (`shift_cross_mod_after_insert`: a sibling's cables to a modulator's rate, stages and
    the depths of its cables; `shift_group_member_mod_after_insert`: for a lane that is a member of a group, the group's cables to its modulators). One that goes in at the end moves
    nothing: a session is loaded a modulator at a time, and the cable of an earlier one can address one that comes later. 13 tests, 13 mutations all caught.
  - **34.5 as built (3aa4a4f).** The model hands back a modulator with its own cables (`SessionModel::remove_modulator_at`) and puts it back (`insert_modulator_at`), by the same rules,
    the same one for the end. 6 tests, 15 mutations all caught.
  - **34.6 as built (703d2f9).** `RemoveNode` of a modulator is a step (`Plan::Removed`, the words of the button that adds one like it, `ModulatorKind::of`): the controller writes down
    the cables that address it, and then the modulator with its own cables and what each was centred on (`Inverse::Modulator`), and undo puts them back in the reverse order; the mock
    engine does the same (`planOf`), the fixture has six scripts more, the TUI's `d` on a modulator is a step that Ctrl+Z puts back, and a browser test of the real program checks the
    saved file (the file after Undo is the file before the removal). 11 Rust tests, 6 Vitest, 1 browser test; 19 Rust and 7 Vitest mutations, 2 of them that make no difference (either
    half of "the cables that go with a modulator", its own and those that address it, finds a cycle through the other).
  - **Found on the way (34.4 to 34.6).** The full matrix, run before the engine's part was committed, found that the first `InsertModulator` moved the cables that address a place beyond
    the rack when a modulator went in at the end, which broke the load of every session that has a cable to a modulator that comes later (three tests of the controller, "as loaded"):
    it moves nothing now, in the audio thread and in the model, with a test each for a lane's rack, a group's and a member's. A swap of two lanes (the TUI's Shift+Up and Shift+Down on a
    lane, which is no intent) would have had a step put what it took out into whatever lane is at its place afterwards: the swap forgets the history (`acdee64`), and so does every
    method of the controller that takes something out, moves it or puts a lane in a group, with no intent being carried out (`32681fb`). What a cable that drives nothing (a file can have
    one, to the depth of a cable that is not there) is centred on is not kept by undo; it makes no difference to what is heard.
  - **34.7 as built (78080bc).** The audio thread hands what an effect's removal takes out to a channel that asks for it: `RemoveEffect` and `RemoveGroupEffect` carry `keep: Option<Sender<Box<dyn Plugin>>>`
    (`hand_back`: a `try_send` to it, and to the return channel, as always, if there is none, nobody listens or it is full; the audio thread never waits), and `InsertGroupEffect` tells the group's cables to the bus
    effects at the place it goes in, and after it, the place they have now (`shift_group_bus_after_insert`, as `InsertEffect` did for a lane's chain). 11 tests, 10 mutations all caught (one by a hang: a blocking send).
  - **34.8 as built (9fbfb95).** The model puts an effect back in a chain at its place (`SessionModel::insert_effect_at` and `insert_bus_effect_at`, with `shift_slots_after_insert`, `shift_member_slots_after_insert` and
    `shift_group_bus_after_insert`: the inverses of the fixups for a removal, which tell the cables of the lane's modulators, of its group and of the bus the place they have now before the effect goes in). 5 tests, 12 mutations all caught.
  - **34.9 as built (5731685).** `RemoveNode` of an effect, a lane's or a bus's, is a step in the words of its plugin (`remove Reverb`), from the page and from the TUI's `d`: the controller writes down the cables that drive it and then the
    effect (`Inverse::Effect`: the model's `PluginSlot`, its place, and `Kept`, the plugin that was in the chain, which the audio thread hands to a channel made for it), and undo gets the plugins first (it waits for them, 250 ms in all,
    `recovery_wait`; a plugin that did not come, or that the history let go of, is loaded again with its preset and the values the user set; one that does not load, or has other outputs than the chain's, refuses the undo and the step
    is dropped), and then puts everything back in the reverse order, the effect at its mix. The history keeps the plugins of the newest 8 steps only (`MAX_KEPT_PLUGINS`), what an undone add took out is not held on to, and
    `Controller::shutdown` lets go of what the history holds before the plugin runtime goes. The mock engine does the same (`planOf`), the fixture has four scripts more (and the scripts of what cannot be undone take a group out,
    since an effect is no barrier now), the TUI's `d` on an effect is a step that Ctrl+Z puts back, and a browser test of the real program checks the saved file; one Rust test runs a real audio thread on the null backend to see
    the plugin come back across threads. 16 Rust tests, 3 Vitest, 1 browser test; 39 Rust mutations (37 caught, 2 that were taken for no difference: a preset that is not loaded again, which makes none for the plugins a test can load, since the model's
    values are set over it and the built-in presets set nothing else, and the count of what an evicted step keeps, which was taken for none since an evicted step is always older than every step that is kept, and is not: 34.15 found
    that a big step can be let go of while a smaller, older one stays, and has the test that catches it) and 4 Vitest, all caught.
  - **Found on the way (34.9).** The matrix found a test helper whose type was too complex for clippy and two fields of a test's structs that were only there to be dropped. The test of the order of the drops at shutdown could not tell whether
    the history had been let go of (it said the order of the devices and the plugin, which holds either way): it has a subscriber now, which is dropped after the plugin runtime, to be told from. The controller's layout of the
    telemetry lists a cable by its modulator's place in the rack and the mock engine's lists one that is drawn after the others (f114): a cable of an earlier modulator is told in another order, which no reader depends on, and the
    fixture's scripts draw from the last modulator of the rack. TODO: the mock could keep the layout in the rack's order.
  - **34.10 as built (ecd8116).** The audio thread hands a lane back whole and puts it back: `RemoveInstrument { keep: Option<Sender<KeptLane>> }` (`hand_back_lane`: a `try_send`, and the plugins by the return channel,
    as always, if there is none, nobody listens or it is full) and `InsertInstrument { index, lane }` (the pattern players are told their places, a member tells its group's cables to the members from its place on their
    ordinals, what the lane counted as held and what its pattern was playing is forgotten, a group that is not there is no group); `KeptLane::take_plugins` and `put_plugins` (a lane that gave up its plugins is turned away
    until it has plugins again). 10 tests, 28 mutations all caught (one by a hang: a blocking send).
  - **34.11 as built (54765ae).** The model puts a lane back among the lanes at its place (`SessionModel::insert_lane_at`, by the rules of `InsertInstrument`). 3 tests, 6 mutations all caught.
  - **34.12 as built (5ac5d5f).** `RemoveNode` of a lane is a step, in the words of its instrument (`remove the lane of Oscillator`, `remove an empty lane`), from the page and from the TUI's `d`: the controller writes down the
    cables of its group to it and then the lane (`Inverse::Lane`: the model's `InstrumentNode` and the `KeptLane` the audio thread hands back), and undo gets the lane first (it waits `recovery_wait`; a lane that did not come
    refuses the undo, and one that gave up its plugins has them loaded again into it), then puts everything back in the reverse order, and tells the lanes the tempo. The journal is generic over what it keeps (`Kept<T>`),
    one function makes the channel (`channel_for`), and a test that has no audio thread has its lanes handed back with no plugins in them (`KeptLane::empty`, from the stub engine's `drain`), which makes the controller load
    them again. The mock engine does the same (`planOf`), the fixture has four scripts more and its runner reads the engine's commands after each op, the TUI's tests say a lane is a step that Ctrl+Z puts back, and a browser test
    of the real program checks the saved file. One Rust test runs a real audio thread on the null backend to see the lane come back across threads. 12 Rust tests, 5 Vitest, 1 browser test; 28 Rust mutations (25 caught and 3
    that were taken for no difference: a lane that was let go of and is counted by what the model says, which 34.15 has a test for (see 34.9); the arm that lets go of a lane that is in hand, which is not
    reached, since a step that has let go keeps no plugins; and the outputs of an effect that is loaded again into a lane, which no plugin a test can load gets wrong) and 6 Vitest, all caught.
  - **Found on the way (34.10 to 34.12).** The `d` of the TUI on the instrument of a lane empties the lane (nothing puts that back, so it empties the history) and on a lane that is empty takes it out (a step): the TUI's
    test of what `d` forgets keeps the first and has a test of its own for the second. The "×" of a lane asks first and says "for good", from the time that nothing could be undone; it can now, and the words are to be
    changed (a group's still cannot). A note that is held while a lane is taken out is not released in its instrument when the lane comes back (as when a lane's range is changed under a held note): the lane counts none as held
    and its pattern plays nothing, but a plugin that was not sent the note-off keeps its voice. TODO: an all-notes-off for the lane when it comes back.
  - **34.13 as built (534bca6).** The audio thread hands a group back whole and puts it back: `RemoveGroup { group, keep: Option<Sender<KeptGroup>> }` (`hand_back_group`: a `try_send`, and the bus effects by the return channel,
    as always, if there is none, nobody listens or it is full) and `InsertGroup { index, group, members }` (the lanes in the groups from that place on are told the group they are in now, and the members, places among the lanes,
    are put in it); `KeptGroup::take_plugins` and `put_plugins` (a group that gave up its bus effects is turned away until it has them again). 8 tests, 18 mutations all caught (one by a hang: a blocking send).
  - **34.14 as built (101ff1c).** The model puts a group back among the groups at its place (`SessionModel::insert_group_at`, by the rules of `InsertGroup`). 3 tests, 7 mutations all caught.
  - **34.15 as built (3b61e32).** `RemoveNode` of a group is a step, in the words of its name (`remove Pad`, or `remove Group 2` for a group that has none), from the page and from the TUI's `d`: the controller writes down the lanes that are
    its members (by node, read before it goes), takes the cables of its modulators out, each written down as a cable, and then the group (`Inverse::Group`: the model's `GroupNode` and the `KeptGroup` that the audio thread hands back);
    undo gets the group first (`recover_group`: it waits `recovery_wait`; a group that did not come refuses the undo, and one that gave up its bus effects has them loaded again into it), refuses a group, an effect, a modulator or a cable that
    is there already and a member that is gone or is in a group, and then puts the group in at its place with its members, and its cables after it. No removal of a node is a barrier any more: `Plan::Barrier` is not made for a node (the arm
    that made it was not reached once the group was a step, and is gone), and the page's `planOf` takes a group out as a step unless its own cables drive each other's depths. The fixture has three scripts more (53 to 56), the TUI's tests say that
    a group that `d` takes out is a step that Ctrl+Z puts back, with its members and its cables, a browser test of the real program checks the saved file, and the tests of what cannot be undone, which took a group out, take a preset or a group made of a lane that is in another group
    instead. One Rust test runs a real audio thread on the null backend to see a group come back across threads. 11 Rust tests, 3 Vitest, 1 browser test; 28 Rust mutations, all caught in the end (two survived the first run and have
    tests now: the words of a group that is not the first, and the count of what a step that let go of its plugins keeps) and 7 Vitest, 5 caught and 2 that make no difference (which end of a cable is the group's, and the cables that
    go with it before the test of a cycle, which the test finds without them).
  - **Found on the way (34.13 to 34.15).** The count of what a step keeps was tested only through the cap of eight, and the mutations of it that 34.9 and 34.12 took for no difference were not: the loop that lets go of the plugins of the
    steps over the cap goes on to the older steps, so a big step in the middle can be let go of while a smaller one before it stays, and what a step that has let go of its plugins counts then matters. A test of the count for an effect, a lane
    and a group, before and after they let go (7d7bee6), catches all five mutations of it; the two entries above say so. The words "for good" in the page's question before a lane or a group is taken out are from the time before there was an undo (the TODO in CLAUDE.md is the user's to decide).
  - **Limits, as built.** A lane or a group that the audio thread did not hand back within `recovery_wait` (250 ms) refuses the undo with the reason: it cannot be built again from the model, as a plugin can be loaded again. A note that is
    held while a lane or a group's member is taken out is not released in its instrument when it comes back (TODO: an all-notes-off). Cables that drive each other's depths (only a file can make them) are not put back, and the removal forgets
    the history. What the TUI does to the structure with no intent (`clear_instrument`, `set_instrument`, the order of the lanes) forgets the history too, and its own edits that have no intent (the pattern, the tempo, the Type row) are not in it. The TUI has no key that takes a
    single cable out. The undo of a removal gives back the slots of the telemetry the nodes had, and the redo of an add makes new ones (4,096 of them, never reused). The controller's layout of the telemetry and the mock engine's list a
    cable drawn later in another order (f114), which no reader depends on. Only Chromium was tried, and what the audio thread did was checked through the master's meter on the null backend and by the tests' own graphs, never on a sound card.
  - **Tests.** Rust: 145 more than at the end of step 33 in the default configuration (1267 against 1122), and the same 145 in each of the others (1425 with `web`, 1262 and 1418 without the default plugin formats); the real program's
    22 and the shell's 3 as before. Web: 1523 in 74 files (1491 in 73); 73 in browsers (68). The mutations of each part are in its entry above.
  - **Numbers at the end of the step.** Rust 1267 tests, 1425 with the `web` feature and 22 on the real program, 1262 without the default plugin formats and 1418 with `web`, and 3 of the shell's in each; web 1523 in 74 files; 73 in
    browsers; 319 commits on the branch before this entry.

- [x] **35. Undo, what is replaced (phase 1).** A preset loaded from the "≡" menu and an instrument put in a lane from the "♪" menu emptied the history (so does a group made of a lane that is in another group, which this
  step leaves), and both are used a lot: one Ctrl+Z after them found nothing, and everything done before them could no longer be undone. They replace something and are put back by what they replaced (D37). *Done when:*
  an instrument that another replaced (the very plugin that the audio thread hands back, with its values, the lane's note remap and the cables that drove it) and a preset that was loaded (the preset the plugin had and every
  value that the load overwrote, with what the cables that drive it are centred on) are put back by Undo, from the page and from the TUI, and done again by Redo; the mock engine does what the controller does, as far as the
  fixture can say. In parts (35.1 the engine, 35.2 the instrument, 35.3 the preset), and a bug of the mock engine that was found on the way. *Not done (later steps):* a group made of a lane that is in another group and a lane
  put in a group or out of one whose modulators drive it (the cables of the group that has it go), the TUI's own edits that have no intent, and cables that drive each other's depths.
  - **35.1 as built (b30ea19).** The audio thread hands an instrument that a swap replaces to a channel that asks for it: `SwapInstrument` carries `keep: Option<Sender<Box<dyn Plugin>>>` (`hand_back`, as for an effect that
    is taken out: a `try_send`, and the return channel, as always, if there is none, nobody listens or it is full; a lane that had no instrument hands nothing back). 4 tests, 5 mutations all caught (one that did not build was
    made again).
  - **Found on the way (35.2): the mock engine, 31a5c73 (f116).** When a step that took something out was redone, the mock engine kept the session it had written down the first time (`Step.taken`: the nodes, the cables and the
    layout of the telemetry) and put that back at the next undo, where the controller writes down again what each redo takes. The two differ when what was taken out was made by an add that has been undone and redone since (the
    redo of an add makes new slots of the telemetry). Found by the fixture script of a replaced instrument in a lane that an add had made; it has a script of its own and a test, both of which failed before the change. 5
    mutations of the fix, 3 caught and 2 that make no difference (only the telemetry refreshed, since the graph is the same in both lives; and the refresh on undo, which is never reached).
  - **35.2 as built (8e3c589).** `ReplaceInstrument` is a step, in the words of the button that puts an instrument in (`change the instrument to Oscillator`, by the name the catalog has for the new one, or `put Oscillator in a
    lane` for a lane that had none), from the page and from the TUI's `i`: `Controller::install_instrument` makes a channel for the instrument that goes (when the lane had one) and writes down, as `Inverse::Instrument`, the
    model's plugin (values, preset, pins), the lane's note remap and pitch bend range (which go with the instrument they were for) and what keeps the plugin, after the cables that drove it, each written down as a cable. Undo gets the
    plugin first (`recover_plugin`, now told what the place asks of a plugin: `has_audio_output` for an instrument, the chain's outputs for an effect), refuses a lane that is gone or that its node is no lane any more, swaps the
    plugin in with the remapper made again from the model, or empties the lane (`ClearInstrument`) if it had none, and puts the cables back after it. The history keeps the plugins of the newest eight steps, instruments included.
    `set_instrument` forgets the history only when no intent asks for it. The page's `planOf` and the mock engine do the same, the fixture has five scripts more (57 to 62: among what was set before and after, a lane that nothing
    drives, a lane that an add made, what is refused, and a session of its own with a note remap, the cables of a group and of the lane, and an empty lane), the tests that took the replacement for what cannot be undone take a group
    made of a lane that is in another, the TUI's `i` is a step that Ctrl+Z puts back, and a browser test on the real program checks the saved file. One Rust test runs a real audio thread on the null backend to see the instrument come
    back across threads. 8 Rust tests, 5 Vitest, 1 browser test; 24 Rust mutations, all caught (two survived the first run and have tests: a node that has the id of a lane and is none, and an instrument with no audio output that was
    handed back) and 6 Vitest, all caught.
  - **35.3 as built (4ec274c).** `LoadPreset` is a step, in the words of the preset (`load preset Small Room`, by the name the plugin has for the id), from the page and from the TUI's `p`: `Controller::load_preset` writes down,
    before the model is given the preset's values, the preset the plugin has and the rows that the preset changes or that the user set over what they had (`rows_a_preset_changes`: the preset that is loaded again would put its own
    over the user's), as `Inverse::Preset`. Undo has the audio thread load the preset again, if the plugin has one by that name (a name that a session file gave and the plugin has not is kept as the name), gives each row its
    value and baseline in the model and sends the values (`SetParameter`, which centres the cables that drive them, so that nothing else is told), and refuses a plugin that is gone, a bus effect's and a lane that has no instrument
    any more. A node with no such plugin, a bus effect and an id the plugin lacks are refused, with no step. The page's `planOf` and the mock engine do the same, the fixture has three scripts more (62 to 65), the tests that took
    a preset for what cannot be undone take a group made of a lane that is in another, the TUI's `p` is a step that Ctrl+Z puts back, and a browser test on the real program checks the saved file. 7 Rust tests (and one fewer: the old one
    that said a preset empties the history), 4 Vitest, 1 browser test; 23 Rust mutations of the first version, 17 caught: the others led to a simpler choice of rows (two of the conditions were each implied by the rest) and to
    tests (the lookup of the plugin, and the plan of what is refused); 7 mutations of the final code, all caught; 6 of the page's plan, 5 caught and 1 that is the same code written otherwise.
  - **Limits, as built.** An instrument comes back as the very plugin only if the audio thread handed it back within `recovery_wait` (250 ms) and the history kept it (the newest eight plugins): else it is loaded again from its id,
    its preset and the user's values, and what it held apart from them is lost (`Plugin` has no state to save). Redo of a replacement loads the new instrument from the catalog again (a pause for a heavy plugin, on the thread that
    asked), since the one that undo swapped out goes by the return channel. A note that is held while an instrument is swapped is not released in the one that goes out or in the one that comes back (TODO: an all-notes-off). A
    preset that is put back gives the plugin its parameters and the preset it had, and no more: a plugin that had no preset keeps whatever the preset that went set apart from its parameters, and a plugin that loads a preset
    slowly loads the one it had again as slowly (on the audio thread, as before). Undo of a preset sends a `SetParameter` for each row the preset changed, through the channel of 64 commands that the audio thread empties every
    buffer: a plugin with thousands of parameters makes the main thread wait for it for a moment (as any burst of commands does, and for as long as the audio thread does not take them). The TUI's `n` and then a choice is two steps, and `d` on an instrument (the TUI's `clear_instrument`, which has no intent) still empties
    the history. A group made of a lane that is in another group, and a lane put in a group or out of one whose modulators drive it, still do.
  - **Tests.** Rust: 18 more than at the end of step 34 in each configuration (1285 against 1267 in the default one; 1443, 1280 and 1436 in the others); the real program's 22 and the shell's 3 as before. Web: 1533 in 74 files (1523);
    75 in browsers (73). The fixture of the history has 65 scripts (56). The mutations of each part are in its entry above.
  - **Numbers at the end of the step.** Rust 1285 tests, 1443 with the `web` feature and 22 on the real program, 1280 without the default plugin formats and 1436 with `web`, and 3 of the shell's in each; web 1533 in 74 files; 75 in
    browsers; 324 commits on the branch before this entry.

- [x] **36. Cables to the depth of a cable, from the page (phase 3).** The controller has taken a cable to the depth of another since step 26 (`AddModEdge` with a `depth` port) and the TUI's `t` makes them, and the page neither drew
  such a cable nor offered it (f83): a session that the TUI had cabled that way had cables that the canvas left out. *Done when:* on the real program a browser drags a cable from a modulator's output to the port of a row of
  another modulator's cable, or picks it from the list at the foot of the modulator, under the modulator whose cables they are; it is in the server's session, the saved file has it (`mod_depth = [0, 0]`), undo takes it out and redo
  draws it again; a cable that is taken out takes the cables that drive its depth with it, and undo gives them all back; the modulator that drives the depth of a cable is a row above the one whose cable it is; the cable runs round
  the node it ends at, with its slider between the two nodes; and the cable and its slider are named by the whole of what it joins. In parts, each its own commit: **36.1** a modulator's node lists its cables, **36.2** the cable is
  drawn, **36.3** it is routed round the node; and a browser test of what undo does to such cables. *Not done (later steps):* the depth that the cables which drive a depth make of it is not shown (a cable to a depth has no slot of
  telemetry), a cycle of cables that drive each other's depths cannot be made from the page (a file can, and its removal is not put back: D37), and the slider of every other cable is still at the middle of it.
  - **36.1 as built (5c57bde).** Every cable that starts at a modulator is a row at the foot of its node (`CableRows` in `app/patch/nodes.tsx`, `PatchNodeData.cables`: the cable's id, what it ends at in words, and whether a cable
    ends at its depth), with a port on the node's left edge named after the cable (`depth-<id>`: `DepthPort`, `depthHandle`), at first one that takes no cable. The words for what a cable ends at are one function now (`endLabel`,
    `cableWords` in `client/names.ts`): a parameter or a setting is the node, a colon and its name, and the depth of a cable is told by that cable (`depth of LFO → Filter: cutoff`, D38), with a guard for cables that drive each
    other's depths. The layout counts the rows: `nodeHeight(node, knobs, cables)` adds `CABLES_GAP` (8) and `CABLE_ROW_HEIGHT` (20) for each, which a browser test measures against the mock engine and the real program (a row is 20 px;
    the node of the tracer's LFO is within the 12 px of allowance of the height the layout assumes), and the layout tests that counted a modulator's height without its cables count them. 22 Vitest tests and 1 browser test, red
    first; 27 mutations of the code, 26 caught and one that is the same code written otherwise (`nodeHeight` ignores the count of rows for a node that is no modulator).
  - **36.2 as built (abc6465).** The ports take the cable. `handleOf` names the depth (`depth-<id>`) and is total (there is no port without a handle, and the branches that asked are gone); `portAt` reads it from a connection;
    `drivablesOf` offers the depths of the cables of the modulators in reach, under the modulator whose cables they are, each as `depth → <where the cable goes>`, and what a modulator drives of them is keyed by the cable
    (`fieldKey`: a modulator has a depth for each cable, so it can still be drawn to the others' and another modulator can drive the same one). A cable is dragged from a modulator's output and let go over a row, anywhere in it (the
    port grows to the row while one is dragged, as a control's does to its cell), or picked from the list; `tiersOf` and `modulatorColumns` see the cable as an edge to a handle, so the modulator that drives the depth of another's
    cable is a row above it, in its column. The editor tells cables with `cableWords`, as the canvas does ("Could not draw a cable from ADSR to depth of LFO to Filter: cutoff: ..."). The tests that pinned that a depth has no port
    say what it is now. 20 Vitest tests added (9 of the old ones renamed or replaced) and 6 browser tests (the mock engine: drag to the port, let go anywhere over the row, own lane and not another's, the keyboard; the real program: what the server and the saved
    file have, undo and redo, and the cables that go with a cable whose depth they drive); 22 mutations, all caught.
  - **36.3 as built (9492999).** A cable that ends at the left edge of a node is drawn round it (`cableRoute`: the library's smooth-step path, down from what drives it, along under it, down beside the node and in, with 24 px of
    room), and its slider is on the run under what drives it, half way to the node, where nothing is; before, the curve crossed the node and the slider was over its controls. Every other cable is the curve it was. 5 Vitest tests
    and 1 browser test (the path is sampled: no point of it is inside a node, it comes in at the left edge, and the slider and the button that takes it out are between the two nodes); 9 mutations of the route, 8 caught and one
    that makes no difference (a port exactly the room under the output, where the library puts the slider where the route does), and the mutant that draws the curve again is caught by the browser test.
  - **Found on the way: the message of the commit of 36.2 named a limit that was not one.** It said that the removal of a cable whose depth another cable drives cannot be undone; only a cycle of cables that drive each other's
    depths cannot (D37), and the page cannot make one. The message was written again before anything was pushed (the trees are the same). A browser test on the real program checks what undo does to such a cable (b4b75aa): the cable
    and the one that drives its depth are put back at the depths they had, and redone.
  - **Limits, as built.** A cable to a depth has no slot of telemetry (as no cable to a modulator's own field has), so the slider of a cable whose depth is driven shows what was set and not what the cable that drives it makes of it;
    the label of a row is cut when it is long (its title has all of it); the slider of a cable whose port is not below what drives it (a node that was moved) is where the library puts it; a modulator with many cables is as tall as its
    rows; a group's modulator that drives the depth of a member's modulator's cable is checked by the unit tests and by the fixture of the controller (`fields.json`), not in a browser; only Chromium was tried.
  - **Tests.** Web: 1571 in 75 files (1533 in 74); 84 in browsers (75). Rust is as it was at the end of step 35: nothing in it changed. The mutations of each part are in its entry above.
  - **Numbers at the end of the step.** Rust 1285 tests, 1443 with the `web` feature and 22 on the real program, 1280 without the default plugin formats and 1436 with `web`, and 3 of the shell's in each; web 1571 tests in 75 files;
    84 in browsers; 329 commits on the branch before this entry.

## 13. Later: track B (`builtin:patch`)

A patchable synth that needs no host changes: a `builtin:patch` instrument that
holds its own voice graph (oscillator, filter, VCA, ADSR, LFO, mixer, …) with
typed cables (audio / control / gate). To the host it is just another `Plugin`,
so lanes, groups, modulators and the TUI are untouched. The graph is compiled on
the main thread into an immutable program and swapped on the audio thread
through the same send-in / return-old mechanism plugins use. It exposes macro
parameters to the host, so lane modulators and the TUI can drive it. The canvas
edits its inner graph, reusing the node / edge shapes from §4.2. Scope it after
the phase 0 tracer shows what the canvas wants; richer builtins (noise, VCA,
mixer, delay, …) are the natural first increment.

### Sources as nodes (ideas)

Two ideas for after the canvas can build what the engine has now; neither is started.

**A keyboard as a node.** Today every note goes to every lane (`NoteInput`: hardware MIDI, the virtual piano and the browser's keys are one
path) and each lane filters it by its own key range (`InstrumentLane.range`, `SetRange`). A keyboard node (the computer's keys, an on-screen
piano, a MIDI device) with a *MIDI cable* to a lane would take the range off the lane and put it on the keyboard, so that several keyboards
with ranges of their own (a split, a layer, one keyboard to two synths) are several nodes, and anything that sends notes can be a node as well:
a pattern, an arpeggiator, Web MIDI from the browser, a MIDI file. In the model it is a node kind and a port kind next to audio and modulation
(`NodeKind::Keyboard`, `Port::MidiOut` / `MidiIn`, `EdgeKind::Midi`); in the engine the global fan-out and range filter become a routing table
per lane (the channel-based routing of per-note expression stays as it is).

**Audio input as a source.** The engine has an output stream and no input. A lane whose chain starts at an input of the sound card (a
microphone, a line in) in place of an instrument would run through the same effects, groups, modulators and meters; an envelope follower is
the modulator that goes with it (what the input puts out, as an LFO's output is). From there, Tang as a plugin (VST3 or CLAP) whose buffer
comes from the DAW: `audio::Renderer` is already the one piece of code that the sound card's callback and the null backend run, so a plugin's
`process` would be a third thing that drives it, with the input as the source node's buffer.

## 14. Decisions and open questions

| # | Decision | Status |
|---|---|---|
| D1 | Browser is a front-end to the same process; no audio in the browser; the TUI stays first-class | Agreed |
| D2 | v1 matches the Session and Piano tabs | Agreed |
| D3 | Track A (rack-as-patch on the existing engine) first; `builtin:patch` later | Agreed |
| D4 | Framework: robust, expandable, snappy → TypeScript + React + Vite + React Flow + Zustand, framework-agnostic client layer | Proposed (criteria agreed) |
| D5 | Controller is a plain struct driven by the owner loop; the web runtime talks to it by channels (no shared model, no locks) | Proposed |
| D6 | `axum` / `tokio` behind a `web` feature; Node in CI; assets embedded in the binary | Assumed |
| D7 | Graph-shaped wire model with stable `NodeId`s; layout stored in the session TOML as `ui` tables | Proposed |
| D8 | Telemetry via lock-free slots, binary frames; undo via inverse intents + recovered plugin instances | Telemetry built (step 6); undo of values built as D35, of what is added as D36, of what is taken out as D37 (cables, modulators, effects, lanes and groups, with the plugin instances, the lanes and the groups that the audio thread hands back) and of what is replaced (an instrument, with the plugin instance that the audio thread hands back, and a preset), also as D37 |
| D9 | Short design + TODOs in `CLAUDE.md`, long form here | Assumed |
| D10 | `tang serve` listens on `127.0.0.1:8765` by default; beyond loopback `--web-bind` and a token made for the run | Built (step 10) |
| D11 | One current-thread tokio runtime on its own thread for the server and the pusher; `ctrlc` for stopping a headless process | Built (step 10) |
| D12 | The client's store is vanilla zustand (no React in `client/`), the mock engine speaks the protocol through the same `Transport` as a socket, and the fixture the mock serves is generated from the Rust types and kept current by a Rust test | Built (step 11) |
| D13 | `web/dist` is embedded with `rust-embed`, with a committed `.gitkeep` so that the program builds without Node; the interface's content security policy has no `unsafe-*` | Built (step 11) |
| D14 | Telemetry never passes through React: one frame loop, readers that find their slot once for each layout, DOM writes only when a value moved; controls are controlled components, and what a control shows while the server catches up, and the one intent a frame, are the `Drive`'s | Built (step 12) |
| D15 | React Flow draws the canvas; `toFlow` is a pure function of the graph, nodes are functions of their data with controls that read their own values, and the canvas is laid out again only when the structure of the session changes | Built (step 12) |
| D16 | The TUI loop selects on a reader thread's channel, the browsers' requests and a tick, on the one controller; what the TUI changes without an intent is found by comparing what the subscribers were told with the session (`resync`, a snapshot at a revision of its own), until every edit is an intent | Built (step 13) |
| D17 | `--web[=ADDR]` is a flag of the TUI alone, with `serve`'s arguments (`127.0.0.1:8765`, `--web-origin`, a token beyond loopback); the address comes only after an `=`; the address and the warnings are printed before the TUI starts and shown in the tab bar and on the Help tab | Built (step 13) |
| D18 | The first of phase 3, a modulator's settings and a cable's depth, is done ahead of phase 1 (session parity): it is what the web UI is for ("build synths and control parameters with visual modulators"), it needs no structural intents, and it exercises the whole path (intent, controller, engine, event, reducer, control) on the thing the user wants to see | Built (step 14); say if phase 1 should have come first |
| D19 | What the client has to know about what the controller clamps to (the ranges of the gains, of a modulator's settings and of a cable's depth, what a new modulator starts as, the order of the waveforms) is generated into `web/src/protocol/generated/constants.ts` from the Rust that clamps, and checked by the test that checks the generated types; the client's mock engine and its controls read it and keep no copy | Built (step 14) |
| D20 | An intent that adds or takes out nodes or cables has no event a client could apply: `Applied.restructured` is set and the subscribers are given a snapshot at once, at the revision after the intent's, so that the controller's projection of the graph (the order of the nodes, the audio path, the layout of the telemetry) is the only one; the structural events of §5 are not built | Built (step 15) |
| D21 | Where a cable may end is the model's rule (`SessionModel::param_targets`: a lane's modulator reaches its lane's chain, a group's its members' chains and its bus effects); `AddModEdge` refuses the rest. A client that marks the valid ports of a cable being dragged reads the rule from the graph by the `parent` of each node (`reachFrom`), and a fixture the controller writes (`reach.json`) pins the two together, so that they cannot drift | Built (step 15) |
| D22 | Replacing a lane's instrument takes out every cable that drove the old plugin's parameters (a cable names its parameter by the plugin's own index, so one kept would drive whatever the new plugin has at that index); keeping the cables whose parameter the new plugin has under the same name is a later refinement, a TODO in `CLAUDE.md` | Built (step 20) |
| D23 | The tests own the scene they take for the known one (`web/src/client/fixtures/tracer.toml`), and the examples are for people: a Save in the page writes to the file the program serves, and the way to try the program is to serve an example | Built (step 20) |
| D24 | A preset is chosen from a menu on the node of the plugin that has it, the one loaded last marked and the menu opening on it; the knobs take the values from the `PresetLoaded` event, which every client is told, and the page reads nothing of the preset itself (a plugin of the machine's is not the page's to open); a load must also move what the cables that drive the plugin are centred on, which is the engine's to do (f64); a bus effect is offered none until the audio thread can load a preset into a group's chain | Built (step 21) |
| D25 | All the parameters of a plugin are reached through one panel docked beside the canvas, with a filter by name, and not by letting a node grow: the layout of the canvas stays a function of the graph (no node changes size by a click), a plugin with thousands of parameters costs a page of rows, and a row is the same session parameter as a knob (one `Control` for both: what was set shows until the engine has answered) | Built (step 22) |
| D26 | A lane's keys are chosen in the page as two notes, `any` or a note each, and are never typed as text (`C0-B3`): the text is for the session file and the TUI, which have one parser; the intent carries numbers, a choice that would cross the other end pushes it along instead of being refused, and the whole keyboard is no range | Built (step 23) |
| D27 | The piano of the page draws what the server tells and decides nothing: the scale arrives as its root and pitch classes (the page holds no table of scales), the held keys as a telemetry channel of 128 values at the telemetry rate rather than the bitset on change that §6 planned (one frame format to read, by the note's number, and no bookkeeping of what each client was told), and the lock is the sender's (the server drops a note outside the scale from every source, so the page cannot disagree with a MIDI keyboard about it) | Built (step 24) |
| D28 | Where a node is drawn belongs to the session, not to a page: a page asks `SetNodeUi` once, when a node is put down (not a stream of moves), the controller keeps the place tidy (a tenth of a pixel and within a limit, so that a file is read by people and a node cannot be lost beyond the edge), every other page draws what the server tells, and a node that has no place is where the layout puts it, with no room left for one that was moved: a place is a pin, not a hint. The places are a side table of the model by `NodeId` and not a field of each node, so that the engine's types and the TUI do not carry what they do not use | Built (step 25) |
| D29 | A cable between modulators is the model's rule read from both sides, as a cable to a parameter is (D21): `SessionModel::modulators_in_reach` and `field_targets` decide, in the controller and for the TUI's selectors, and the page reads the same rule from the graph (`modulatorsInReach`, `fieldsOf`) to mark the ports that take the cable that is dragged, pinned to the controller's by a fixture it writes (`fields.json`) and mirrored by the mock engine in the controller's order of refusals and words. The depth of a cable was a port of the controller and not of the canvas: no cable to one was drawn or offered by the page until a node had a place where such a cable could end (a row of the node of the modulator whose cable it is: D38). A modulator that drives another is drawn above it, so that a cable runs down, and a rack in which none does is laid out as it always was | Built (step 26; the depth of a cable, step 36) |
| D30 | What a modulator has put out is the page's own record, not the server's: a `TraceBuffer` of the samples a node's frames took of the telemetry slot it reads anyway (the past of a page is from when it was opened), so that nothing is added to the wire, nothing is kept for a client that is not there, and what is drawn is exactly what the telemetry carries (30 values a second, which aliases an LFO faster than 15 Hz: f85). A trace is a canvas drawn on the frame loop and a cable's brightness is a CSS variable, so that telemetry still does not pass through React; a value that has not moved for as long as the trace shows is drawn once | Built (step 27) |
| D31 | An effect is moved by a place in its chain (`MoveEffect { node, index }`), not by a step: a client that offers steps (the page's buttons, the TUI's Shift+Up and Shift+Down) asks for `index ± 1`, one that drags asks for any place, and the controller answers both alike, a lane's chain and a group's bus by one intent and two controller methods that do the model and the audio thread together. A refusal is a refusal of the intent (a place the chain has not, the place it is at), never a silent no-op, so that a client's notice can say why. The page keeps the focus on the button that was pressed by asking for it again when a browser lets it go (`keepFocus`), rather than by keeping the DOM order of the nodes fixed, which would have left the Tab order different from what is drawn | Built (step 28) |
| D32 | An envelope's dot is placed from what the telemetry already carries, the level the envelope puts out, read with which way it was going (`courseAfter`), and not from a phase that the engine would publish: nothing is added to the wire or to the audio thread (as for the traces, D30). The page infers the stage, and is wrong in a few places that the next reading mends: the first reading of a note is a rise whichever stage it is on, a note that ends during the decay is drawn on the decay until the level is below the sustain, and the first reading after a gap is told by its place alone. A channel for the stage would make it exact; it is not worth one yet. The telemetry is slower than the page's frames, so a frame that finds the level where it was is no news and the level goes on the way it was going, until a rise has stood still for three readings (100 ms). A stage's width is on a logarithmic scale of its time (a stage is 1 ms to 10 s), and the sustain, which has no time, has the mean of the others | Built (step 29) |
| D33 | Which parameters of a plugin its node shows as knobs is the session's, kept as a list on the plugin (`pinned = ["cutoff", ...]`, by name, a key of the plugin's table beside `ui`; in the model `PluginSlot::pinned`, the plugin's own indices; on the wire `Plugin.pinned`), not the page's and not inside `ui` as §4.4 first planned: it follows its plugin wherever the plugin goes (an effect that is moved takes it along, an instrument that is replaced has none, with nothing to remember), a node can be pinned without being placed, and `NodeUi` stays a `Copy` pair of numbers that some forty places copy. One intent, `SetPinned { node, params }`, replaces the whole list: simple, idempotent, one event, and the last list wins (two pages that pin in the same moment can lose one of the pins; a pair of intents to pin and to unpin would not, and the page works out the list from what it shows, `pinnedWith`). An empty list is the front-end's own choice, the first eight, so there is no way to ask for a node with no knobs at all, which nobody needs yet; and the first pin of a node keeps the first eight, since a node that lost them at its first pin would be a surprise. A cable's target is shown for as long as the cable is there and is not a pin (§4.4 planned that attaching a cable pins its target: a pin that appeared with a cable would have been written into the session file) | Built (step 30) |
| D34 | A group is made and filled by what a front-end means, not by the steps of the TUI's popup: `AddGroup { members }` makes it with its lanes in one intent (every member is looked at before anything is made, so a refusal leaves no group and a client has no half-made state to undo), `SetLaneGroup { node, group }` puts a lane in a group or, for none, takes it out (planned as `AssignGroup`: the intent says what changes), and a lane that leaves a group lets go of it as a node that is taken out does: the group's cables to it are taken out (`take_out`, with what drives their depths) and what they drove has the value the user set (the model used to drop the targets and leave the parameters where the cable had put them, f65). A name is `RenameGroup`, which answers with an event that carries the name the group has *now* (a name of nothing is the default, "Group" and its place), so that no client works out a default. A lane or a group is taken out by `RemoveNode` (the controller keeps one lane: `a session keeps its last lane`), and since there is no undo the page asks first, in a menu that opens on "Keep" and says in words what goes and what stays, and does not offer it on the only lane; the controller does not ask, since a client that is not a person (a script, a test) has no one to ask | Built (step 31) |
| D35 | Undo is a history that the controller keeps, not an `undo` on every answer: for each intent that has an inverse the controller reads from the model what the intent will overwrite, before it is applied, and keeps (in a list of 200) the intents that put it back and the intents that do it again, with the words of a button for the step. `Undo` and `Redo` are intents like the rest, each intent of a step a revision of its own, so that every front-end (a page, the TUI, a script) uses the one history and what was set in one can be put back from another. A change of one thing a second or less after the last of it is one step, so that a drag is one; a change after an undo ends the redo; what has no inverse yet (the structure, presets, the instrument of a lane, a lane's group) empties the history, so that Undo never does something other than the last thing that was done. The history is the program's, so what Undo and Redo would do is told as a message of its own, `History { undo, redo }`, when a client subscribes and when it changes, before the answer to the request that changed it, and not as an event (a second event at one revision is taken for a repeat). The mock engine keeps the same history, pinned to the controller's by a fixture the controller writes, and the limits (200, a second) are generated into `constants.ts`, not copied | Built (step 32); what is added, step 33; what is taken out and replaced, later |
| D36 | What undo takes out and redo makes again has the ids it had. A step names the nodes and the cables of what came before it by id (a gain set on an effect that was added, the depth of a cable that was drawn), so an add that is undone and done again as a new node would leave every later step naming a node that is not there, and a step that cannot find its node is dropped. The alternatives were to rewrite the later steps with the new ids (the history is a list that every undo and redo walks, and a rewrite would have to reach each of them, in the mock engine as well) and to give a step names that are looked up when it is carried out (which a person's own changes of the structure would break): both are more than to give the ids back. So `NodeIds` has a queue of ids to hand out before new ones (`reuse`), the controller writes down the ids an intent hands out (`Made`, `made_last`) and a step keeps them (`undo_made`, `redo_made`), refused if one is in use and dropped if unused; the mock engine does the same (`client/counters.ts`) and the fixture records the shape of the session after every change of structure, so the two are the same after every undo and redo. A telemetry slot is not given again, as everywhere (§6): a node that comes back has new ones, and each undo and redo of a modulator or a cable uses some of the 4,096. A modulator and the cable to it, which the TUI's `m` and `l` ask for, are one intent (`Modulate`, step 37) and so one step; before it, the TUI joined the steps of two intents (`Controller::history_mark`, `join_steps_since`), which nothing asks for now and which are gone | Built (step 33) |
| D37 | What is taken out is put back by what was taken, not by the intents that would add it again. A removal is not the inverse of an add: an add makes a node with the defaults it has, and the removal took one that had been set up, its values, its place in its chain or rack, the cables to it and from it, and the plugin with whatever it holds. Undo by `AddEffect`, `AddModulator`, `AddModEdge` and the `Set*` that follow would put a modulator last in its rack (and the page numbers LFOs by their places), load a plugin again (a pause for a heavy one, with its own state lost, and none at all for a plugin the session loaded from a path, which the catalog has no id for), give every node new telemetry slots, and be a list of intents that has to be right for whatever a node had. So the controller writes down what an operation takes out, as it goes, in a `Journal` of `Inverse`s (`app::journal`; only while an intent is carried out, so that what the TUI or a test takes out by itself leaves nothing behind): the model hands back what it removed (`RemovedCable`: the cable with its id and the slot it publishes to, where it was, and what it was centred on, read while what it drives was there), and a step of the history that holds a journal puts everything back in the reverse order (`Controller::rewind`), each into the session as it was when its turn to go came, by an insert that is the exact inverse of the removal (`SessionModel::insert_target_at` and `GraphCommand::InsertModTarget`: what addresses a later place is told the place it has now, before the thing goes in), under its ids and its slots. Redo takes it out again and writes down what that took, which is what the next undo puts back (the step's way back is replaced each time: it holds plugin instances, which are used up by putting them back). Nothing changes on the wire: the way back is the controller's own, and the page is told what changed as it is told of any change of structure, a snapshot, and of the words of the buttons, the `History` message. A cable drives one thing, so the cables that go with one are a tree of its drivers, which go before it, unless the cable lies on a cycle of cables that drive each other's depths (only a file can make one: a cable is drawn to a cable that is there already); then one of the two goes while the other is still there, driving a place that has no cable at it for a moment, and once it has gone that place is the same number as the place of the cable after it, so the two cannot be told apart and the removal is not put back (`RemovedCable::still_driven`, `Inverse::NoWayBack`: it empties the history, as every removal did). The mock engine puts back by keeping the nodes, the cables and the layout of the telemetry as they were before the removal, which is exact because everything done after it was undone first, and decides what is not put back by the graph (`drivesRoundToItself`); the fixture has a session of its own for that script. A modulator is put back the same way, with its own cables and what each was centred on (`Inverse::Modulator`, `insert_modulator_at`, `InsertModulator`), after the cables that addressed it were put back before it, and one that goes in at the end moves nothing, since a session is loaded a modulator at a time with cables to ones that come later. An effect is put back with **the very plugin that was in the chain**, which is what D8 asked for: a plugin has state that nothing but the plugin holds (the tail of a reverb, a sample that was loaded, a patch of a plugin that has no parameters for it), and a heavy one takes a while to load, and one that the session loaded from a path has no id the catalog knows. So the controller makes a channel for each plugin that an intent takes out (a `bounded(1)`, its sender in the command, `RemoveEffect { keep }`), the audio thread hands the plugin to it where it used to give it to the channel that the main thread drops from (and to that one, as before, if nobody listens or the channel is full: it never blocks), and the journal holds the end that listens. Undo asks for all the plugins of a step before it changes anything, so that what can fail fails first: it waits for them, for 250 ms in all (the audio thread takes a command within a block or two, and has handed the plugin back long before a person has pressed Undo), and loads again what has not come (from the id, the preset it had and then the values the model has, which are the user's); what cannot be loaded, or has other outputs than the chain's, refuses the undo with the reason and the step is dropped. The history keeps at most 8 plugins, the newest steps' (`MAX_KEPT_PLUGINS`), since a plugin can be big: a step that cannot keep all of its plugins keeps none, and loads them again. A plugin is dropped on the main thread, while the libraries it came from are loaded, so the controller lets go of the history before it lets go of the plugin runtime. A lane is put back **whole**, and not built again from the model: it has state that nothing but the lane holds (the modulators' phases and envelopes' levels, the plugins and their state, the pattern and its recording, the note remap, the meter), and the loader that builds a lane from a session file does it while it loads, so that a lane built from the model would be a second implementation of that. The audio thread hands it back through the channel the controller made for it (`RemoveInstrument { keep }`, an opaque `KeptLane`), and `InsertInstrument` puts it in at its place, telling the pattern players of the lanes their places and the group's cables to the members from its place on their ordinals, as the removal told them the other way. What the lane counted as held, and what its pattern was playing, is forgotten as it goes in, since the notes were let go of without it. A lane that did not come in time refuses the undo (it has nobody to be built from), which is what a stalled audio thread comes to; the history keeps at most 8 plugins in all, and lets go of the oldest steps' by taking the plugins out of the lane it keeps, which are loaded again into it. A group is put back **whole** in the same way (`RemoveGroup { keep }`, an opaque `KeptGroup` with the bus effects and what they hold, the modulators with their state, the meter, the volume and the pan; `InsertGroup { index, group, members }`): the cables of its modulators go first, each as a cable, and are put back after the group; the group is written down with the lanes that were its members, by node, since they are in no group once it has gone (the lanes of the groups after it are told their group's new index as it goes, and the other way as it comes back); undo refuses, before it changes anything, a node, an effect, a modulator or a cable that is there already and a member that is gone or is in a group, and waits for the group as it waits for a lane and refuses if it did not come. With it every removal of a node is a step of the history. An instrument that another replaces is put back the same way (`SwapInstrument { keep }`, `Inverse::Instrument`: the model's plugin, the lane's note remap and pitch bend range, and the plugin, after the cables that drove it, which come back after it), and a preset that is loaded, which stays in the plugin, is put back from what it overwrote (`Inverse::Preset`: the preset the plugin had, loaded again on the audio thread, and each row the load changed or the user had set over what it had, with its baseline). A journal holds things by the places they had, so what the controller edits without an intent forgets the history | Built (step 34) for cables, modulators, effects, lanes and groups, and (step 35) for the replacement of an instrument and for a preset; not for a group made of a lane that is in another and a lane moved between groups whose cables drive it |
| D38 | The depth of a cable is a port in the node of the modulator whose cable it is: `Port::Depth { edge }` is an endpoint at the owner of the cable it names (the `from` node of that edge), and the node lists the cables that start at it, a row each, with the port on the node's left edge (`depth-<id>`). A port has to be in a node, since a handle is, and an edge has none; the depth of a cable is no control of its own (the cable has a slider at its middle, and a knob for each cable would make the node as tall as a plugin's); and the controller's port is per cable, so a menu has to tell one cable's depth from another's in any case, which it does by where the cable goes (`depth → Filter: cutoff`). The words for what a cable ends at are one function (`endLabel`, `cableWords`), used by the canvas, the lists and what the page says of a refusal, so that two cables of one modulator are two things to a screen reader as to the eye: the depth of a cable is told by that cable (`depth of LFO → Filter: cutoff`), and cables that drive each other's depths (only a file can make them) are not read for ever. The rows are what the layout counts (`nodeHeight(node, knobs, cables)`), and a modulator that drives a depth is a row above its owner as for a setting. A cable that ends at the side of a node is not a curve: a curve to the left edge of a node crosses it, from a modulator above, and its slider at the middle is over the node's controls; it is drawn down, along under what drives it and round the node (`cableRoute`), with the slider on the run along. The alternatives were a port on the cable's slider (nothing but a node has a handle), one port in the node for the depths of all of its cables (a cable would have to be told apart after it was dropped, and the controller takes one by id) and a knob in each row (a second control for what the slider says) | Built (step 36) |

Open questions

1. Display text for external plugin parameters (§4.3).
2. Whether the `web` feature joins the Windows CI matrix. (It does, as of step 10,
   with `--no-default-features --features vst3,web`; the first run of that will say
   whether it builds there.)
3. Worker-thread plugin loading (phase 1): completion messages, cancellation,
   what the TUI shows while a load is pending.
4. Session browser and runtime session switching (phase 4): tearing down and
   rebuilding the graph while the stream runs.
