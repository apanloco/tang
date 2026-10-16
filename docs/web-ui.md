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
moves loads to a worker whose completions `poll()` picks up. (Until then the page
says that it is waiting: a light for a request that is slow, step 40.)

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
  as an event; the tempo (as a snapshot, until it had an intent in step 49) and a new lane set in the TUI reach the browser as snapshots; a
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
  (its source: LFO waveform and rate, ADSR, a macro's value, or a MIDI control, bound or learning),
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

*As built (the VST3 adapter, after step 38):* not flags that a client has to read, but a list that leaves out what a person should not be
shown: a parameter that the plugin hides (`kIsHidden`) or says is read only (`kIsReadOnly`), and the placeholders that a plugin makes so that a
host can send MIDI controllers through its parameters (a parameter for each of 16 channels and 130 controllers, which its `IMidiMapping` points
at, and that cannot be automated: `is_listed`). Found by looking at the plugins on this machine, which all do it: 2,080 of Pianoteq 9's 2,294
parameters were "MIDI CC 3|47" (214 of its own are left), 2,080 of ROLI Airwave Player's 2,089 (9 are left), 2,078 of ROLI Studio Drums'
2,156 (78) and 2,080 of Traveler's 2,450 (370); they were 91% to 99.6% of the list in the ⚙ panel and in the terminal. The routing of
the controllers is unchanged (it goes by the parameter's id, not by the list), and a real parameter that a controller is mapped to can be
automated and stays. The count that `tang enumerate` and the selector show is the same number. A parameter of 1 to 64 steps (`stepCount`) is a
list: an enum (`labels`) of what the plugin says at each step (`getParamStringByValue`: "Off" and "On", "EXP" and "LIN", Studio Drums' "Scale Type"
with 19 scales and its "Root Note", Pianoteq's "Bounce Sync Speed" with 30), with `min` 0, `max` the last step and the step as the value that is
given and taken, since VST3's steps are k/n whatever the plain range (Pianoteq's is 0 to 1 for every parameter: read as a plain value a step 13 of 29
would be 0.45); `step_count` is reported for every parameter. A session that has such a parameter at a plain value that is not a step (0.34 for
the 10th of 29) reads it as a step; that is a list of more than two, of a plugin whose plain range is 0 to 1. Units and groups are not
reported yet: the units that a VST3 plugin gives (`dB`, `Hz`) name what its display text shows, and not its plain range, which is 0 to 1 for
all of Pianoteq's, so "0.44 dB" would lie as "0.37 Hz" would for the filter. CLAP and LV2 do not leave anything out yet.

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
the layout of the others does not leave room for one that was moved: a node dragged away leaves its space to whatever comes. A node that
the layout would put on one that was placed, or nearer to it than 16 pixels (`CLEARANCE`), is moved clear of it, below it with the gap of a
row (above it for a modulator, which belongs above its strip), and clear of the others it then comes to, one after the other in the order of
the graph: a node that is added later, an effect at the end of a chain say, does not hide one that was put there by hand (`clearOfPlaced` in
`app/patch/toFlow.ts`, f80). A node that is clear of the placed ones stays where the layout puts it, so nothing moves because another is dragged
unless it is dropped on it.

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
- Listening: `SetMute { node, muted }` and `SetSolo { node, solo }` (built, step 53, D53): a lane or a group silenced, or the only one heard (the laws are in `CLAUDE.md`,
  "Mute and solo": a mute wins over a solo, a solo carries the group of a lane and the lanes of a group, and the audio thread lets a node go or brings it back over one buffer).
  `node` is a lane's (an empty lane's too) or a group's. Refused, with nothing changed, for any other node (`only a lane or a group can be muted`, `... soloed`), and, as every intent
  is, for the master and for a node that is not there; answered by `MuteSet` and `SoloSet`, events and not snapshots, since no node or cable comes or goes. A step of the history
  in the words `mute the lane of Oscillator`, `unmute Pad`, `solo an empty lane`, never joined with the step before it (a toggle joined with its opposite would leave a step that does
  nothing), and none for a press that changes nothing. The page's cell and the terminal's `M` and `S` ask them, so that each sees the other's.
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
  (sine, triangle, saw, square, and from step 45 random and smooth), refused with nothing changed for a node that is no
  modulator, a setting its kind lacks or a value that is not a number; and the depth of a
  cable, -1 to 1, with `EdgeNotFound` for one that is gone. The events are
  `ModulatorFieldSet` and `EdgeDepthSet`, carrying the value after clamping. Step 15 built the
  four that change the structure: `RemoveEdge { edge }` (a cable, and with it the cables that
  drive its depth, and theirs in turn: left pointing at a cable that is gone they would
  drive whichever one is put there next), `AddModulator { parent, kind }` (an LFO, an
  envelope or an armed MIDI control at the end of the rack of a lane, whose node is its
  instrument's, or of a group; any other parent is refused; a macro came in step 48, and a velocity and a key, bound at once, in step 52), `AddModEdge { modulator, to }`
  (a cable at depth 0.5, `to` a node and a port of it: a plugin's `param`, where the controller
  decides where it may end, D21; or, since step 26, a modulator's `rate`, `attack`, `decay`,
  `sustain` or `release`, or the `depth` of one of that modulator's cables, where it decides by
  `SessionModel::modulators_in_reach` and `field_targets` (D29) and refuses, with nothing changed
  and in this order, a node that is not there, the modulator itself (`a modulator cannot drive its
  own settings`), a node that is no modulator, one out of reach, a setting its kind lacks (`that
  modulator has no rate`; a depth that is not one of that node's cables is `EdgeNotFound`) and one
  the modulator drives already; a cable to a setting takes a telemetry slot, for the value that setting is set to (f132; it had none until step 39)) and `RemoveNode { node }` (a modulator, with its cables and the cables
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
  scale that is set is no change there, as before). Both are saved with the session. `SetBpm { bpm }` is built, step 49 (D49): the tempo that patterns are recorded at, held to 20 to 300 (`BPM_MIN`, `BPM_MAX`, which the page reads as `BPM_RANGE`), the model's `bpm` and the audio thread's `SetGlobalBpm`, saved as the file's top-level `bpm`; refused, with nothing changed, for a value that is not a number (`NaN is not a tempo`); told as `BpmSet { bpm }` with the tempo it has now; a step of the history (`set the tempo`, a drag being one); the patterns that have been recorded keep the tempo they were recorded at. `SetRecording { node, recording }`, `ClearPattern { node }` and `SetPatternField { node, field, value }` are built, step 50 (D50): what a person does to the pattern of a lane. A recording is started as the terminal's `r` does it (a lane that has no pattern is given an empty one, four beats, looping, not in key, not heard, at the session's tempo; the audio thread is told the tempo and the length first, since the count-in and the recording are counted in them, and then to record) or stopped (at once; the pattern that a finished recording makes is the audio thread's report, which `Controller::poll` folds in and the browsers are given a snapshot of, there being no event for it yet); a recording that is started while it is going on, or stopped while it is not, is told as it is and sends nothing, since a second start would begin the count-in again. `ClearPattern` takes the pattern out (and a recording that is going on with it) and is refused for a lane that has none. `SetPatternField` has a `PatternField`, `length` (1 to 32 beats) or one of the switches `enabled`, `looping` and `transpose` (0 or 1, rounded), held to `PatternField::range`, and gives a lane that has no pattern an empty one, so that the length of its first recording can be chosen; a value that is not a number is refused first, and so is a node that is no lane (`a pattern belongs to a lane`). All three are told as the one event `PatternSet { node, pattern }`, the pattern the lane has after it (`PatternInfo`: whether it is recording, enabled, looping and in key, its length in beats, the tempo it was recorded at and how many notes it has; null when there is none), which is also on the lane's node in every snapshot (`Node.pattern`); and none of them is a step of the history, so that undo goes back past them and what is done to a pattern forgets nothing (Open question 11). `Save` is built, step 18: it writes the session to its own file, as the TUI's
  `Ctrl+S` does, and nobody names a file; it is answered by a revision and the event
  `Saved { file }`, which leaves the session with nothing unsaved; a session with no file, or
  one that cannot be written, is refused with the code `io`, which is no revision and changes
  nothing. `SaveAs { name }` is built, step 38 (D40): the session is written as the file that `session::plain_file_name` makes of `name`
  (the spaces round it go, `.toml` is added unless it ends so, and it has to be a plain file name, §8), beside the session's own file (the
  working directory if it has none), and that file is the session's from then on; it is answered by a revision and `Saved { file }` with the
  new name, which every client takes as it takes a save (nothing unsaved, and the name the bar shows). It is refused, with nothing written and
  nothing changed, for a name that is not a plain file name (`invalid`, in the rule's words) and for a file that is there already, a link that
  leads nowhere included, unless it is the session's own file (a save as the name the session has is a save); a write that fails is `io`. It is no
  step of the history and no barrier: what was done can still be undone, and the file is no part of it. A file that is there is refused by the write
  itself (an exclusive create, `session::save_new`), not by a look at the path before it, so a file that appears in between is not written over. `Undo` and `Redo` are built, step 32: the controller keeps a history
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
`SetLocked` and `MoveEffect` (step 32), `SetMute` and `SetSolo` (step 53), and what is added (step 33): `AddEffect`, `AddInstrument`, `AddModulator`, `AddModEdge`, `Modulate` (step 37), `AddGroup` of lanes
that are in no group, and `SetLaneGroup` of a lane that its old group does not drive. What is added is taken out by `RemoveNode` and `RemoveEdge`
and put back by redo **under the ids it had** (D36): the controller writes down the ids that an intent hands out (`Made`), the step keeps them
(`redo_made`), and `carry_out_all` says them to the allocator before the intents are carried out, so that a step from after the add (a gain set on the
effect that was added) finds the effect when it is redone; ids that are in use are refused, and ids the intents did not use are dropped. A change of one thing (the same parameter, gain, setting or cable, `Key`) a second or less after the
last of it is one step, so that a drag is one (`SAME_STEP`; a pause of more than a second makes a step of its own, and exactly a
second does not), 200 steps are kept (`MAX_STEPS`; the oldest go), a change after an undo ends what could be redone, an intent that is
refused leaves no step, and `Save` is no change. `SetNodeUi`, `SetRange`, `SetPinned`, `RenameGroup`, `SetScale`, `SetLocked`, `MoveEffect`,
`SetMute` and `SetSolo` have no key, and each is a step of its own. `Undo` and `Redo` are intents like the others: a step is applied
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
master peak and clip, and the last frames of the output mix, which a scope draws (step 42).

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
    parameter. A target that drives a modulator's own field (cross-mod) has one too (f132, step 39): what that
    setting (an LFO's rate, a stage of an envelope, the depth of a cable) was set to this buffer, which is the
    last write to it when several cables drive it (the engine's cross-mod is last-wins, `apply_cross_mod`), read
    back from the setting once all are applied (`publish_cross_mod_effective`), and for a group's cable to a
    member's modulator after the lanes have run (`publish_group_member_mod_effective`, since the member's own
    cross-mod, inside the lane, wins). The layout names it at the port the cable ends at (`rate`, `attack`, …,
    `depth`).
  - *Meters* (`MeterSlots { peak, clips }`): master, every lane
    (`InstrumentNode.meter`, measured after the lane's volume and effects and
    before it joins its group) and every group (`GroupNode.meter`, after its
    volume, bus effects and pan, before the master). The peak is held and falls
    20 dB per half second, so a reader at 30 Hz sees at most about 1.3 dB under
    the loudest sample since its last look. `clips` is a count that goes up by one
    for each buffer with a sample beyond full scale (|s| > 1.0) and is never
    reset, so any number of readers can watch it. The TUI's `CLIP` light is the
    first reader (`src/tui/clip.rs`).
  - *The output itself* (`Telemetry.scope`, a `ScopeRing`; step 42, D43): the last 8192 frames of the master mix
    (170 ms at 48 kHz), both sides, that `AudioGraph::process` stores after the master meter has looked at
    the buffer. Every sample is a 32-bit atomic cell of its own, so that a float cannot tear and the audio thread
    stores with no lock and no allocation; a count of the frames stored in all, which the writer updates after the
    frames and a reader reads before and after its copy, says whether the copy is one run of frames that were stored
    together (a copy that the writer overtook is made again, three times at most: the writer is a buffer ahead at the
    most, and the ring has room for four times what a reader may take, `SCOPE_READ_MAX`, 2048 frames). A reader
    takes the latest frames, the newest last; before the first buffer it gets silence. The page's scope and the
    terminal's Scope tab are the readers.
  - *The signal at a node* (`Telemetry.probes`, a pool of `PROBES` = 8 rings of that kind; step 46, D47): the last 8192
    frames of what comes out of an instrument (after its volume), of an effect (after its mix) and of a group (after its pan;
    a bus effect, after its mix, before the pan), stored by the audio thread's taps, one at each of those places, into the ring
    that the controller has told the audio thread the node has (`GraphCommand::SetProbe`; a lane or a group that nothing listens to has an empty list of taps, a check at each stage). The rings are made with the telemetry, so that asking for a node's signal allocates nothing on the audio
    thread, and are shared: which node each is for is the controller's (`Controller::set_probes`, `src/app/probes.rs`), and is
    in the telemetry for a reader to ask (`Telemetry::probe_node`). A reader copies the latest frames as it does the output's.
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
  **As built (step 42):** the scope is a fourth channel, `scope` (kind 3): the latest 2048 frames of the output
  (`SCOPE_FRAME_LENGTH`, 43 ms at 48 kHz) of the left side and then 2048 of the right, the newest of each last,
  `layout_rev` 0 (it follows no layout), silence before the first buffer; 2 × 2048 × 4 bytes and an 8-byte header, 30
  times a second, to a client that asked for it. Since that is a good deal more than the other channels (a frame of
  those is a value or two for each meter, modulator and cable) and only a picture on the screen uses it, a page asks
  for it only while it shows one (`Client.watchTelemetry`, counted, asked again on every connection: D43). The spectrum is not on the
  wire: a client makes it from the samples.
  **As built (step 46):** the signal at a node has frames of its own, channel number 4 (no client asks for it as a channel: a client asks for
  nodes, by `probes { nodes }`, §7): the same 2 × 2048 values as the scope's, the left side and then the right, and the node's id where a
  frame of the others has the layout's revision, since a probe follows no layout. A client is sent one for each node that it watches and that
  has a ring, 30 times a second (`web::pusher`).
- **Visual vocabulary.** Knob = base value; ring = modulation extent (depth ×
  range); dot = effective value. A setting of a modulator (an LFO's rate, a stage of an
  envelope) that a cable drives has the same since step 39 (D41), and the depth of a cable that
  another cable drives has a bar from the middle for the effective depth, under its slider.

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
    `subscribe`, `telemetry { channels }`, `probes { nodes }` (step 46: the nodes whose signal the client
    wants, replacing the list it had; see below), `request { reqId, intent }`,
    `noteOn { note, velocity }`, `noteOff { note }`.
  - server → client: `hello { protocol, rev, features, needsToken }` (first),
    `snapshot { snapshot }` (the answer to `subscribe`, and, since step 13, also sent
    unasked to the subscribed when the session changed in a way no event describes: the
    client replaces its copy, the layout included, and carries on from the revision in
    it), `event { event }` (a `StampedEvent`), `layout { layout }`, `catalog { rev,
    scanning, entries }` (step 19: the plugins that can be added; see below),
    `history { undo, redo }` (step 32: what Undo and Redo would do; see below),
    `probes { nodes }` (step 46: which of the nodes a client asked the signal of are watched now; see below),
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
  - **Probes (step 46, D47).** A client says which nodes it wants the signal of with `probes { nodes }`, the whole list each time (what it
    asked before and does not now it no longer wants; none is `[]`), and is answered with the nodes of them that are watched: those that have a signal
    (an instrument that has a plugin, an effect, a group; not a modulator, not the master, which has the scope that every client may ask for, and not a node that
    is not there) and a ring (the engine has eight, shared by every client: two clients that watch a node share its ring, and a node that no ring is
    left for is watched when one is free, without asking again, and is not in the answer until then). The server keeps each client's list
    in the hub (`Hub::set_probes`, at most `MAX_PROBES`, 64, of a longer one; the message is limited in rate like a request) and the controller keeps who
    watches what (`Controller::set_probes`, with the connection's number as the watcher), and a client that closes lets go of its nodes, by the owner's next turn
    (`refresh_probes`). The frames are the telemetry channel of §6. The answer is no event and no revision.
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
  front-ends. (Built, step 38, D40: `session::plain_file_name`, which the intent
  `SaveAs` asks and the TUI's popup asks through it. A name is trimmed, may have no `/` or
  `\` (so no `..` as a folder and nothing absolute), none of `:*?"<>|` and no control
  character, has to have something before the extension, is not a name that Windows keeps
  for itself (`con`, `prn`, `aux`, `nul`, `com1` to `com9`, `lpt1` to `lpt9`, in any case,
  with any extension) and is at most 255 bytes; a file that is there is not written
  over.)
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
tree, names, the parameters of their plugins, cables, and where the session has placed a node) and not for a value, and it does not rearrange the nodes: `toFlow` is given where it put them last
and leaves a node where it was while its slot is the same (step 44, D45); the layout from nothing is what a page that is opened has, and what the button "Arrange" asks for.
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
the layout puts them; `sameStructure` compares the places, so that a move from another page lays the canvas out again. A button
in the corner of the canvas, "Arrange" ("Arrange the nodes again" to a screen reader; a `ControlButton` among the zoom's, a small arrow, until step 44),
lit while a node has a place or one is not where the layout has it, gives every node that has a place back to the layout, lays the others out from nothing and fits the view to them. Not built: moving a node with the keyboard (React Flow's own needs a node to
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
tall as it is long. (This is the layout from nothing: the page draws it when it is opened and when the nodes are arranged,
and a cable that is drawn leaves the nodes where they are, from step 44: D45.)

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

**The scope of a node (step 46, D47).** The header of an instrument that has a plugin, of an effect (a lane's and a bus's) and of a group has a button with a sine wave
on it (`ScopeButton`, "Scope of X", pressed while the scope is shown) that shows a scope of what comes out of the node, in the node, below its controls: the wave or
the spectrum of the master's (`ScopePanel`, which `MasterNode` is drawn from as well), with the choices of its view and its time base, which are the scope's own and are
lost with it. A lane that has no instrument has no signal and no button, and neither has a modulator; the master has its scope always. Which nodes show one is the
viewer's, as which node's parameters are in the panel is: `PatchView` holds the set, `scopes.ts` is what the nodes ask it for (the same shape as `inspector.ts`), and
nothing of it is saved, sent or put in the history. The page asks the server for the signal of a node while its scope is in it (`NodeScope`: `Client.watchProbe`, which the
effect lets go of with the scope), reads it as frames of its own (`telemetry.probe(node)`), and draws it as the master's is drawn. The layout is told which nodes show
one: `toFlow(graph, remembered, scopes)` makes the nodes that do (`canScope`: an instrument with a plugin, an effect, a group) `NODE_SCOPE_HEIGHT` taller, the 96 pixels of the
scope, the gap, the choices and the foot (157, as a browser draws them: a test on the page's mock engine keeps the constant at the measure), and makes room for it as for
a knob that is pinned, so that the rows below move down by what is needed, strips and all, and nothing lies over anything (a test goes through every set of nodes of a session
with a group that show one). What was moved stays when the scope is taken away, until the nodes are arranged; "Arrange" lays them out with the scopes that are shown, and a
node that is taken out and put back by undo shows its scope again, since the set is of ids (a scope that is asked for a node that has no signal is not shown). A scope that is opened where it cannot be seen moves the canvas to the node, as a node that the page added does (`Reveals`:
`PatchView` notes the node when a scope is opened, and asks to show it once the layout has the scope, in an effect of the layout that runs after the canvas's own, which keeps the layout it moves by; the
canvas moves if the node with its scope is not all in the view, by the least that puts it in the middle at the zoom the person has, and not at all when it is, nor when a scope is taken away, nor again
when the layout changes later; `rectOf` is as tall as the layout makes the node, with the scope it shows and the rows of the cables a modulator lists). The
badge of a node, what kind of plugin it is, is under its name in the header, in small type, since f148: beside the name it left 37 of the 75 pixels that "Oscillator 1" needs in a lane that has a number
in its name and a "×" (the scene of two lanes, the split example) and 30 of the 92 that "Pianoteq 9" needs; the title has what the buttons leave and the header is as tall as it was.

**As built (step 51).** A lane has the pattern block under its controls (`PatternCell`, `app/patch/PatternCell.tsx`; a block of its own and not a cell of the grid, since it takes the node's whole width), an empty lane too: it
asks the intents of step 50 and shows what the session says of the lane's pattern (`patternOf`). Its first row is the button that records (`● Record`, red; while a recording goes on `■ Stop`, red all through and pulsing,
which `prefers-reduced-motion` stops; `Editor.setRecording`, with the count-in and the length and the tempo in its hint), the menu that clears (`Clear`, `MenuButton`: it opens on "Keep the pattern" and the other item says "Clear the pattern of
Oscillator for good: 2 beats, 3 notes", since undo does not put a pattern back; `Editor.clearPattern`, and the focus goes back to the record button, for the clear button is dimmed when there is none) and the length in beats (the page's
number field, `ui/NumberField`: Enter or leaving it asks, Escape gives up, the arrow keys turn it by one and by four with Shift, 1 to 32 from `PATTERN_FIELD_RANGES`; a lane that has no pattern shows `PATTERN_DEFAULT_LENGTH`, and a length
that is set gives it an empty pattern: `Editor.setPatternField`). The second row has the switches, each a toggle button (`aria-pressed`) that asks its setting the other way round from what it is: Play (a held key plays the pattern),
Loop (it goes round while the key is held) and In key (it is moved by the degrees of the piano's scale and not by semitones), dimmed while the lane has no pattern. The third row is a line that says what the pattern is
(`client/patternWords.ts`): "No pattern yet"; "2 beats · 3 notes · 90 BPM" (its own tempo, the one it was recorded at); "Counting in, then 4 beats at 120 BPM: play now" while a recording goes on (the session's tempo, which the
recording is made at; the count-in is told in the hint of the record button, since the line is as short as the node is wide). The block is as tall whatever it shows, which the layout counts on: its rows have heights of their
own (the button's glyph `■` has a taller fallback font than `●`, which made the row 2 px taller in the first measurement), `nodeHeight` adds `PATTERN_HEIGHT` (97, measured in a browser in both states of the button) to every lane
and to nothing else, and the browser tests hold the block to it. The mock engine does what the engine does with a recording, so that the page can be tried without a server (`?engine=mock`): `MockServer.settle`, see step 51 in §12.

**As built (step 53).** A lane and a group have a mute and a solo (the intents `SetMute` and `SetSolo`, D53), in a cell that is last in the grid of the node's controls, called "Mute / Solo" (`MuteSoloCell`, `app/patch/controls.tsx`:
after the keys and the group of a lane, after the volume and the pan of a group; an effect, a modulator and the master have none). It is two small buttons side by side, `M` and `S`, each a toggle (`aria-pressed`), named for the node on the canvas
(`Mute Oscillator 2`, `Solo Pad`) with a title that says what a press does (`Mute …` or `Unmute …`, `Solo …` or `Unsolo …`); a pressed mute is red and a pressed solo yellow (`--tang-danger`, `--tang-warn`). A press asks for the other way round
from what the session has (`Editor.setMute`, `setSolo`) and the buttons show what the session has: nothing is changed before the answer is in, and a second press before it asks for the same
as the first. A node that cannot be heard at the master is drawn dimmed (`tang-node--unheard`, `opacity: 0.55`, and not brighter under the pointer, so that a press of its mute is seen at once; it is as ready as it was): a lane or a group
that is muted or not soloed while something is, and the effects in the chain of one (`audibleAt`, `client/selectors.ts`). A lane in a muted group is dimmed as well, since nothing of it reaches the master, though its own buttons say what is its own.
A modulator is never dimmed: it goes on. What can be heard is `audibleOf` (`client/audible.ts`), the law of the engine's `AudioGraph::process` written again (a mute wins over a solo; a solo carries the group of a lane and the lanes of a group),
worked out once for a graph (a `WeakMap`: every node of the canvas asks at every change). It is pinned to the engine by `fixtures/audible.json`, which a Rust test writes from a run of the audio graph over every combination of the flags of two
groups and four lanes (4,096 cases, `the_web_clients_fixture_of_what_is_heard_is_current`) and a test of the page reads. A lane has four fixed controls now (its volume, its keys, its group and this) and a group three, which `controlCount`
says and the layout counts on (`nodeHeight`; a lane's first eight parameters and its four are three rows of four). The mock engine does what the engine does with its meters (`MockServer.frame`): a lane that is not heard puts out no level, a group is heard
for a lane in it that is, the master's meter is the sound of the lanes that reach it and its scope is silent when none does; it has no fade.

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
clamped, and the dot is the telemetry's effective value. Where a lane's own modulators and its
group's both drive a parameter the ring is that of the lane's cables alone, since the engine writes
the lane's over the group's (D42). For a setting of a modulator the ring
is `settingExtent`: the extent of the last cable alone, since the engine writes a setting once
for each cable and the last stands (D41).

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
- **Velocity (step 52).** A pointer plays a key as hard as where it is on it: a velocity from 20 at the key's top edge, the far end from the player, to 127 at its front edge, evenly between (`velocityAt` in `client/pointerPiano.ts`; 1 is a note nobody hears), told when the pointer goes down and when it moves on to another key. A key that has no
  height to tell a place on is played at 100, as the keys of the computer are (they report no velocity). The keys' row says it in a tooltip. It is what a velocity control (D52) follows from the page.
- *Not built:* a picker that filters as the TUI's does (two plain choices do), the notes of the scale named on the keys, the chord readout and
  the scale strip of the TUI's tab, the labels of the computer's keys on the piano.

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
   depth of a cable in step 36, and a modulator made for a knob from the knob, the TUI's `m` and `l` (a control that is armed to learn), in step 37; still to
   do: an audio cable that puts a lane in a group, patterns, and a MIDI control taught a controller in a browser test: the browser tests have no MIDI port. The program's side is tested with a virtual one
   since step 38: a controller that is moved binds the control, the parameter follows the knob, a second browser and the saved file have it
   (`tests/serve.rs`, `#[ignore]`d, since it needs CoreMIDI or ALSA's sequencer; f124).)
4. **Extras:** mixer view, session browser (needs runtime session loading),
   Web MIDI, touch layout. (The scope, on the master node and in the terminal, was done in step 42, ahead of these.)
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
    directory, `.toml` appended once, trimmed, a failed save changes nothing; it
    became the intent `SaveAs` in step 38, with a rule for the name),
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
    (one that drives a modulator's own field) had none here (step 9); it has one since step 39 (f132).
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
    even when there is nothing to save; Shift+Control+S was left alone (Save As was the TUI's, and
    the page's when there was a validated name for it, §8: step 38 made it the page's too).
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
  - **Not done.** Save As (a validated file name for both front-ends, §8: done in step 38); the page does not warn
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
    kept the effect all the same, which the loader now refuses too (f68, fixed: below). Why a plugin did not load
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
  - **f68 (fixed).** A session file that named an effect with other outputs than the chain has loaded, and
    its model kept the effect that the audio thread dropped (the loader sent the command and pushed the
    slot without looking), and the parameters of the effects after it were told to places the engine has not,
    or to another effect (`SetParameter` addresses an effect by its place in the chain, and the chain was
    shorter by one). The loader refuses such a session now, in the words of the place in the file and of the
    plugin (`instrument[0].effect[1]: MonoEffect has 1 audio output, and an effect here needs 2`, and
    `group[0].effect[0]: …` for a bus), by the one rule that the selector and undo have (`outputs_fit_the_chain`,
    which was written twice, with two wordings, before: the refactoring is its own commit). A built-in that
    exists in the tests only, `builtin:mono-effect`, is the effect with one output that the loader's tests load, and
    with it two places in undo that nothing reached (a lane's or a group's plugins loaded again and looked at) have tests.
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
    the layout did not make room around it, so a node added later could land on one and hide it (f80: it is moved clear of it
    now, §4.4, after step 38).
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
    fixture's scripts draw from the last modulator of the rack. TODO: the mock could keep the layout in the rack's order. (Done in step 39, which closed f114: `withEntry`.)
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
    cable drawn later in another order (f114; the mock lists it where the controller does since step 39), which no reader depends on. Only Chromium was tried, and what the audio thread did was checked through the master's meter on the null backend and by the tests' own graphs, never on a sound card.
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
  telemetry; it has one, and the slider a bar for it, since step 39), a cycle of cables that drive each other's depths cannot be made from the page (a file can, and its removal is not put back: D37), and the slider of every other cable is still at the middle of it.
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
  - **Limits, as built.** A cable to a depth has no slot of telemetry (as no cable to a modulator's own field has; they have one since step 39), so the slider of a cable whose depth is driven shows what was set and not what the cable that drives it makes of it;
    the label of a row is cut when it is long (its title has all of it); the slider of a cable whose port is not below what drives it (a node that was moved) is where the library puts it; a modulator with many cables is as tall as its
    rows; a group's modulator that drives the depth of a member's modulator's cable is checked by the unit tests and by the fixture of the controller (`fields.json`), not in a browser; only Chromium was tried.
  - **Tests.** Web: 1571 in 75 files (1533 in 74); 84 in browsers (75). Rust is as it was at the end of step 35: nothing in it changed. The mutations of each part are in its entry above.
  - **Numbers at the end of the step.** Rust 1285 tests, 1443 with the `web` feature and 22 on the real program, 1280 without the default plugin formats and 1436 with `web`, and 3 of the shell's in each; web 1571 tests in 75 files;
    84 in browsers; 329 commits on the branch before this entry.

- [x] **37. Modulate a knob from the knob (phase 3).** The TUI's `m` (a new LFO or envelope for a parameter) and `l` (a MIDI control that learns the next controller that is moved) made a modulator and the cable to it with two or
  three intents and joined their steps into one (`as_one_step`, over the controller's `history_mark` and `join_steps_since`); a browser had to add a modulator from a lane's menu and then drag a cable to the knob, which was two
  steps, and had no gesture for a MIDI learn at all. *Done when:* every knob of a node, every row of the ⚙ panel and every row of a modulator's cables has a button with a menu of a new LFO, a new envelope and a new MIDI
  control, and of the modulators that are there and could drive it; a new one is made for the control with its cable as one change, which is one step of the history ("Undo modulate cutoff") that one Undo takes out and one
  Redo makes again; a MIDI control is armed to learn, with its cable at the whole range; on the real program the session and the saved file have it, and a second page sees it. In parts, each its own commit: **37.1** the
  intent `Modulate` in the controller, the mock engine and the TUI, **37.2** the page. *Not done (later steps):* a menu on a control that is chosen from a list (the TUI's `m` leaves such a row out, and so does the page), a
  modulator of a group made from a knob of a member lane (it goes in the lane's rack, as the TUI's `m` has it), a list of what is there that follows the session while the menu is open, and a controller moved in a browser
  test (the program has no MIDI port there; after step 38 the program's side has a test with a virtual one, `tests/serve.rs`).
  - **37.1 as built (4396f5b).** `Intent::Modulate { to, kind, depth }` (D39): a modulator of `kind` at the end of the rack that reaches the port `to`, and a cable from it to the port, at half depth or at `depth` (clamped as
    `SetEdgeDepth` clamps it); a MIDI control is armed. Everything that can be refused is looked at before anything is made, with the modulator at the place it will have (`param_cable` and `field_cable`, which `AddModEdge` plans its
    cable with too: one rule of reach and one set of words), so a refusal makes nothing, uses no id and leaves no step; a node that is a group is refused in its own words, since it has no parameter. It is a `Plan::Added`
    whose step takes out the modulator (`takes_out`: one node and one cable were made, and the cable goes with the node), in the words of the TUI (`modulate cutoff`, `learn cutoff`, `modulate rate`, `modulate depth`), and redo
    makes both again under the ids they had. The TUI's `m` and `l` ask it (`Driver::New`); a modulator that is there still gets `AddModEdge`. With it the joining of steps has no user, and went: `HistoryMark`,
    `History::mark` and `join_above`, `Controller::history_mark` and `join_steps_since`, and the eight tests of them; the TUI's `TargetEntry::label`, which named the step, went too, and the golden tests of the selectors
    no longer pin it. The mock engine does the intent as the controller does (`modulateOutcome`: the modulator is made first, since what it reaches is read from the graph, and the ids and slots are given back if the cable is
    refused: `Counters.mark` and `rewind`), with the words of the history and `takesOut`, and the controller's history fixture has five scripts of it (70 scripts, 672 rows) that pin the mock to it, done and refused. Found on the way:
    the fixture's text of a cable's depth was written by Rust (`1.0`) and read by a script (`1`), which no script had shown until the first whole depth; it is written as a script writes it. 7 tests of the controller and 3 of its
    history, in place of the 8 of the joining; 22 of the page's client; 22 mutations of the Rust code, all caught, and 32 of the page's, all caught.
  - **37.2 as built (e82f261).** `Editor.modulate(node, port, kind)` asks the intent (a control that learns at depth 1, as the TUI's `l`; the others at the controller's half) and resolves with the node it made, which the canvas
    is asked to show; what the page says of a refusal names the control and the kind (`Could not modulate Filter: cutoff with an LFO: …`). `driversOf` (`client/reach.ts`) is what could still drive a port, `drivablesOf` read from
    the other side (tested against it over two fixtures); `MenuButton` has an `onOpen`, so that the menu looks it up when it is opened and not when anything changes. `ModulateMenu` is the button and its menu, "New" and "Attach"; it is
    on the knob of a parameter (`ParamCell`) and of a modulator's number (`ModulatorSettingCell`), on the rows of the panel beside the pin (`tang-row__tools`) and on the rows of a modulator's cables (it modulates the depth of
    that cable); a control that is chosen from a list has none. In CSS it is quiet until the pointer or the focus is on the control, over the corner of its knob and in front of the control beside it, and it gives way while a
    cable is dragged. Found on the way: the first version wrapped the control of a cell to put the button at its corner, and the whole browser suite found that the wrapper (positioned, after the port in the page) covered the port that
    grows to the cell while a cable is dragged, so a cable let go over a control was not drawn to it; and a screenshot showed the button's glyph cut by the control beside it. The wrapper is gone, and what is drawn at the button's centre and
    where the button is are checked. 32 Vitest tests, 13 in browsers (10 on the mock engine, 3 on the real program: the session, the file, Undo and Redo, a second page, a MIDI control that is armed and kept unbound); 37 mutations
    of the code, 36 caught (two after the tests were made to say which parameter and which setting a menu is for) and one that is the same code written otherwise (the list is looked up when the page is drawn as well as when the menu opens),
    and 7 of the CSS against the browser spec, 6 caught and one that makes no difference (the button of a row is under the port that grows to its row, so it need not give way).
  - **Limits, as built.** What is there is looked up when the menu is opened: a modulator that another front-end adds while it is open is not in it. A control that is chosen from a list has no button (a cable can still be dragged to its
    port). The new modulator for a member lane's plugin is the lane's, as the TUI's; the menu lists the group's modulators among those that could drive it, since they reach it. A screen with no pointer shows the button at 60% (`hover: none`):
    not tried, and only Chromium on a desktop was. A MIDI control made here waits for a controller: the browser tests have no MIDI port, so they check that it is armed and that the file has it unbound
    (after step 38 an `#[ignore]`d test of `tests/serve.rs` teaches one a controller through a virtual MIDI port: it is bound, the parameter follows the knob,
    a second browser and the saved file have it). The slider of a cable that ends at a
    depth still shows the depth that was set (f118; step 39 added a bar under it for what the engine makes of it).
  - **Tests.** Rust: 2 more than at the end of step 36 in each configuration (1287 against 1285 in the default one; 1445, 1282 and 1438 in the others); the real program's 22 and the shell's 3 as before. Web: 1625 in 77 files (1571 in 75);
    97 in browsers (84). The fixture of the history has 70 scripts (65). The mutations of each part are in its entry above.
  - **Numbers at the end of the step.** Rust 1287 tests, 1445 with the `web` feature and 22 on the real program, 1282 without the default plugin formats and 1438 with `web`, and 3 of the shell's in each; web 1625 tests in 77 files;
    97 in browsers; 332 commits on the branch before this entry.

- [x] **38. Save as from the page (phase 1).** The page had Save and the TUI had Save As, so a copy of a sound was made by stopping the program and copying the file; and the TUI's `Controller::save_as` put the name
  after the session's directory with `dir.join(name)`, which an absolute name replaces, took `a/b` for a folder and wrote over a file that was there. §8 had said from the start that Save As takes a plain file name
  only, from one validated function for both front-ends. *Done when:* the page has a "Save as…" that asks the server for a name; the server says what a name may be and refuses what is not one (a folder, an absolute
  name, a character that a file cannot have, a name that Windows keeps, more than 255 bytes) and a file that is there; the session is the new file from then on, in every page and in the terminal; and on the real
  program the copy is written beside the file the session had, which is not written, and a save after it writes the new file. In parts, each its own commit: **38.1** the intent, the rule and the TUI's popup on it,
  with the mock engine and `Editor.saveAs`; **38.2** the page's form, and the bar's layout. *Not done (later steps):* a file in another directory (a name is a name: the directory is the session's), a way to write
  over a file that is there from the page (it is refused: there is no undo for a file), and a session browser (§11, extras).
  - **38.1 as built (4cae8aa, and 9d61ece for the words of a notice).** `session::plain_file_name` is the rule (§8, D40): the spaces round the name go (Rust's `trim`), no `/` or `\`, none of `:*?"<>|` and no control
    character, `.toml` is added unless the name ends so, something has to be before the extension, the part before the first dot is not a name that Windows keeps (`con`, `prn`, `aux`, `nul`, `com1` to `com9`, `lpt1` to
    `lpt9`, in any case), and the file is at most 255 bytes; each of the five refusals has its words. `Intent::SaveAs { name }` is `Controller::save_as_intent`: the path is beside the session's file (`save_as_path`;
    the working directory if it has none); a file that is there is refused unless it is the session's own path, by the write itself (`save_to` with `FileMode::CreateNew`, which is `session::save_new`, an exclusive create: a link that leads nowhere is a path that is taken too, and is not followed; it was a look at the path first, `symlink_metadata`, until f128), and the name is
    looked at before anything is written; the write is `save_to`, which `Save` uses with `FileMode::Replace`; only then does the session have the new file, the revision go up and `Saved { file }` reach the subscribers (a refusal tells nobody and
    changes nothing, the unsaved mark included). It is no step of the history (`Plan::Nothing`, which `apply` never reaches for it and a test asks directly). The TUI's `Ctrl+Shift+S` asks the same intent, and
    `Controller::save_as` is gone: what the popup does changed (a name is checked, a file that is there is no longer written over, the browsers are told), and the five tests of the old method went with it. The client has
    the rule written out for the mock engine (`client/filenames.ts`, which the page itself does not use: the server decides), pinned to the controller's by `fixtures/filenames.json`, 67 names that a Rust test writes (25 that
    make a file, 42 that are refused, in the five sets of words): the ends of the control ranges that JavaScript has no word for (U+001F, U+007F to U+009F and the characters next to them), the white space that Rust's
    `trim` takes off and JavaScript's does not (U+0085) and the other way round (U+FEFF), names of exactly 255 bytes and one more, counted in bytes and not characters. The mock engine's `saveAsOutcome` knows the files of
    its directory (`MockOptions.files`, and the session's own, and the ones a save as writes), and `Editor.saveAs(name)` asks for the name as it was typed and says it, if refused, in quotes, trimmed and cut after 24
    characters (`quoted`, which `renameGroup` uses too: `Could not save the session as "other": a file called other.toml is there already`).
  - **38.2 as built (6007806, 2624781).** `app/SaveAs.tsx`, after Save in the bar (`SessionBar`): a "Save as…" button that is there whether or not anything is unsaved, which opens a field in its place with the name of
    the file the session has (read when it is opened), selected and with the focus, and OK and Cancel; Enter or OK asks `Editor.saveAs`, one at a time and not for a blank name, and an Enter that confirms what an input
    method is composing is not for the field; when it is done the field goes and the button has the focus (a layout effect: with a passive one the page showed the field gone a moment before the focus moved, and a test of the
    focus failed in 5 of 20 runs); when it is refused the field stays with the name selected and the focus in it; Escape and Cancel leave and ask nothing. `Control+Shift+S` and `Command+Shift+S` open it (or find it open
    and select it), keep the browser from offering to save the page, and are not heard with Alt or for another letter (Control+Shift+Z is Redo's); Control+S is still the save. The bar takes a second row in a window that is
    too narrow for all of it (`flex-wrap`): with the field open the bar was too wide below about 1190 pixels (the hint wrapped into three lines at 1100 and the page scrolled 81 pixels sideways at 1000), and the closed
    bar, which is older, wrapped its hint at 900 and scrolled at 800; it is 41 pixels from 1280 up, field open or not, and 63 or more below, with no sideways scrolling from 700 to 1600. Found on the way: the focus above (by the baseline
    check that the mutation script has now, after one run said "all caught" over a test that was red: it expected four notices where the board keeps three), and the layout (by a screenshot).
  - **Limits, as built.** A file that is there is never written over from either front-end, and the write is what refuses it (an exclusive create), so a program that makes the file at any time before the write is caught as well; the session's own file is the one that is written over, by a save as its own name, which is a save. The directory
    is the session's: a name cannot put the file elsewhere (a person who wants that moves the file). The page cannot list the directory, so it finds out that a name is taken by asking. The rule is the same on every system:
    `con.toml` cannot be made on Linux either, so that a session can be moved to Windows. In a window narrower than about 1190 pixels the bar takes a second row while the field is open, and the canvas below it is that much
    shorter. Only Chromium was tried; another browser may keep Control+Shift+S for itself (as the shortcut for a screenshot, say), and the button is the way in then. The terminal's keys are as they were.
  - **Tests.** Rust: 12 more in each configuration (17 added, and the 5 of the old method gone: 1299 against 1287 in the default one; 1457, 1294 and 1450 in the others); the real program's 22 and the shell's 3 as before. Web:
    1658 in 80 files (1625 in 77); 106 in browsers (97 in 22 files): 3 on the real program (the copy beside the file, the name in the bar, a save that writes there and not into the first, a second page told, and another
    page's save as told to the first; the refusals, with nothing written but what was asked for; the keys, and a letter typed in the field that is no note) and 6 on the bar's layout. Mutants: 31 of the Rust code, all caught
    (one survivor was a check that the next made redundant, which is gone, and one an arm that `apply` never reaches, which a test asks directly); 54 of the client's rule, mock and editor, 5 of which survived the first
    run (the fixture lacked the ends of the control ranges and a name ending in `toml` with no dot, and nothing showed that the mock remembers a file a save as wrote), all caught after; 7 of the notice's words (one
    survived: no test had a name of exactly 24 characters, the group's rename too); 46 of the form, 4 of which survived (the chord tried with one other letter, a refusal asked through OK, Enter taken, and absences
    looked at before the request could have reached the server) and were caught after; 6 of the page in the browser, 5 caught and one that makes no difference in Chromium (`select()` focuses an input there, which was
    checked); 3 of the CSS, one caught and two `nowrap` rules that made no difference and are not there.
  - **Numbers at the end of the step.** Rust 1299 tests, 1457 with the `web` feature and 22 on the real program, 1294 without the default plugin formats and 1450 with `web`, and 3 of the shell's in each; web 1658 tests in 80 files;
    106 in browsers; 338 commits on the branch before this entry.

- [x] **39. A cable to a modulator's own setting is seen on the setting (phase 1).** An envelope drawn to the rate of an LFO moves the rate, and the knob of the rate showed nothing of it, where a parameter's knob has a ring (how far the
  cables that end at it can take it) and a dot (where the engine has it); the user reported it ("if I connect an ADSR cable to for example Rate on LFO, I dont see the same visuals on that Rate-knob", f132). The slider of a cable whose
  depth another cable drives showed the depth that was set, for the same reason (f118, from step 36). The cause was one fact: a cable to a modulator's own field had no slot of telemetry (`ModTargetKind::has_effective_value` was false for it),
  so the engine published nothing for it, the layout named nothing and the mock engine had nothing to draw. *Done when:* the engine publishes what a setting was set to, into the slot of each cable that drives it; every cable has a slot, and
  the layout names the value at the port the cable ends at; the mock engine does as the controller does; the knob of a rate and of the stages of an envelope has the ring and the dot, and the slider of a driven depth a bar; and on the real
  program the dot of the Rate knob follows the real envelope while a note is held. In parts, each its own commit: **39.1** the engine, **39.2** the model and the mock engine, **39.3** the page. *Not done (later steps):* a ring for the depth
  slider (the bar says where the depth is, not how far the cables that drive it can take it), and the mock engine's slot for a value that two cables drive when the first is taken out (f109, which now holds for a setting too).
  - **39.1 as built (c5ce0f5).** `apply_cross_mod` takes the telemetry and, once every write is applied, `publish_cross_mod_effective` stores the value each driven setting has into the slot of each cable that drives it: the last write
    when several cables drive one setting (the writes are last-wins, as `apply_cross_mod` has always had them), nothing for a cable to a setting its modulator has not (an envelope has no rate) or to its own. The cables of a group's modulators to
    the modulators of its members are published after the lanes have run (`publish_group_member_mod_effective`), since a member's own cross-mod is applied inside the lane, after the group's, and wins: what is published is what the setting
    ends up as. `sibling_field` is the one mapping from a cable to the setting it drives (the application had it inline) and `cross_mod_field_value` reads a setting. Dormant until the model gave these cables a slot (39.2). 8 tests added
    and the one that said a cross-mod target publishes nothing replaced; the audio thread still does not allocate with cables of each kind; 14 mutations, all caught (the first run had a survivor, a test whose slot held 0.0 already).
  - **39.2 as built (5be3a9c).** `has_effective_value` is gone, with no other reader: `Controller::add_target` and the loader's two sites (a lane's modulators and a group's) allocate a slot for every target, and `check_ids` claims it for
    each (a cable without one, or sharing one, is noticed). The layout names the value at the port the cable ends at (`rate`, `attack`, ..., or the `depth` of the cable that is driven), which `aim` already mapped: its code needed no change,
    its test names three more entries. The fixtures that hold the layout (`groups.json`, `history.json`) are regenerated: entries added and slots shifted. The mock engine does it as the controller does: a cable to a setting takes a slot
    and its value an entry unless another cable drives it already, where the controller lists it (the lanes' targets before the groups', by the place of the modulator: `withEntry`, which the cables to parameters use too, with one tail,
    `withDrawnCable`, for both); the value is what the engine's last write is (`#settingEffective`: each cable from the value the user set, an envelope rising to it, anything else offsetting it, clamped to the range, the last one standing);
    and `paramKey` became `aimKey`, which keys every port, so that taking out the last cable that drives a setting takes its entry out. Rust: 1 test added and 6 changed to say that a cable to a setting has a slot. Vitest: 10 added.
    Mutations: 17 of the mock's code and 5 of the controller's, the loader's and `check_ids`, all caught after a first run of the mock's that had 5 survivors (tests for an LFO as the source, for each stage and for the depths of two
    cables took them down to 3: two were dead code, an `aimed ?` that the next line made redundant and the place of a cable among its modulator's, which a cable that is drawn last and a modulator that cannot drive a value twice make
    irrelevant, and are gone; the third was a missing test for the order of lanes and groups).
  - **39.3 as built (9d74698).** `modulationsAt(graph, node, port)` is `modulationsOn` for any place; `settingExtent` is the extent of the last cable alone (D41). `ModulatorFieldControl` gives its knob the ring (`modulation`) and the dot
    (`readEffective`, the slot named at the setting's own port), as `ParamControl` does, and does not render again for the telemetry. `CableDepthControl` finds the modulator its cable starts at (`cableSourceOf`: the layout names the value of
    a depth there) and, when another cable ends at the depth, puts a centred `LiveBar` (-100 % to 100 %, "Depth of X, as modulated") under its slider, in a column (`.tang-depth__body`; the pill is 6 px taller then). Vitest: 23 added. Browser:
    2 on the real program (`e2e/settingmod.spec.ts`): an envelope drawn to the LFO's rate gives the knob a ring, a dot at 0.255 Hz at rest, at 0.4265 Hz with a note held (the sustain, 0.7) and back on the release, while the knob stays at
    0.5 Hz; the same for the depth of the LFO's cable (-0.3, 0.19, -0.3, the slider at 40 % all the while). 17 mutations of the helpers, the controls and the selector, all caught at the first run; 3 of the page on the real program (no
    dot, no ring, a depth that is never driven), all caught. Looked at in Chromium on the real program, at rest and with a note held (the ring a short arc over the track, the dot at the end of it and then near the pointer; the bar a small
    block on either side of the middle).
  - **Found on the way.** f114 (the mock lists a cable drawn from an earlier modulator after the others, where the controller lists it by its modulator's place in the rack) is closed: `withEntry` is the rack's order, and the history fixture
    pins it (its replay alone catches the three mutations of that order: the entry put after, the place of the modulator ignored, groups before lanes); the note in the
    fixture's script that said the mock lists it after the others is gone. The mock's `without` knew only the entries of parameters, which could not matter until a setting had one. And a look at the bar on the real engine, after the step, found a bug that had been there since cables to depths (step 36)
    and in a file's `mod_depth`: **f133**, fixed in 36aebac. The engine moves what the cables that drive a rate or a stage work from when the user sets that rate or stage (`update_cross_mod_base`), and for a depth it had an arm and no caller,
    so that the user's setting of the depth of a driven cable was written back at every buffer to the old value and the cable's offset: the slider did nothing to what was heard, and the bar said the old depth. A test of a lane's and a group's,
    and one on the real program, which fails on the engine without the fix and passes with it.
  - **Limits, as built.** The ring of a setting is that of the last cable by the order of the graph's edges, which is the engine's order (a group's cables, then a lane's, each modulator's in its rack's order): a client that was told the cables
    in another order would draw another ring. A MIDI control that is not bound, last of the cables, leaves no ring (it writes the set value over the others, D41). The depth slider has a bar and no ring (a TODO in CLAUDE.md), and the bar's scale is
    the whole width of the slider while the thumb's travel is inset by half a thumb at each end: a depth near either end puts the bar's end a few pixels past the thumb's centre, as the panel's rows have it. The telemetry is 30 values
    a second: a setting driven by a modulator faster than 15 Hz aliases in the dot, as the trace of an LFO does. The mock engine keeps the slot of the first cable when two drive one value and the first goes (f109). Only Chromium was tried.
  - **Tests.** Rust: the engine's 8 and the model's 1 more in each configuration, and 7 changed. Web: 1698 in 80 files (1665 at the end of 39.1); 110 in browsers (108). The mutations of each part are in its entry above.
  - **Numbers at the end of the step.** Rust 1336 tests, 1495 with the `web` feature and 22 on the real program, 1331 without the default plugin formats and 1482 with `web`, and 3 of the shell's in each; web 1698 tests in 80 files;
    110 in browsers; 359 commits on the branch before this entry.

- [x] **40. The page says when the server is slow (phase 1).** A plugin that is slow to load is loaded by the program on the thread that answers, so every request waits behind it (§3.1: the known
  cost of the owner doing blocking work), and the page showed nothing: the menu closed, nothing happened, and a second click asked again. (None of the plugins on this machine is slow: `tang enumerate plugins` says 315 ms for Pianoteq 9,
  348 for ROLI Airwave Player, 15 for ROLI Studio Drums and 0 for the CLAP ones, which is under the light's 400 ms. CLAUDE.md's text on the splash says that a heavyweight instrument can take seconds, and this is for a load that does.) *Done when:* a request that has waited longer than a set time for its answer is counted by the
  client, a light says so over the canvas for as long as there is one, and it goes when the answer comes, or the request is refused, or the connection is lost; and on the real program, with the words of the server held back, the light
  comes and goes and moves nothing. *Not done (later steps):* what the server is doing (the plugin it loads) is not said; the terminal says it for a plugin chosen in its selector, with a box drawn before the load (a3c454d), and not for a load that a browser asks for or an undo that loads one again (a TODO in CLAUDE.md); and the loads are still
  on the owner's thread (§3.1 and open question 3: a worker, with completions that `poll()` picks up).
  - **As built (936d4bc).** `Client.slow` is a store of its own (`{ count }`), not part of `ClientState`: the state is told to every control at every change, and a request goes at every frame of a drag, where this changes only for the few
    that are slow. Each request has a timer (`ClientOptions.slowAfter`, 400 ms: `SLOW_AFTER`) that says it is slow; a request that is answered, refused, lost with the connection or stopped is not counted any more (`#forget`, in `#settle` and
    `#rejectPending`, one place for both). `useSlowRequests` reads the count and `BusyLight` (`app/BusyLight.tsx`) draws it: a pill, "Working…" with a spinner, over the top of the canvas in the middle (`position: absolute` in the main area, out of the
    flow, `pointer-events: none`), for a person who has asked for less motion a ring that stands still. It is a `progressbar` with no value and not a `status`: the state of the connection is the page's one, which the tests find it by
    (`getByRole("status")`). One light for any number of slow requests. Vitest: 12 added (8 of the count, 4 of the light). Browser: 1 on the real program (`e2e/busy.spec.ts`), with the words of the server held back in the browser
    (`page.routeWebSocket`: what a slow answer looks like from the page): a request that is answered at once never shows the light, one that is not shows it, a node is where it was, and the answer takes it away and what was asked is
    there. 17 mutations (9 of the client, 4 of the light, 1 of the hook, 3 of its CSS and of its place in the page), all caught; the first run left one, a time to wait that the tests could not tell from the default (each waited longer
    than 400 ms), which a test within a quarter of a second and a test of the default settle. Looked at in Chromium on the real program.
  - **Found on the way.** The first version of this entry, of the comments and of the messages of 936d4bc and a0fd705 said that a plugin "takes seconds to load (Pianoteq 9, a ROLI plugin)": that was said from memory of the splash
    text, and it is not what this machine does (the numbers above, from `tang enumerate plugins`). Looking at it, the terminal's part was done (a3c454d), which costs nothing for a plugin that loads at once and says what is going on for
    one that does not. The comments and this entry say "slow" now; the two messages are as they were (they are local, but this document, the log and the memory name their hashes, which are worth more than a word).
  - **Limits, as built.** The light says that the server is slow, not what it is doing. A drag of a knob goes on sending a request a frame while the server is behind, and the light is there for as long as any has waited 400 ms, which is true.
    A save to a slow disk or an undo that loads a plugin again shows it too, as it should. A node that is in the middle of the top of the canvas is under the light, which takes no pointer. The threshold is one number for every kind of
    request. Only Chromium was tried.
  - **Tests.** Web: 1710 in 81 files (1698 in 80); 111 in browsers (110); Rust as at the end of step 39. The mutations are in the entry above.
  - **Numbers at the end of the step.** Rust 1336 tests, 1495 with the `web` feature and 22 on the real program, 1331 without the default plugin formats and 1482 with `web`, and 3 of the shell's in each; web 1710 tests in 81 files;
    111 in browsers; 361 commits on the branch before this entry.

- [x] **41. The ring of a parameter that a lane's modulators and its group's both drive (phase 1).** A lane in a group can be driven by its own modulators and by its group's. The engine applies a group's modulators before the lane renders and the lane's own after,
  each sum from the value the user set, so that the lane's sum stands and the group's offset is not added to it (the tests of this step pin it); the page's ring added the cables of both, and the mock engine's value did too. *Done when:* the ring is where
  the lane's cables alone swing the parameter when any modulator of the lane drives it (one whose MIDI control is not bound yet too) and the group's where none does; the mock engine's value of a parameter that several cables drive is the
  engine's, and its MIDI control rests where the engine's does; the engine's order of writes is pinned by tests; and on the real program a group's envelope drawn to the cutoff that the tracer's LFO drives leaves the ring as it was, and the cutoff goes from 0.1 to 0.9 over a turn of the LFO as the ring says.
  - **As built (75f726c, 028385a, 4131b79).** `modulationsOn` finds whose each modulator is (the `parent` of its node is a group or a lane) and gives the lane's cables alone when there are any (`modulationsAt` is as it was: it is the settings'
    too, and a setting's last write is the lane's already, D41). The mock engine's `#effective` makes each cable's offset from the value the user set and adds them (it chained them: an envelope drawn after an LFO took its share of the
    LFO's swing, 0.45 where the engine has 0.65 for the tracer's cutoff with the LFO at the top and an envelope at rest) and keeps to the lane's where there are any (f135). Its MIDI control put out a constant 0.5, bound or not, and drove with an LFO's law, so a cable from
    one moved a dot where the engine's armed control does nothing (f137, found by reading the mock's `#effective` for the first): it puts out what it rests at now (nothing; the middle for the pitch bend wheel), and one function, `offsetOf`, is the law of what a modulator adds, for
    a parameter and for a setting of a modulator, which had each its own. Rust: two tests that pin the engine's order (ea8458d and 012e041:
    the lane's sum stands over the group's, and so does the value of a MIDI control that is not bound, which adds nothing). Vitest: 6 of `modulationsOn` and 13 of the mock (4 of the cables that drive a parameter, 9 of the MIDI control). Browser: `e2e/ring.spec.ts`, on the real program: a cable
    of the group's alone to the resonance gives that knob a ring, one to the cutoff leaves its ring as it was (the same path), and the cutoff, sampled for 2.6 seconds (1.3 turns of the LFO), goes from 0.1 to 0.9.
    Mutations: 2 of the engine (a grouped lane's own modulators left out, caught by both; a cable of the lane that adds nothing left unwritten, caught by the second), 7 of `modulationsOn`, 6 of the mock's cables and 6 of its MIDI control, and 2 in browsers (all the
    cables count; the group's alone do not), all caught; the mock's first run left one, the clamp, which a test of the clamp settled.
  - **Found on the way.** The engine test of a group modulator on a bus effect (`group_modulator_drives_bus_effect`) could not fail: it held no note, the test instrument puts out something only while a note is held (its
    comment said it did regardless), so the bus was silent whatever the modulator did, and the test passed with the call that applies a group's writes taken out of `process` (5526b83: it holds a note now, hears the bus first, and fails
    without the call; the test of a group modulator on a member's parameter did fail without it, and was fine). The dot of a knob was right all along: the layout names the first cable to aim at a value, a lane's before a group's, and the lane's value
    is what the engine stores in that slot.
  - **Limits, as built.** A parameter whose lane cables are taken out gets the group's ring back (the cables are the graph's, and the rule is read from them at every change). A lane's MIDI control that is not bound yet, alone among the lane's
    cables, leaves no ring over a group's cable: the parameter does stay at the user's value, which is what the dot says. The mock engine has no MIDI: its controls only rest, so a bound control moves nothing there. Only Chromium was tried.
  - **Tests.** Rust: 2 more in each configuration (1343 to 1345; 1502 to 1504 with the `web` feature; 1338 to 1340 and 1489 to 1491 without the default formats); web: 1729 in 81 files (1710); 114 in browsers (113).
  - **Numbers at the end of the step.** Rust 1345 tests, 1504 with the `web` feature and 22 on the real program, 1340 without the default plugin formats and 1491 with `web`, and 3 of the shell's in each; web 1729 tests in 81 files;
    114 in browsers; 375 commits on the branch before this entry.

- [x] **42. The scope: the output as a wave and as a spectrum, on the master node and in the terminal (an extra, ahead of phase 4).** CLAUDE.md had a TODO ("mirror the audio output into a lock-free ring buffer for the oscilloscope
  display") and the terminal's Scope tab was a placeholder; the page had nothing of the output but the master's meter. *Done when:* the audio thread keeps the last frames of the output mix in a ring that any thread reads without a lock;
  the page's master node draws them under its meter as a wave that stands still (the time base a choice) and as a spectrum on a logarithmic frequency axis, and asks the server for them only while the node is on the page; the terminal's Scope tab
  draws the same on a braille canvas; and on the real program the wave is flat in silence and goes up and down with a note, and the spectrum of a sine has its peak at the note's pitch, in a browser and in the terminal. *Not done (later steps):*
  a scope for each lane and each group (the ring is the master mix); a reading of the pitch; a vertical zoom (the tracer is quiet on purpose, an oscillator volume of 0.1, and its wave is two pixels high on the scope until the volume is put
  back to 1); freezing a picture; the terminal's tab being played into by the computer's keys (the terminal has no keyboard piano yet: a MIDI keyboard does it, or a browser with `--web`).
  - **As built.**
    - *Engine* (0e63b51): `ScopeRing` in `Telemetry.scope` (§6). 12 tests (the ring on its own, with a reader on another thread that always sees a run and the three ways the retry can go, made deterministic by `latest_checked`; and the
      graph's: the scope has the sum of two lanes, a buffer at a time) and 11 mutants, all caught; the graph's test of allocation covers the push.
    - *Wire* (752d677, 85163ce): `Meta.sampleRate` (the controller's own, the rate that the plugins are loaded at: a client needs it to put a frequency on a spectrum; `Meta` is camel case on the wire from now), and the `scope` channel, kind 3 (§6).
    - *Page, client* (1eaa208, 9f14f78, 52abc2b, f4192ce, 92a9819): `client/fft.ts` (`Spectrum`: a periodic Hann window, a radix-2 FFT of 2048 samples, levels in decibels with 0 dB a sine of full scale and a floor at -120 dB);
      `client/scope.ts` (`triggerStart`, `plotWave`, `plotSpectrum`, `frequencyColumns`, `levelRows`: pure functions of numbers); `Client.watchTelemetry(channel)` (a channel asked for besides the ones the client was started with, counted,
      asked again on every connection); and the mock engine's scope, which sounds like the notes that are held (a tone and the two harmonics above it for each note, the same on both sides) and which a lost connection makes a client forget.
    - *Page, drawing* (8f175db, 373ac6b, ad8c32c): `ui/scope-draw.ts` and `ui/LiveScope.tsx` (drawn on a canvas on every new frame and not by React, as the traces are; a meter to what reads the page, whose value is the loudest sample on either
      side), and the master node's scope with two choices under it: the view, and for the wave a time base of 256, 512, 1024 or 2048 samples, told in milliseconds from the sample rate (5, 11, 21 and 43 ms at 48 kHz). The choices are the viewer's
      own and not the session's (D43). The node is taller, 223 px as a browser draws it, so the layout assumes 228 (it was 164; a test keeps it between the measure and a little more).
    - *Terminal* (9929ecf): `src/tui/scope_view.rs` has the trigger, the FFT and the spectrum's points as the page has them (the page's tests ported, and one of the FFT against the sum that defines it), and the Scope tab: `v` (the wave or the
      spectrum) and `Left` / `Right` (the time base), the left side in teal and the right in green where it is not the same, a line that says the time base and the loudest sample in dB below full scale (red, with a plus, above it), the
      spectrum's lines at 100 Hz, 1 kHz and 10 kHz and at every 20 dB, and levels that fall 40 dB a second. It copies the ring on every frame that it draws while the tab is shown (about ten a second).
    - *Browser tests* (db622c3): on a program of its own, the scope of the master is silent until a note is played, draws a wave that is 31 pixels high while it is held (the oscillator's volume put back to 1: the loudest sample is 0.47) and a
      flat one again after it, and the spectrum of a sine has its peak at 135.5 Hz for a note of 130.8 Hz, 56 pixels above the middle of the line.
  - **Found on the way.** The first version of the browser test of the spectrum took the leftmost pixel of the top row of the line for the peak, and a spectrum that was a flat line along the top (no levels worked out: a mutant) passed it: that pixel
    is where the line crosses the grid line of 100 Hz (measured: the pixels of the line's top row have an alpha of 191, under the test's 200, and the one over the opaque grid line 255), and that column is 100.6 Hz in the test's own mapping, just inside the window that it asked for. The measure is column by column now (the highest and the lowest pixel of the line's
    colour in each), the peak has to stand clear of the median of the line, and the line has to go all the way across; the mutant is caught. Running the terminal on the real program (below) found that the tail of a filter, which is never exactly
    nothing, showed as a level of -135.7 dB a second and a half after a note; the peak is a dash under what the meters call silence (`telemetry::SILENCE`, 1e-6, which is -120 dB) as the meters' is. The first wave test failed for a reason that was
    no defect: the tracer's wave is two pixels high.
  - **On the real program, in the terminal.** `tang --web=127.0.0.1:18799 --audio-device none` in tmux with the tracer at volume 1, and a browser that held Z: the Scope tab's wave showed three periods of the filtered saw, standing still, with a peak
    of -7.5 dB; the time base (Right) 43 ms with -8.6 dB; the spectrum (v) a peak just above the line of 100 Hz (the note is 130.8 Hz) and the harmonics falling away, with -9.5 dB; and 1.5 seconds after the key was let go the levels had fallen (40 dB a second) and the
    peak was -77.3 dB. The tab bar said `web 127.0.0.1:18799 (1)`. (The null backend: nothing was heard, and a sound card was not tried.)
  - **Limits, as built.** The resolution of the spectrum is that of a block of 2048 samples, 23 Hz a bin at 48 kHz, so that notes in the bass are a few bins wide and the picture says nothing of a pitch to a semitone (a reading of the pitch would
    interpolate between bins). The frames are 43 ms of the output every 33 ms (the page) or about every 100 ms (the terminal), so that a sound is seen in snapshots, which stand still where the sound is periodic and the trigger finds its place;
    a sound that is not (noise, a chord of unrelated pitches) moves from frame to frame. The spectrum is of the mix of the two sides, so that what is in anti-phase on them is not seen. The page's choices are in the node: a reload goes back to the wave at
    21 ms. A client that asks for the scope costs the server 16 KB thirty times a second, and the page asks only while the master node is there. Only Chromium was tried; no sound card.
  - **Tests.** Rust: 50 more in each configuration (1345 to 1395; 1504 to 1558 with the `web` feature; 1340 to 1390; 1491 to 1545), 36 of them the terminal's (30 of the scope's pure parts and state, 4 golden pictures and 2 of the keys), and 4 more with the `web` feature than without (the server's side of the channel); web: 1829 in 85 files (1729 in 81); 116 in browsers (114). The mutations of each part are in the commit that has it: 11 of the ring, 1 of the sample rate, 10 of the FFT, 21 of the plots and the graticule, 14 of the channel, 9 of `watchTelemetry`, 8 of the mock's scope, 12 of the drawing, 17 of `LiveScope`, 10 of the master node, 7 of the page in browsers, and 35 of the terminal's tab, all caught (the first runs left some in the FFT, the plots, `LiveScope` and the browser tests, each settled by a test or an assertion).
  - **Numbers at the end of the step.** Rust 1395 tests, 1558 with the `web` feature and 22 on the real program, 1390 without the default plugin formats and 1545 with `web`, and 3 of the shell's in each; web 1829 tests in 85 files;
    116 in browsers; 389 commits on the branch before this entry.

- [x] **43. A setting that is turned on a log scale is modulated on that scale (a bug that the user reported, in the engine and in the page).** "When an LFO connects to an ADSR (attack) there is a small bug in how it modulates." *Done when:* a cable to an LFO's rate or to an envelope's
  attack, decay or release moves the setting a fraction of the knob's travel, in the engine, in the page's ring and dot and in the mock engine, so that a swing goes as far up the knob as down it; what is turned by distances (a sustain, the depth of a cable, the parameter of a plugin)
  goes by the range as it did; and on the real program the Attack knob of an envelope that an LFO drives has the ring and the dot where the law puts them. *Not done:* a parameter of a plugin that the plugin turns on a log scale (`ParameterInfo` has no taper, so that the
  page and the engine move it by its range, as they did).
  - **What it was.** Measured on the real program (a script that adds an envelope to the tracer, draws the LFO to its Attack and samples the knob): the Attack runs from 0.001 s to 10 s, four decades, so that the user's 10 ms is a quarter of the way up its knob, and the engine's law for a cable,
    `base + output x depth x range`, took it at the default depth of 0.5 from the lowest value to 5.0 s: the ring drew 0 to 92 % of the knob round a pointer at 25 %, and the dot, which follows the engine's value, sat at the lowest value for half of each turn of the LFO and went to the top in the other
    half. The same was true of a rate (0.01 to 50 Hz), a decay and a release. A depth meant nothing at the bottom of such a knob and everything at the top.
  - **As built.** *Engine* (42ab454): `Modulator::modulated_value` writes what a cable makes of a setting for `apply_cross_mod` and for a group's cable to a member's modulator (`collect_group_member_mod_writes`); for a rate, an attack, a decay or a release (`ModTargetKind::is_turned_on_a_log_scale`,
    `CrossModField::is_turned_on_a_log_scale`) it puts the user's value at its place on the knob, `ln(value / min) / ln(max / min)`, lets the source offset that place by its own law with the knob's travel, 0 to 1, as the range (`Modulator::offset`, the law of `target_offset` over the numbers it is given: an LFO
    plus or minus `depth`, an envelope from `depth` of the way down to the value the user set, a MIDI control as before), and puts it back as a value, `min x (max / min)^place`. A range that starts at nothing has no decades and goes by the range, and a value beyond the range is taken at its end, as the knob puts it.
    *Page* (28febbf): `settingExtent` takes the scale of the setting (`MODULATOR_FIELD_SCALES`, which the controller generates) and draws the ring of such a setting by the same law, the mock engine's `#settingEffective` does as the engine does, and how a range is spread over a knob (`Scale`, `isLog`, `fractionOf`, `valueAt`)
    moved from `ui/knob-geometry.ts` to `client/travel.ts`, which the knob's module still exports. *The real program:* a new browser test draws an LFO to the Attack of an envelope and reads the knob: the ring is the arc from 0 to 0.75 of its travel, and over two and a half seconds the dot goes to both ends of it and no further.
  - **Found on the way.** The engine's tests of a sustain could not tell the two laws apart: a range that starts at nothing is linear on either, and the sustain's is 0 to 1. They have a range from 0.1 (and one from nothing, which must not come out as not a number). Four of the twenty mutants of the engine's law did not build, and two did not change anything;
    each was settled before they counted. The page's mutants left three: the default scale of `settingExtent` (a test with a range above nothing and no scale), an outer clamp that `valueAt` makes itself (taken out), and `value <= min` in the travel, which is 0 either way (it changes nothing).
  - **Limits, as built.** A saved session whose cross-mod cable (`mod_rate`, `mod_attack`, `mod_decay`, `mod_release`) was made with the old law sounds different: its depth is a fraction of the knob now. Of the saved sessions on this machine two have one, an envelope on the rate of an LFO, at depth 1.0 and 0.35: at depth 1.0
    it goes from the lowest rate at rest to the value the user set at its peak, as it did, but along the knob and no longer along the range, so that the rise is by ratios.
  - **Tests.** Rust: ten new in `plugin/chain.rs` (the law for each of the four settings, up and down; the swing's ends are as far from the user's value on the knob either way; the envelope's and the MIDI control's laws; the ends of the range; a range of one value and one from nothing; a sustain and a depth that go by the range; a group's cable;
    a value beyond the range) and six that pinned the linear law pin this one: 1395 to 1405 (1558 to 1568 with the `web` feature; 1390 to 1400; 1545 to 1555). Web: seven new (six in `modulation.test.ts`, one in `connection.test.ts`) and the ones that pinned the old law say the new: 1829 in 85 files to 1836 in 85;
    one more in browsers, 117 (116). Mutants: 20 of the engine's law, all caught after the four that did not build and the two that changed nothing were settled; 27 of the page's, 25 caught and two that change nothing; two against the real program, caught by the new browser test.
  - **Numbers at the end of the step.** Rust 1405 tests, 1568 with the `web` feature and 22 on the real program, 1400 without the default plugin formats and 1555 with `web`, and 3 of the shell's in each; web 1836 tests in 85 files; 117 in browsers; 394 commits on the branch before this entry.

- [x] **44. The canvas stays where it is, and a button that says Arrange arranges it (a bug that the user reported).** "When attaching a cable the UI gets rearranged, which I don't think should happen (maybe have an auto-rearrange button?)" *Done when:* a cable that is drawn or taken out moves no node; a
  node that is added or taken out leaves the others where they are, but for what needs room (what has grown, a strip that has no room above its row for the modulator that is added to it) and what is in a column that moves; a button that a person finds arranges every node where the layout has it, and then
  shows them all; and on the real program a cable from an LFO to the Attack of an envelope moves no node. *Not done:* the places that the layout remembers are the page's and not the session's (a page that is opened again, and a second browser, lay the nodes out from nothing: D45, Open question 6); a cable between two
  modulators that are in one row runs across what is between them until the nodes are arranged (f39).
  - **What it was.** `toFlow(graph)` laid the whole session out from nothing at every change of its structure (D15). A cable from one modulator to a setting of another makes the one that drives a row above the one it drives (step 26, `tiersOf`), which moved the strip below it down by the room of a row: every node of the
    canvas moved under the hand that drew the cable. A chain that closes up when an effect is taken out and a lane that gets a row of modulators were the same.
  - **As built.** *Layout* (8c25d9d): `toFlow(graph, remembered)` is given where it put the nodes last (`Flow.places`: the position and the slot of each node that no person has placed) and leaves a node where it was while its slot is the same (`slotsOf`: an effect is in the place of its chain that it is at, a modulator is in
    its rack and not at a place in it, a lane is in the session, a group and the master are in the column that the layout gives them, and what is in a group's strip is in the slot of the group, so that the strip goes along with it when a lane grows past it). What is new goes where the layout puts it, as far from its strip as the layout has it
    (`carryOver`); a lane that comes to be where one stayed goes below it, and a modulator above the modulators that stayed (`clearOfKept`). Room is made by rows (`makeRoom`): the nodes that are level (a strip's row, a row of modulators) are moved together, from the top, below what they come to be over, with the gap of a row, and every row below them
    by as much, so that a node that has grown (a knob pinned to it, a cable listed in it), or a modulator that is added to a strip that has no room above its row, moves the strips below it down as they are, and the rows never come nearer to each other. A node that a person has placed is not moved by the layout, and what comes to be on it, or within 16 pixels of it, is
    moved clear of it by itself, last (`clearOfPlaced`, as it was). `Flow.arranged` says whether every node is where a layout from nothing puts it. *Hook* (8c25d9d): `useLayout` keeps the places from one change of the session to the next and lays the nodes out from nothing when it is asked to (`arrangeAgain`); while the places that a person had given
    are being given back, one answer of the server at a time (`Editor.resetLayout`), it lays out from nothing each time (`arranging`), so that a node that was moved clear of a place that is given back goes where the layout has it when the last is. *The control* (2c1f0f7): a button in the corner of the canvas, "Arrange" (to a screen reader "Arrange the nodes
    again", as it was when it was a small arrow among the zoom's), lit while a node has a place or one is not where the layout has it; pressed, it lays the nodes out from nothing, gives the places back and fits the view to every node, when they are all given back (`FitWhenArranged`).
  - **Found on the way.** Drawing the scenes of the layout before it was committed found what the test of one cable does not: a group's bus effects and modulators were left in their column when the group went along (the group came to be on its own effect, and was moved below it, with the master), strips broke in two when a node grew
    (the instrument went below and its effects stayed), and a modulator that was added to the second lane went above the first, since the strip above it was in the way. The slots of what is in a strip name the slot of its parent, room is made by rows, and a modulator keeps clear of the modulators that stayed and not of the rest. The browser test that was there
    for arranging the nodes (a node dragged, a second page, the saved file) found that after the nodes were arranged the button stayed lit: the nodes that the layout had moved clear of a place that was given back stayed where they went (`arranging`). Of the 118 browser tests 23 failed on the change: one (`stress.spec.ts`) sampled the dots at an unlucky time and passes alone
    (three times) and in the next whole run; five said that a cable arranges the canvas (`cascade.spec.ts`, two of `build.spec.ts`), and press the button now, after saying that the cable moved nothing; and the rest began a drag where the view was moving to show a node that was added, which is not where the layout put the others now (an envelope goes above the first row, and the view moves to
    it): every helper that adds a node waits for the canvas to stop moving (`addEnvelope`, `still`; `showAll` for a group's modulator that leaves the lane's out of the view).
  - **Limits, as built.** The places that the layout remembers are the page's: the nodes that a person has not placed are laid out from nothing when a page is opened, so that a person who has drawn cables and reloads sees the rows above the strip that the layout has, and a second browser does too. Until the nodes are arranged a cable between two modulators in one row runs across what is between them, and its slider can be over a node
    (f39). A node that is taken out leaves the room it had (an effect's chain closes up, nothing else does), and "Arrange" closes it. The view moves to show a node that is added where it cannot be seen, and may leave others out of it. A row that makes room moves every row below it, in every column, so that a modulator added to a group moves the lanes that are lower than it; a node that has grown past a place
    that a person gave moves clear of it by itself. "Arrange" moves the view and the zoom too. Only Chromium; the pictures were looked at on the real program (a cable to an envelope's Attack, arranged; a group; an envelope in the group).
  - **Tests.** Web: 22 new in `toFlow.test.ts` (93 to 115: the scenes above and the ones that came out of drawing them, a group's strip, a strip that grows, a modulator in a lane that has no room, a node put on one that stayed, a lane after a gap, a rack with a hole, a place that was remembered and is not any more, `arranged`), seven in `PatchView.stable.test.tsx` (the canvas as it is drawn, with the control),
    two in `PatchView.fit.test.tsx` (the view is fitted once, and when the last place is given back), and the arranging test of `PatchView.move.test.tsx` says that the button is dim again: 1836 in 85 files to 1867 in 87. In the browser: a new test (an LFO drawn to the Attack of an envelope moves no node; the button has its word, is over none of the nodes and arranges them, and every node is in the view), and `cascade.spec.ts`
    says what it said of the rows with the button pressed: 118 (117). Mutants: 48 of the layout, the hook and the control, 47 caught and one that changes nothing (a slot with the node's own number in it); ten of the fit, nine caught and one that changes the room of the canvas's first fit, which no test in jsdom sees; three against the real program, all caught.
  - **Numbers at the end of the step.** Rust 1405 tests, 1568 with the `web` feature and 22 on the real program, 1400 without the default plugin formats and 1555 with `web`, and 3 of the shell's in each; web 1867 tests in 87 files; 118 in browsers; 395 commits on the branch before this entry.

- [x] **45. An LFO that is random: `random` and `smooth`.** *Done when:* an LFO can be given the waveform `random` (sample and hold: a number for each cycle, held until the next) or `smooth` (the same numbers, glided between) in a session file, in the
  terminal's waveform row and in the page's choice of a waveform; the engine puts out the number of the cycle it is in, and the same numbers each time a session is loaded; the page's mock engine draws the same numbers from the same hash, pinned to the engine's by a table that the engine
  writes; on the real program the LFO's bar holds one number for a whole cycle (`random`) and glides without a jump (`smooth`). *Not done:* a seed that a person can choose or a button that draws new numbers (D46, Open question 7); a random modulator that takes a new number for each note
  and not for each cycle; numbers for each voice (a modulator is lane-wide, as it was).
  - **What it was.** An LFO had four shapes, all of them a function of its phase, so that no cable could take a parameter to a number it had not been at before: a random melody on a filter's cutoff, a pan that is different for each cycle, a pitch that drifts. A modulator of that kind needs a
    number that changes from one cycle to the next, and `Modulator::tick` had none (`waveform.eval(phase)`).
  - **As built.** *Engine:* `LfoWaveform` has `Random` and `Smooth` after `Square` (indices 4 and 5, so that the index of each of the four that were there, a saved `waveform = "square"` and the protocol's order stay as they were; `ALL`, `name`, `from_str` (`random`, `s&h`, `sh`, `smooth`),
    `next` and `prev` follow the list). The number of cycle `n` of an LFO that has the seed `s` is `noise(s, n)`: the top 24 bits of `lowbias32(lowbias32(s + 0x9E3779B9) + n)` over 2^23, less 1: from -1 up to but not including 1, and exact in a 32-bit float (the seed is offset first, since the hash of 0
    is 0 and the numbers of seed 0 would have started at the bottom of the range). `LfoWaveform::eval_cycle(phase, seed, cycle)` is a shape that goes by the cycle as well as the phase: `random` is the cycle's number, `smooth` is a smoothstep from the cycle's number to the next cycle's (no corner at
    either end, so that the cycles join; a phase that is not in 0 to 1, which a negative rate in a hand-written session makes, is at the nearer end, so that it stays between its two numbers), and `eval(phase)`, which only the tests use now, is the first cycle of seed 0. `Modulator` has a `seed` and a `cycle`;
    the cycle is counted as the phase wraps (`turned as u32`: turning the rate does not start the numbers again, and a rate that goes round more than once in a buffer counts every cycle: 50 Hz at 1,024 frames is 1.07 a buffer). `AudioGraph::next_modulator_seed` gives every modulator that comes in the next seed,
    one counter for a lane's and a group's, so that a session that is loaded has the seeds in the order of its file. *Controller and protocol:* `ModulatorField::Waveform` runs from 0 to the last of `LfoWaveform::ALL` (derived, `(0, 5)`), the protocol's `Waveform` has the two names, and the generated `Waveform.ts`, `WAVEFORMS` and
    the range are regenerated; the loader, the saver (`waveform.name()`), the tree's label (`LFO 4.0Hz random`), the intents and the client's reducer needed nothing else, since they go by the list. *Terminal:* the waveform row of an LFO is built from `LfoWaveform::ALL`, so `Left` and `Right` go round six.
    *Page:* the choice of a waveform lists them (it reads `WAVEFORMS`), and the mock engine draws them: `client/noise.ts` is `noise` in TypeScript (`Math.imul`, the low 32 bits of the seed and of the cycle), pinned to the engine's by `fixtures/noise.json`, 130 numbers that the engine writes (`the_web_clients_fixture_of_the_noise_is_current`; the ends of the
    32 bits are among them), and `shape(waveform, turns, seed)` takes the cycle from the whole turns; its seed is the node's id (the engine's is the order in which modulators came in, which a page does not know: D46).
  - **Found on the way.** The first draft of the engine's test of a smooth LFO took two cycles whose numbers fall for two that rise (the numbers of seed 5 at cycles 2 and 3 are 0.94 and 0.53, at 1 and 2 they are 0.56 and 0.94): the test now looks the two up and says that they rise. The browser test of the smooth LFO said that it jumped (a step of 1.25 between two frames, once in
    three runs): it was the first frames of the window, which still showed what the sine had put out, the telemetry being a few frames behind the server's snapshot that the test waited for (every other step was 0.044 or less, which is what a smoothstep over two seconds allows); both new browser tests wait 300 ms before they look, as the test of a negative depth does, and passed six runs in a row after that.
    A smooth LFO whose phase is below 0 gave 31.8 at a phase of -3 in the test that was written for it (a rate below 0, in a file written by hand: the loader checks that a number is finite and not its sign, and the saw and the triangle are out of their range there too: f147), and its phase is clamped.
  - **Limits, as built.** The seed is not in the session file (D46): a session that is loaded draws the same numbers every time, but a modulator that is added later or that undo puts back has the next seed and draws other numbers than it did, and the mock engine draws other numbers than the engine for one session (its seeds are node ids). The engine's modulators are block-rate: a random LFO is
    one number a buffer at most, whatever its rate. The page's trace of a modulator has 30 values a second (f85), so that the steps of a random LFO of 15 Hz or more are only roughly drawn. Only Chromium.
  - **Tests.** Rust: 18 new (the known numbers of `noise` and its range, spread and seeds; the first cycle of a random and of a smooth shape, and a smooth one outside its cycle; an LFO that holds, glides, counts two cycles in a buffer and goes on when its rate is turned; the seeds of a graph, a lane's and a group's among them; the list, its names and its indices; the range of the setting;
    a random waveform to the engine; a save and a load of each; a client told of one; the file that the engine writes for the page) and nine that name the waveforms or count them, which now name and count six: 1405 to 1423 (1568 to 1586 with the `web` feature; 1400 to 1418; 1555 to 1573). Web: six new (four of the hash, two of the mock engine's LFO) and five that said four say six: 1867 in 87 files to 1873 in 88; two more in browsers, 120 (118).
    Mutants: 41 of the engine (the constants and shifts of the hash, the order of seed and cycle, the glide and its easing and its clamp, the counting of cycles, the seeds of a lane's and a group's modulators, the names, the list, the range, the protocol's names), all caught; 19 of the page's (the hash, the cycle of a shape, the glide, the seed), all caught. The terminal was looked at on the real program in tmux (the null audio device, no MIDI): on the tracer's LFO the waveform row went from `sine` to `random` (four steps with `Right` from `sine`) and to `smooth`, and the tree and the pane's title said `LFO 0.5Hz random` and `smooth`.
  - **Numbers at the end of the step.** Rust 1423 tests, 1586 with the `web` feature and 22 on the real program, 1418 without the default plugin formats and 1573 with `web`, and 3 of the shell's in each; web 1873 tests in 88 files; 120 in browsers; 397 commits on the branch before this entry.

- [x] **46. A scope on every node that has a signal: instruments, effects and groups (an extra, ahead of the list).** *Done when:* a person can press a button in the header of an instrument, an effect
  or a group and see what comes out of that node, as a wave or a spectrum, in the node on the page; the engine listens to a node only while somebody watches it; two browsers can watch one node, and
  one that stops leaves the other's drawn; the layout makes room for a scope that is shown, so that nothing lies over what is below it; and on the real program the scope of an instrument is loud where
  the scope of a filter that is shut, and the master's, are nearly silent. *Not done:* a scope in the terminal for each node (the Scope tab is the master's; step 47 gave the terminal one); a scope for a node that no ring is
  left for (there are eight, shared by every browser: f151); scopes that are kept, with the session or with the page (Open question 8).
  - **What it was.** The page had one scope, the master's (step 42, D43), which says what the whole session puts out and not what a node does to the sound: with a filter, a reverb or a bus in a chain,
    the question is what comes out of *that* node. D43 had left it out for a reason, a ring and a store in every lane and group for pictures that nobody asked for.
  - **As built.**
    - *Engine* (1a73440): `Telemetry.probes`, a pool of `PROBES` = 8 rings of the scope's kind (64 KB each, made with the telemetry, so that asking allocates nothing on the audio thread); `ProbeTarget` (a lane's
      instrument, an effect of a lane's chain, a group, an effect of a group's bus), `GraphCommand::SetProbe { target, ring }` and `ClearProbes`; a tap at the end of a lane's instrument stage (after its volume,
      on the fast path too) and after each effect's mix, and in `Group::finish` (a bus effect after its mix, the group after its pan), each a store into the ring that it was told (a lane or a group that nothing listens to has an empty list of taps, an empty-slice check at each stage).
    - *Controller* (157da8a): `app::probes` (`Watcher`, `Probes`, `Controller::set_probes`, `reconcile_probes`): which watcher watches which nodes, which ring each watched node has (two watchers of a node share its
      ring; a node that no ring is left for has one when one is free; a node that has no signal, a modulator, a lane that has no instrument, the master and a node that is not there, has none), and what the audio thread was
      last told (where a node is in the graph changes when lanes and effects do, so that it is worked out again whenever the structure changes, from `conclude`, and on every `poll`). `Telemetry::set_probe_node`
      says which node each ring is for, for the pusher. With the `web` feature or in tests only (in every build since step 47).
    - *Server* (a37af57): `ClientMessage::Probes { nodes }` and `ServerMessage::Probes { nodes }` (the generated types are committed), `probe_frame` (channel 4, the node's id where the layout's revision is, 2 × 2048
      values), the hub's list for each client (`MAX_PROBES`, 64, limited in rate like a request), the pusher's frame for each ring that a client wants, the owner's answer, and `refresh_probes`, which lets
      go of what a client that has gone watched (`Runtime::refresh` takes the controller for it).
    - *Client* (0f09fb8): `Client.watchProbe(node)` (counted, asked again on every connection, forgotten with it), `ClientState.probed`, `TelemetryBuffers.probe(node)` and `probeFrames(node)`; the mock engine has the
      eight rings (given in the order the clients asked) and sounds like the notes that are held, as its scope does.
    - *Page* (327cf58): `NodeScope.tsx` (`ScopeButton`, `NodeScope`, `ScopePanel`, which `MasterNode` is drawn with too), `scopes.ts`, the set that `PatchView` holds, `toFlow(graph, remembered, scopes)` with `canScope`,
      `PatchNodeData.scope` and `NODE_SCOPE_HEIGHT`, and `useLayout(graph, scopes)` (§9.3).
    - *Page, after* (7a9bf35, b892fa6): the badge of a node under its name (f148, below), and the canvas moving to a scope that is opened where it cannot be seen (`rectOf` counts the scope and the rows of the cables a modulator lists;
      `PatchView` asks `Reveals` once the layout has the scope: §9.3).
  - **Found on the way.** The height of a scope was measured, and not guessed: a throwaway spec on the page's mock engine read the node before and after (a node 360.3 px tall on the screen at a zoom of 0.948 became 509.2, and
    a second node 257.9 became 406.8: 148.9 px both, 157 in the layout's pixels: the scope's 96, the gap, the 41 of the choices and the foot). A canvas that remembers moves the row that a scope is in the way of, and a lane that is level with
    the group below moves with it, where a layout from nothing leaves the lane and moves the group alone (the test of two groups says both). Two lanes of one name are numbered ("Oscillator 1"), so that the first test
    of the room asked for a button that was not there. The browser test of the real program first said that every scope was not silent when the note ended: the filter's and the master's were at 1.07e-42, the tail of the filter shut at 20 Hz, a
    number that is not zero and no sound (f149: the engine does not flush denormals; its cost was not measured), and the test says a millionth. The scope button took 30 pixels of a header that had 40 for the name of a lane that has a number in it ("Os…") and 30 for Pianoteq 9 ("Pian…"): found by looking at the page on the user's own
    examples (the split example, the Pianoteq session) once the step was done, and fixed in 7a9bf35 (f148: the badge is under the name). The browser test of the real program failed once in a whole run, on a key pressed before the focus that the removal of the LFO gives to the button that adds a modulator had come (ee69778), and again,
    by a timeout, when the canvas began to move to a scope that is opened: the test took the LFO out after it had opened two scopes, and the LFO was above the view, where Playwright cannot scroll to a node of the canvas ("element is outside of the viewport"); the test sets the sound first now (b892fa6).
  - **Limits, as built.** There are eight rings for every browser together: a ninth scope shows a flat line, and says nothing (f151; `ClientState.probed` has what the server says is watched, but a node that was asked
    for and left out cannot be told from one whose answer has not come). A ring that goes to another node keeps the last frames of the one it was for until the audio thread stores over them, so that a scope
    can show up to 43 ms of another node's signal at first (f150). The scopes are the page's: a reload, and anything that takes the canvas off the page, closes them (Open question 8). The terminal had no scope for a node (step 47 gave it one). Only Chromium;
    the sound only on the null backend.
  - **Tests.** Rust: 41 new (12 of the engine's taps, 12 of the controller's registry and 17 of the server's: the messages and the frame, the hub, the connection, the owner and the pusher, two of them on the real program), 1423 to 1448 (1586 to 1625 with the `web` feature;
    1418 to 1443 without the default plugin formats; 1573 to 1612 with `web`); on the real program 22 to 24. Web: 19 of the client (`watchProbe`, the frames, the mock's rings), 24 of the page (nine
    of the layout, 15 of the nodes: the button where it is and where it is not, the scope and its choices, what is asked of the server and let go of, the frames drawn, undo, the room and arranging), two of the header's title and six of the canvas's move to a scope that is opened (`rectOf`, the move and when it is not made): 1873 to 1924; browsers 120 to 126 (the sizes, the signals, two pages, the move to a scope, and two of the headers). Mutants: 22 of the engine's taps, 22 of 23 of the controller's
    (the one that is left was a clause that no input made a difference to, and was taken out), 21 and one of the server's, 30 of the client's, 30 of the page's, three of the header's and seven of the move, all caught; three of the page's in the browser (and two of the move), of which two were caught and the third, a scope that is
    never let go of, is not seen by a test that has a second page that holds the node, and is caught by the page's own.
  - **Numbers at the end of the step.** Rust 1448 tests, 1625 with the `web` feature and 24 on the real program, 1443 without the default plugin formats and 1612 with `web`, and 3 of the shell's in each; web 1924 tests in 89 files; 126 in browsers; 408 commits on the branch before this entry.

- [x] **47. A scope of the selected node in the terminal (an extra, ahead of the list).** *Done when:* a person can press `o` on the Session tab of the terminal and see, under the chain and the parameters, what comes out of the node under the chain cursor, as a wave or a
  spectrum; the engine listens to that node only while the strip is shown on that tab, and lets go of it when the cursor is on another node or the tab is left; `tang --web` keeps the terminal's watch when a browser that watched goes away; and a node that has no signal, or
  that no ring is left for, says so. *Not done:* a time base for the strip (it is 21 ms), the key in the action bar, a word on a terminal too short for the strip (it needs 24 rows, and shows nothing under them), and the strip's being kept anywhere.
  - **What it was.** The TUI is first-class, and step 46 had given the scope of a node to the page alone: the terminal had the master's (the Scope tab) and nothing for a node, and the controller's registry of what is watched (`app::probes`) was built only with the `web` feature, so that
    a program without it could not ask the audio thread for the signal of a node.
  - **As built.** *Controller:* the probes are in every build (`app::probes`, `Telemetry::set_probe_node`, the controller's `probes`, `reconcile_probes` in `poll` and after a change of structure; `keep_probes_of`, which only the web interface's owner calls, stays with the `web`
    feature or tests, and the three `expect(dead_code)` on `GraphCommand::SetProbe`, `ClearProbes` and `ProbeTarget` are gone). `Watcher::TERMINAL` is `Watcher(u64::MAX)`, the terminal's own number (the hub numbers its connections from one), and `Owner::refresh_probes` keeps it when
    it lets go of the watchers of the connections that have closed, so that a `tang --web` has both. `Controller::probe_ring(node)` says which ring of the pool a node has now, for a reader that has the node and not the ring. *Terminal* (`src/tui/scope_view.rs`, `src/tui/mod.rs`):
    `Strip { on, scope, watching }` has a `Scope` of its own (its view and its time base are not the Scope tab's); `State::selected_signal` says what is under the chain cursor if it has a signal (an instrument that has a plugin, an effect, a group, an effect of its bus) and what
    to call it (a group by its name, or `Group n`); `Strip::sync(ctl, shown, selected)` asks `set_probes(Watcher::TERMINAL, …)` when the node to watch is another (the one under the cursor while the strip is on and the Session tab is shown, and none otherwise) and forgets the
    picture; `Strip::take` copies the latest frames from the ring that the node has; `split_for_strip` gives the strip the foot of the tab, ten rows, when it is on and the tab has 22 rows (24 for the terminal); `render_strip` draws the box for one of three states
    (`Looking::Nothing`: "No signal here…", `NoRing`: "No ring is free…", `Ring`: the picture, with the name, the view, the time base and the peak in the top line, red over full scale), the picture being `scope_view::paint`, which the Scope tab's `render` uses too now. `o` shows the strip
    and takes it away, `O` chooses the view (the Help tab lists both); `State::sync_strip` runs from every tick of the loop, whose every turn begins with one, so that what the terminal's events and the browsers' requests did to the cursor and to the nodes is in it before the frame
    (the tests' keys call it in `process_event`).
  - **Found on the way.** Working out what a mutant of the strip could change, before the first run of them, found a bug and two clauses that did nothing. The bug: when the cursor went to another node the strip kept the levels of the node it left, and a spectrum falls 40 dB
    a second, so that the levels of a loud node were seen under the name of a quiet one for a second or two (the new test was red first: the peak of 0.5 stayed); `sync` forgets the picture when the node changes. The clauses: the `silence` that `take` made for a node that has no ring (the
    picture was silent already, from `sync`) and the time of the last look that `silence` forgot (the levels are at the floor, so that there is nothing to fall from); both are out. `render_strip` took a name and whether the node has a ring, and one of the four combinations could not
    happen (a ring for no node): it takes one of three states now. A `sync_strip` after the events in the loop did what the next turn's tick does, and was taken out (the tests' `process_event` keeps it). The first run, of 44 mutants, left four alive, and they were tests that were too soft (a least height that the
    test wrote with the constant that it tested, a right side that was silent in the ring of the test of `silence`, a spectrum whose fall no test ticked across, and `O` on the other tabs): the tests were made harder, and the four were caught. The three `expect(dead_code)` that un-gating the probes made unfulfilled were
    found by the first clippy of the matrix, the build without the `web` feature, and by none of the tests.
  - **Limits, as built.** Under 24 rows `o` does nothing that can be seen (the strip is on and the node is watched, and there is no room; the Help tab says 24 rows, and the action bar does not list `o`: its hints are in about a hundred snapshots). The strip's time base is 21 ms and
    has no key (`Left` and `Right` are the parameters' on this tab), and its view is not kept. The ring that a node is given keeps the frames of the node it was for until the audio thread stores over them (f150), and the strip copies them at once, so that for the first buffer or so (up
    to 43 ms) a picture can be of another node's signal, as the page's can. The eight rings are the terminal's and every browser's together: a ninth watcher is told "No ring is free" (the page says nothing: f151). The picture is drawn about ten times a second, as the Scope tab's.
  - **Tests.** Rust: 25 new, 1448 to 1473 (1625 to 1651 with the `web` feature, where the owner's is one more: 26 new; 1443 to 1468 without the default plugin formats; 1612 to 1638 with `web`); on the real program 24, as before. Two are the probes' (the ring that a node has; the terminal
    and a client sharing a ring, and the terminal not being a client that goes), one the owner's, four `scope_view`'s (the strip's room, its three states, a box with no room inside it, `silence`) and 19 the terminal's (the keys and what each asks of the audio thread; the tab; the cursor, a
    group and its bus; the names; a lane that has no instrument; the peak in red; the ring that a node has when it is not the first; the chain's rows with the strip; the fall of a spectrum across a tick; a node that a browser takes out; the picture forgotten when the node changes; a node that
    has no ring; and four snapshots, a wave, a spectrum, a modulator and a silence). Web and browsers are as they were, since nothing of the page changed: 1924 tests in 89 files, 126 in browsers. Mutants: 45, all caught (26 of `scope_view`: the strip's sync, its take, its room, its title and its box, and `silence`; 16 of the terminal's `mod.rs`: the keys, what is under the cursor and the names it has,
    the sync, the room and the frame; two of the probes' `probe_ring`; and one of the owner's, with the `web` feature), four of them only after the tests were made harder. Looked at in tmux on the real program (the null audio device, a note held from a page): the wave of the instrument (-32 dB: the tracer is quiet on
    purpose), the wave and the spectrum of the filter (the harmonics of the saw, with the lines at 100 Hz, 1 kHz and 10 kHz), the LFO ("No signal here…"), the Piano tab (no strip, and the watch let go of), a terminal of 23 rows (no strip) and of 24 (the strip, and a chain of ten rows).
  - **Numbers at the end of the step.** Rust 1473 tests, 1651 with the `web` feature and 24 on the real program, 1468 without the default plugin formats and 1638 with `web`, and 3 of the shell's in each; web 1924 tests in 89 files; 126 in browsers; 410 commits on the branch before this entry.

- [x] **48. A macro: a knob that drives many parameters (an extra, ahead of the list).** *Done when:* a modulator can be a macro, a number from 0 to 1 that a person sets and nothing else moves, which drives each parameter that has a cable from it as a CC does (at rest it adds nothing, at full it adds the depth of the cable of the range); it is made from the menu of a knob and from the "+" of a lane or a group in the page, and from the popup of a parameter in the terminal; it is turned on its node in the page and in the first row of its pane in the terminal; the session file has it (`type = "macro"`, `value`); one Undo puts its value back and another takes it out, and a Redo makes it again; and on the real program a macro that is turned up moves the engine's value of what it drives, which the dot of the knob shows. *Not done:* a cable to a macro's value (a MIDI control that turns it from a hardware knob, an LFO: Open question 9), a macro with a name of its own.
  - **What it was.** Every source of modulation moved by itself or by a controller (an LFO, an envelope, a MIDI control): none was a number that a person sets. One knob that moves several parameters at once (the brightness of a patch: the cutoff up, the resonance down, the reverb's mix up) had no place in the model: it took a MIDI control bound to a hardware knob, or an LFO at a crawl.
  - **As built.**
    - *Engine* (`plugin/chain.rs`): `ModSource::Macro { value }` puts out its value and has the CC's law, `value x depth x range` (`Modulator::offset`: nothing at rest, the depth of the range at full, below it for a negative depth), which is also what a cable from it to a setting of another modulator does, over the knob's travel (`modulated_value`). A macro has nothing in it but its value, so a change is a macro of the new value given to the engine in place of the old (`SetModulatorSource` / `SetGroupModulatorSource`, `controller::macro_command`): no command of its own, and no cross-mod base to keep.
    - *Model, controller, file:* `ModSourceSlot::Macro { value }` (`new_macro`: at rest; its node is `Macro` and the tree says `Macro 0.50`), `ModulatorKind::Macro` ("a macro" in the words of a button), `ModulatorField::Value` (0 to 1, linear; the first row of the pane, `SetModulatorField`; no port: `modulator_row_to_cross_mod` has none, so no cable can end at it), `type = "macro"` with `value` in the session file (held to 0 to 1 on load, always written: `value = 0.0` for one at rest; `nan` is refused like every number), `ModulatorSource::Macro { value }` in the graph (the generated types and constants say `macro` and `value`), `Snapshot::apply`, and the history (a step reads `set value`; a macro that is taken out is put back at the value it had; `History::field_value` is `ModSourceSlot::setting` now, which it repeated; the history fixture has a script of a macro, which the mock engine replays).
    - *Terminal:* `New macro → <parameter>` in the modulate popup after the envelope (the three builders: a parameter, a bus parameter, a setting of a modulator), the pane of a macro (`Macro`: `Value`, the "Targets" separator and the depth of each cable), `Left` / `Right` a twentieth and `Enter` a number, `m` on the value opens nothing, and the Help tab says so.
    - *Client:* the reducers, the selectors, the history, `modulationExtent` (a macro goes from nothing up as a CC does), `settingPorts` (none), the editor's words, and the mock engine (a new macro is at rest; it puts out its value and moves what it drives as the engine does; `FIELDS_OF` says which settings each kind has).
    - *Page:* `MacroValueControl`, a knob with no place for a cable on its edge, no ring, no dot and no `~` button, since nothing drives it (`ModulatorSettingCell` gives it the value; `settingsOf` says `value`); "Macro" in the "+" menu of a lane and of a group and "New macro" in the `~` menu of a knob, between the envelope and the MIDI control.
  - **Found on the way.** The rules that said whether a modulator has a setting were written as what a kind does not have: in the mock engine (`fieldOutcome`) and in the reducers (`withSetting`) "an envelope has every setting that is not a rate or a waveform", so a setting that neither an LFO nor an envelope has would have been taken by an envelope and set a field that it does not have. They list what each kind has now, with tests that an envelope and an LFO refuse a value and a macro refuses the rest. `History::field_value` was a second copy of `ModSourceSlot::setting`, and went to it. In the page `ModulatorFieldControl` took every setting for a port (`{ type: field }` for the ring, the dot and the handle), and `value` has none: it takes the settings that have one, and the macro has a control of its own. One entry more in the terminal's modulate popup moved five tests that count its rows, one snapshot and the page's two tests that cut the list after its three new items. And the tests' own moves pressed keys until the cursor was where they were asked to be: `Harness::goto`, which `on_modulator_row` calls, moves the cursor of the chain with Up and Down and never ended while the focus was in the parameter pane, and `on_modulator_row` never ended for a row that the pane has not; a test of this step never ended and was killed (f153). Both stop with a message after as many presses as the distance can take now, with two tests of their own that the numbers below do not have.
  - **Limits, as built.** No cable drives a macro's value: a MIDI control bound to a macro, which would turn it from a hardware knob (what a macro is for on a stage), and an LFO that sweeps it, wait for a port (a `Port::Value`, a `CrossModField::Value`, `mod_value` in the file, the target in the engine, the rule of reach and the page's handle: Open question 9, and f84's refactoring first). A macro is from 0 to 1 and unipolar: one that goes both ways round the value is a cable of a negative depth, or two. It has no name of its own: two in a lane are `Macro 1` and `Macro 2` on the canvas, and the terminal's tree says their values, which change as they are turned. The terminal makes one only for a parameter (`m` in the pane); in the chain `m` still adds an empty LFO.
  - **Tests.** Rust: 37 new, 1473 to 1510 (1651 to 1688 with the `web` feature; 1468 to 1505 without the default plugin formats; 1638 to 1675 with `web`; on the real program 24, as before): eight of the engine (what it puts out, the law at rest, at full, with a range that is not 1 and with a negative depth, a new value, the rate of an LFO, a group's), five of the model, two of the intents, four of applying a value (a lane's, a group's, held to 0 and 1, and what has no value), three of loading, three of saving, one each of the graph and the snapshot, two of the history and eight of the terminal (the tree, the popup for a parameter and for a bus parameter, the pane's snapshot, `Left` / `Right`, a typed value, `m`, `d` and Ctrl+Z), and a script of the history fixture. Web: 21 new, 1924 to 1945 in 89 files (the mock engine 5, the knob of the value 6, the history 2, the reducers 2, and one each of the selectors, the scale of a node's output, the rings, the editor's words, the node on the canvas and the menu of a knob, whose list test now says the macro); browsers 126 to 129 (three on the real program). Mutants: 31 of Rust (the law and what it puts out in the engine; the model, the command, the file, the graph, the snapshot, the popup and the pane), all caught; 27 of the page (the reducers, the selectors, the history, the mock engine, the rings, the controls and both menus), all caught, two of them after the test was made harder (an envelope that takes a value; the editor's name for a macro in a notice). Looked at in tmux on the real program (a macro in the tree and the pane, turned with `Right`, and the page and the terminal each seeing the other's change).
  - **Numbers at the end of the step.** Rust 1510 tests, 1688 with the `web` feature and 24 on the real program, 1505 without the default plugin formats and 1675 with `web`, and 3 of the shell's in each; web 1945 tests in 89 files; 129 in browsers; 413 commits on the branch before this entry.

- [x] **49. The tempo (an extra, the first of the patterns' parity).** *Done when:* the tempo that patterns are recorded at is the session's, which a session file can say (`bpm`), which the terminal's `b` and the page's field set by the one intent, `SetBpm`, so that it is a step of the history, marks the session changed and is told to every front-end as an event; a pattern that has been recorded keeps the tempo it was recorded at; and on the real program a tempo typed into the page's field is the engine's, a second page's, the saved file's, and is put back by one Undo. *Not done:* the patterns themselves in the page (recording, clearing, their settings: the next steps), a pattern that follows the tempo (Open question 10), a metronome or a tap tempo.
  - **What it was.** The tempo was the controller's (`global_bpm`), worked out from the first recorded pattern when the controller was made; the model, which is what a save writes, had none. Only the terminal set it, with no intent: a browser could not, it was no step of the history, it did not mark the session changed, and the browsers were told by a snapshot that `resync` made. The only way the file kept it was the `bpm` of each pattern, so that a session with no pattern lost the tempo that was set on it, and one whose patterns were at different tempos left the audio thread at the last one's (the loader sent each pattern's) while the program said the first's.
  - **As built, in five commits.**
    - *The tempo is the model's* (`3ca7b73`, a refactoring that changes nothing the program does): `SessionModel::bpm`, set by the loader; the controller, the terminal, the snapshot and the journal read it there; the two tests of where it starts moved to the loader's.
    - *The file says it* (`b8889f8`): a top-level `bpm` (before every table), read by the loader, held to `BPM_MIN` to `BPM_MAX` (20 to 300; a value that is not a number refuses the file, as every number does) and winning over the patterns'; the audio thread is told once, after everything else, if the file says one or has a pattern; a save writes it only when a load would not come to that tempo anyway (`SessionModel::bpm_to_save` against `tempo_of_patterns`, which the loader shares), so that the files of sessions that never set one are as they were. `session::save`, `save_new` and `to_toml` have a parameter for it, and so has the loader's `SessionConfig` (`bpm`).
    - *The bug* (`1866d49`): the popup of `b` wrote the new tempo into every recorded pattern as well, so that a pattern recorded at 120 and saved after a change to 60 was a pattern of 60, which a load counts its beats in: twice as long, the notes where they were. A pattern keeps what it was recorded at.
    - *The intent* (`8b3e4e0`): `Intent::SetBpm { bpm }`, answered with `BpmSet { bpm }` (held to 20 to 300, refused with `NaN is not a tempo` for what is not a number, the model's `bpm` and the audio thread's `SetGlobalBpm`; a tempo that is set again is a revision, as a lock is), a step of the history (`set the tempo`, `Key::Tempo`: a drag is one step), the terminal's `b` asking it, `BPM_RANGE` generated into the page's constants, and the client's side of it: the reducers (`bpmSet`), `Editor.setBpm`, the history's `planOf` and the mock engine, pinned to the controller's by a script of the history fixture (the drag, two tempos held to the range, the undos and redos).
    - *The page* (`f7c2a5f`): `app/Tempo.tsx` in the bar of the session, after Undo and Redo: a `spinbutton` named Tempo and its unit, `120 BPM`. What is typed is asked for with Enter or on leaving the field, and given up with Escape; the arrow keys turn it by one (ten with Shift), each step asked as it is made from what the field shows; it shows at once what another front-end sets and does not take away what is being typed; a tempo beyond the range is asked as it was typed and the field shows what the server held it to.
  - **Found on the way.** The loader's last-wins tempo and the terminal's rewriting of patterns (above). `b` did not mark the session changed (the intent does). The bar's layout test, `saveasbar.spec.ts`, found that the field with a label ("Tempo 120 BPM", 126 pixels) made the bar wrap at 1280 pixels when the field of Save as was open; the label is in the title now and the field with its unit is 78 pixels, so that the bar with the field of Save as open needs 1265 pixels (measured: it had 101 to spare in a window of 1280 before the field, and has 15: the next thing that is put in the bar will break it, and the test will say). A tempo that is not a number cannot be sent from the page (JSON writes NaN as null), so the refusal is tested on the mock server, where `Editor` has no way to ask it, and the field asks nothing for what is not a number. My first test of the bar was written together with its change and not before it; the mutant that takes the field out of the bar is what showed that the test fails without it.
  - **Limits, as built.** The tempo is the next recording's: a pattern that has been recorded keeps its own (Open question 10 asks whether it should follow). A lane that is added later starts at 120 in the audio thread (`PatternPlayer::new`) and is told the tempo when a recording starts, which the terminal's `r` does first and the intents of the next step will; no front-end can start a recording without it. The field has no drag, no tap and no metronome: a count-in clicks when a recording starts, as it did. A tempo of a file's pattern that is out of 20 to 300 gives the session the nearest one and the pattern keeps its own, whatever it says.
  - **Tests.** Rust: 26 new, 1512 to 1538 (1690 to 1716 with the `web` feature, 24 on the real program as before; 1507 to 1533 and 1677 to 1703 without the default plugin formats): 15 of the file (the parser and writer 3, the loader 6, the model 3, the controller's save 2, the save as 1), 1 of the bug and 10 of the intent (the controller 6, the history 1, the terminal 3). Web: 27 new, 1945 to 1972 in 90 files (the client's layer 9, the field 17, the bar 1); browsers 129 to 131 (two on the real program). Mutants: 19 of the file, 12 of the intent (two survived the first run, the words of a refusal and the range that the popup says, and have tests now), 12 of the client's layer and 15 of the page's field and bar, all caught. Looked at on the real program in Chromium.
  - **Numbers at the end of the step.** Rust 1538 tests, 1716 with the `web` feature and 24 on the real program, 1533 without the default plugin formats and 1703 with `web`, and 3 of the shell's in each; web 1972 tests in 90 files; 131 in browsers; 420 commits on the branch before this entry.

- [x] **50. Patterns in the controller (an extra, the second of the patterns' parity).** *Done when:* what a person does to the pattern of a lane (start and stop a recording, clear it, set its length and its three switches) is done by intents that the terminal and a browser ask alike, each told to every front-end as one event, with the pattern on the lane's node in every snapshot; the terminal keeps no copy of that logic; and on the real program a browser that records a pattern has it in a second browser, in the saved file and in a program started on that file. *Not done:* the page's controls for it (the next step), undo of what is done to a pattern (Open question 11), the notes themselves in the page (a snapshot has how many there are), a recording that ends by itself as an event of its own (it comes as a snapshot: a TODO that was there).
  - **What it was.** The pattern of a lane was the terminal's alone. `r`, `d` and `Ctrl+R` on a pattern, the rows of the pattern pane and a length typed into it each wrote into the model (`PatternState`), sent the audio thread its commands, marked the session changed and rebuilt the tree, in the terminal's own code, four times over (`handle_key`, `clear_pattern`, `adjust_pattern_param`, `set_param_value`). A browser could do none of it, and the graph that a browser is given had no pattern in it, so a pattern that a file had or that the terminal had recorded was not to be seen there.
  - **As built, in two commits.**
    - *The bug that the first test found* (`5a1c231`): the length that is set on a pattern changed only the next recording. `SetPatternLength` set the player's length, but the length in frames of the pattern that was recorded stayed what it was, so that setting the length of a pattern in the terminal changed nothing that was heard until the session was saved and loaded (the loader sent the length after the pattern). `PatternPlayer::set_length_beats` changes the length in frames of the pattern that is there in proportion (its notes stay where they are, and those that the loop no longer reaches wait for it to be long again), and the loader sends the length before the pattern (f157).
    - *The intents* (`a908bf6`): `SetRecording { node, recording }`, `ClearPattern { node }` and `SetPatternField { node, field, value }` (`length`, 1 to 32 beats, or one of the switches `enabled`, `looping` and `transpose`, 0 or 1, rounded), all answered by `PatternSet { node, pattern }` with the whole of what the lane has afterwards, a `PatternInfo` (recording, enabled, looping, in key, length in beats, the tempo it was recorded at, how many notes it has) or null; the same `PatternInfo` is the lane's `Node.pattern` in the graph (`SessionModel::graph` reads it from `PatternState::info`). A recording that is started does what `r` did, in its order (an empty pattern for a lane that has none; the tempo, then the length, then the recording, to the audio thread) and one that is stopped is stopped at once; a start while it is going and a stop while it is not are told as they are and send nothing, since a second start would begin the count-in again. The setting that is given to a lane that has no pattern gives it an empty one, so that the length of its first recording can be chosen; a number that is not one is refused before the node is looked at, and a node that is no lane is refused in the same words by all three (`a pattern belongs to a lane`). None is a step of the history (`Plan::Nothing`): undo goes back past them and what is done to a pattern forgets nothing. The terminal asks them (`r`, `d` and `Ctrl+R`, the pane's rows, a typed length) and rebuilds its tree on `PatternSet`; what it wrote itself is gone. The client has the reducer (`patternSet`: the lane's `pattern` is replaced, a node that is no lane is refused), `Editor.setRecording`, `clearPattern` and `setPatternField` (in the words of each, `start recording a pattern on Oscillator`, `make the pattern of Oscillator loop`), the history's planner (`nothing`) and the mock engine, which does what the controller does (an empty pattern at the session's tempo, held to `PATTERN_FIELD_RANGES`, a switch rounded, the controller's words) and is pinned to it by a fixture the controller writes (`patterns.json`: 302 scripts of intents, each kind asked of each node of a session that has a group, two effects, two modulators, a lane with a pattern recorded at 90 and an empty lane, the master and a node that is not there, with the values that are held, rounded and taken as they are, and runs that go on from one intent to the next: a recording that is started twice, a pattern that is cleared twice) and by a script of the history fixture. On the real program (`tests/serve.rs`) a browser sets the tempo to 300 and the length to a beat, starts a recording, plays a note in the count-in (which is recorded at the start of the pattern) and the second browser is told each of them as an event; when the audio thread is done both are given the pattern as a snapshot, a save has it (`[instrument.pattern]`, with `base_note = "E4"` and the note on and off) and a program started on that file has it in the same lane; a setting and a clear are in the file the next save, and a second clear is refused in the controller's words.
  - **Found on the way.** The length bug (above, f157). My test of the pane's rows could not tell row 1 from row 2: both switches were on, so that a mutant that turned the other's switch looked right (the test turns them one after the other now). The editor's first test of the refusals read the notices at the end and found three: the page keeps the last three (`MAX_NOTICES`), so the tests read each as it is said. The saved file has no top-level `bpm` when a pattern says it (step 49's rule: a load comes to that tempo anyway), which my first test of the real program expected; the test says the rule now. A test of my own had a variable named for something that it was not, and `clippy` found a `map_or(true, …)` in another: both tidied.
  - **Limits, as built.** A snapshot has how many notes a pattern has and not the notes: a page that draws them would ask for them with a message of its own. A recording that ends by itself (its length run out) is the audio thread's report, which the controller folds in when it polls and the browsers hear of as a snapshot, so a page learns that `recording` is over a little late (the controller reads the audio thread's reports when it polls), and cannot tell the count-in from the recording: the flag covers both. A browser can start a recording on a session that nobody plays: it ends with an empty pattern after its length, as the terminal's `r` does. `Clear` is for good (Open question 11).
  - **Tests.** Rust: 21 new, 1542 to 1563 (1720 to 1742 with the `web` feature, which has the test of the constant's range too, and 25 on the real program, 24 before; 1537 to 1558 and 1707 to 1729 without the default plugin formats): 16 of the controller (a recording that starts on a lane with and without a pattern, that is started or stopped when it need not be, that is stopped and then comes from the audio thread; the refusals for a node that is not there, the master and each of the five other kinds of node a session has; a clear, a clear of a pattern that is being recorded and one that is refused; the settings by their commands, their range and rounding, a number that is not one and the order it is judged in, a lane that has none given an empty one; no step of the history; and the fixture the client's tests read), one of the graph and four of the terminal (`r`, a clear and Ctrl+Z, the pane's rows, a typed length), and a script of the history fixture. Web: 14 new, 1972 to 1986 in 91 files (the reducers 5, the editor 5, and the mock engine 4 in a file of its own, `patterns.test.ts`); browsers 131, as before. Mutants: 46 of Rust (the controller's three intents and the order of their commands, the ranges, the model's defaults and what it tells, the graph, the snapshot, the history and the terminal's rows) and 40 of the client (the reducer, the editor's words and intents, the history's planner and the mock engine), all caught; one of each survived the first run and has a test now (the pane's rows 1 and 2; a lane that the mock adds has no pattern).
  - **Numbers at the end of the step.** Rust 1563 tests, 1742 with the `web` feature and 25 on the real program, 1558 without the default plugin formats and 1729 with `web`, and 3 of the shell's in each; web 1986 tests in 91 files; 131 in browsers; 423 commits on the branch before this entry.

- [x] **51. The pattern in the page (an extra, the third of the patterns' parity).** *Done when:* every lane of the page has the pattern's controls: a button that records one, a menu that clears it (asking first: it is for good), its
  length in beats, its three switches (play, loop, in key) and a line that says what it is; what is done there is the session's (a second page, the terminal and the saved file agree); a recording that goes on is seen to end in every page; and
  on the real program a pattern recorded from the page is in a second page and in the file, and is cleared from either. *Not done:* the notes of a pattern drawn (a snapshot has how many there are), where a recording is (counting in or recording, and
  how far along: the engine reports only its end), undo of what is done to a pattern (Open question 11), a pattern that follows the tempo (Open question 10).
  - **What it was.** The page had the tempo (step 49) and the controller had the pattern's intents (step 50), and the nodes of the page's session had each lane's pattern, which nothing drew: a pattern that a file had or that the terminal had recorded
    was not to be seen, and none could be made, cleared or set from the page. The mock engine had the intents and not what the engine does with a recording, so a page that was built on it could start one and never see it end.
  - **As built, in five commits.**
    - *The tempo's field is a control* (`3b8667f`, a refactoring: the page and its tests are as they were): the field of the tempo (a spinbutton that asks for what is typed, gives it up with Escape, turns by a step with the arrows, and shows what it
      asked until the answer comes) is `ui/NumberField`, which knows no session; `app/Tempo.tsx` keeps the label, the unit and `BPM_RANGE`. The tempo's 17 tests and the bar's are unchanged; the field has 17 of its own, with a stub that is asked.
    - *The count-in has a name* (`e41cead`): `COUNT_IN_BEATS` in the engine (it was a 4.0 in the player's constructor), told to the page as `PATTERN_COUNT_IN_BEATS`; an engine test counts the count-in to the frame (a beat is 24000 frames at 120
      and 48 kHz) and then the length of the pattern.
    - *The mock engine's recordings* (`3a8c14c`): a recording that is started is counted in for those beats and then takes the length of the pattern, at the tempo it began with; the notes that are played in the keys of the lane are counted, the
      count-in's too (the engine records them at the start of the pattern); one that is stopped in the count-in has recorded nothing and one that is stopped after it keeps what was played; a pattern that is cleared ends it; when one is done the
      lane has the pattern (its settings and length, heard if it has notes, at the session's tempo then) and the clients are given the session at a revision of its own, marked as changed, which is what the controller does when it polls
      (`finish_recording`, `Controller::resync`). A timer looks at them every 50 ms while there are any, and every message and intent settles what has run out first (`MockServer.settle`; the tests give the server a clock of their own).
    - *The length of a new pattern has a name* (`8c50479`): `PATTERN_DEFAULT_LENGTH`, from `PatternState::empty`, which the page shows on a lane that has no pattern and the mock engine gives a pattern it makes.
    - *The block* (`55c8146`): `PatternCell`, `patternOf`, `client/patternWords.ts`, the node's height (`PATTERN_HEIGHT`), the styles, and the browser tests (see §9.3, "As built (step 51)").
  - **Found on the way.** Eight mutants of the mock's recordings survived the first run: five were what the tests had not said (the tempo a recording is counted in at, a stop that is not let go of, the count-in at the tempo of a stop after a change of
    tempo, a session that is marked as changed when a recording is done, the connections that are told) and have tests; three were branches that the guard of `settle` makes redundant (the entry of a cleared pattern, a pattern that is not recording, which of
    two changes is told) and went. The block's first measurement was 98 px in one state of the button and 100 in the other, from the font of the glyphs: rows have heights of their own now
    (97 in both). A lane is 477 px tall now (it was 380), so four tests of the layout moved with it: one asked a row to be moved below a taller effect and the lane next to it had caught up (the effect grows more), two of the reveal tests had a canvas that looked
    at a lane that was no longer all in the view (it looks lower), and the test of a scope that is in the way of the next group had its premise by the numbers of a lane that was shorter than the scope (the layout from nothing keeps a row's gap below the scope,
    a canvas that remembers moves only what is over something: there are two tests of it now, a group with nothing in it and a group with a lane: in the second the scope reaches 4 px past the row, which the test asserts, so that a change of the block's height that
    breaks the premise says so). A browser test of the instruments asked whether a lane was all in the window by `toBeInViewport({ ratio: 1 })`, which Chromium answers with a 32-bit number, 0.99999994, when the canvas is zoomed to a fraction (the taller nodes made it one):
    it asks for the lane's box now. The page's `Notices` keeps three (step 50's finding), which the tests of this step read as they are said.
  - **Limits, as built.** The page says that a recording goes on and not whether it is counting in or recording, nor how far along it is: the engine reports its end and nothing before (a TODO of `CLAUDE.md`). The notes of a pattern are not drawn. A recording that
    a page starts records what is played by anyone (another page, the terminal, a MIDI device) in the keys of the lane, and the keys of the computer are the piano's in a page, except in a field that takes text. The clear is for good
    (Open question 11), which the words of its menu say. The record button does not ask first for a lane that has a pattern: a recording replaces the notes when it ends (the terminal's `r` does the same). The mock engine counts note-ons only: a note-off that
    is recorded without its note-on (a key held through the count-in) would make the engine's pattern heard with no notes, and the mock's not.
  - **Tests.** Rust: 1 new, 1563 to 1564 (1742 to 1743 with the `web` feature, 25 on the real program as before; 1558 to 1559 and 1729 to 1730 without the default plugin formats): the engine's count-in. Web: 1986 to 2054 in 95 files (68 new: the number field 17, the mock's
    recordings 20, the cell 18, the words of a pattern 7, the selector 4 and the layout 2): browsers 131 to 134 (three new, on the real program and the mock: the block's height, a pattern recorded from the page, a recording stopped in its count-in); and
    the one that moved. Mutants: 4 of the engine's count-in, 36 of the number field (35 caught and one equivalent), 49 of the mock's recordings (the survivors above), 47 of the cell, the words, the selector, the node and the layout (five survived the first run and have tests,
    or went) and 5 on the browser, all caught. Looked at in a browser (the mock engine, the real program's page): the block idle and recording, and the canvas of two lanes.
  - **Numbers at the end of the step.** Rust 1564 tests, 1743 with the `web` feature and 25 on the real program, 1559 without the default plugin formats and 1730 with `web`, and 3 of the shell's in each; web 2054 tests in 95 files; 134 in browsers; 429 commits on the branch before this entry.

- [x] **52. Velocity and key (an extra: sources that follow the notes a lane plays).** *Done when:* a lane (or a group) can be given a control that follows the velocity of the notes it plays and one that follows their pitch, as a MIDI control follows a controller: from a
  session file, from the terminal's `m` and from the page's menus (a lane's and a group's "+", and the "~" of a knob), with a cable at half depth that moves what it drives by the laws below; the saved file has each bound; they are steps of the history in words of their own; and
  on the real program a key of the computer that is played in a page moves the knob that a velocity or a key drives, to where the law puts it, and a key of the page's piano plays harder lower down. *Not done:* a note's velocity or pitch per voice (a chord has the last note's: a
  patch's voices would have each their own, `docs/track-b.md`), a centre or a span of a key that a person can set (it is middle C and five octaves, and the depth of the cable scales it), the remap and the patterns in the mock engine (the engine's controls hear the notes after both), a velocity from the
  keys of the computer other than 100.
  - **What it was.** The sources of modulation were an LFO, an envelope, a macro and a MIDI control that a controller moves (a CC, the wheel, key pressure). Nothing followed what was played: a filter that opens for a hard note, or a level or a pitch that follows the key, was
    not something a cable could do, whatever the keyboard sent, and the page's keys had one velocity.
  - **The laws** (`CLAUDE.md`, "MIDI-learn modulator"). A velocity is the velocity of the last note-on the lane plays over 127, unipolar as a CC (`base + velocity * depth * range`), at rest (0) before a note, and it keeps its value when the note ends (a release goes by its own note's
    velocity); a note-on with no velocity is a note-off, and a note-off that has a velocity (64 on many keyboards) is not a note. A key is the number of the last note over 127 and is bipolar around middle C: `base + clamp((note - 60) / 60, -1, 1) * depth * range`, so that note 60
    adds nothing and five octaves either way are the whole of the depth; it rests at note 60. The notes they hear are the ones the lane's instrument is given (after its range, its remap and its transpose, and a pattern's own notes); a group's hear the notes of its members' ranges, as they are played.
  - **As built, in six commits.**
    - *A pitch-bend control rests at its centre* (`81fc226`, a bug found while planning: f159): the model gave the audio thread every bound control with a value of 0.0 to start from, and the wheel's law is `(value - 0.5) * 2`, so a wheel that a file had bound, or that undo
      had put back, drove what it was cabled to at the bottom of the wheel, minus the whole of its depth, until it was first moved (a wheel sends nothing at rest). `MidiBindSource::rest` says where each control starts; the page's mock engine had always rested the wheel in the
      middle, so the page and the engine disagreed, and the loader's test had pinned the 0.0.
    - *The sources* (`7b4eb92`): `MidiBindSource::Velocity` and `Key` in the engine (`follow` takes the value of a control out of a MIDI message; `note_on_of`), the file's `midi_velocity` and `midi_key`, the protocol's `{ type: "velocity" }` and `{ type: "key" }`, the tree's `Velocity → …` and `Key → …`, the page's words,
      rings and generated `KEY_CENTRE` and `KEY_SPAN`, and the mock's rest of a key at middle C.
    - *The modulate popup's new modulators are a table* (`b56062b`, a refactoring): the terminal's `m` popup listed its first three choices three times over (a lane's, a modulator's own setting's and a group bus's); `NEW_MODULATORS` and `new_modulator_choices` are one.
    - *Making them* (`5b9d583`): `ModulatorKind::Velocity` and `Key` (`ModulatorKind::of` tells a node's kind back from its source, for the words of a removal), `AddModulator` and `Modulate`, the popup's two entries in its three places, the page's "+" and "~" menus, `client/modulatorKinds.ts` (the words and the kind of a
      source, of which `editor.ts` and `history.ts` each had a copy), the mock engine's following of the notes (`MockServer.#listened`: a lane's hears the notes in its keys as its transpose makes them, a group's the notes of its members' keys as they are played), two scripts of the history fixture that pin the mock to the controller, and a browser spec on the real program.
    - *The page's piano plays harder lower on a key* (`af13905`): a pointer played every note at 100; it goes by where the pointer is on the key now, 20 at the top edge to 127 at the front (`velocityAt`; a key that has no height to tell a place on is played at 100, which is what jsdom's are).
    - *The docs* (this entry, D52 and `CLAUDE.md`).
  - **Found on the way.** F159, above. Two of the 21 mutants of the engine's first run lived: a note-off with a release velocity counted as a note (a test has one now), and a rounding of the note number that was never needed (127 times the 32-bit n/127 is n for all 128 notes: the rounding went, and a test goes through every note). The whole browser suite found
    two tests of the knob's menu that listed its items (three lists: the menu has six new ones now), and a lane's menu that gave a key for a velocity lived in the page's mutants until a test chose each item.
  - **Limits, as built.** A velocity or a key is lane-global and block-rate as every modulator is: a chord has its last note's, and two notes in one buffer give the later. They are not learned (no controller sends them as a control) and have no setting but the depth of a cable. The mock engine has no remap and no patterns, which the engine's controls hear the notes after, and its notes
    are the page's (`noteOn` messages). The keys of the computer still play at 100 (they report no velocity). A velocity from a MIDI keyboard goes through as it is sent.
  - **Tests.** Rust: 1564 to 1582 (1743 to 1761 with the `web` feature, 25 on the real program as before; 1559 to 1577 and 1730 to 1748 without the default plugin formats): f159's two, the engine's eight (what each publishes, the laws, a lane's range and transpose, a control that is still learning, a group's, every note exactly), the file's one, the terminal's five (two for the tree and the saving, three for the
    popups), the controller's two (the kinds, and their words in and out); and what the loaders' and the model's tests say, with the MIDI scene of the tests (now with both controls) that every scene test goes through. Web: 2054 to 2090 in 95 to 96 files; browsers 134 to 138 (four new, on the real program: a velocity, a key, the lane's menu and its history, the page's piano; and three lists of the knob's menu that moved). Mutants: 5 of f159's fix; 21 + 3 of the engine and the file
    (the two that lived above, then all caught); 8 of the controller and the terminal; 35 of the page (7, 16 and 12, in the commits that changed it); and 4 against the browser specs (2 of the program, 2 of the page): all caught in the end.
  - **Numbers at the end of the step.** Rust 1582 tests, 1761 with the `web` feature and 25 on the real program, 1577 without the default plugin formats and 1748 with `web`, and 3 of the shell's in each; web 2090 tests in 96 files; 138 in browsers; 435 commits on the branch before this entry.

- [x] **53. Mute and solo (an extra: silence a lane or a group, or hear only one).** *Done when:* a lane or a group can be muted and soloed from a session file (`mute`, `solo`), from the terminal (`M` and `S` on the chain) and from the page (a cell on its node);
  a muted one is never heard, and while anything is soloed only what is soloed is, by the laws below; the notes, the modulators, the patterns and the tails of the effects go on, so that a key that is held is heard again when the node is brought back; the gain
  goes over one buffer, so that it does not click; a press is a step of the history (`mute the lane of Oscillator`); the saved file has the flags; the page draws what cannot be heard dimmed; and on the real program the master's meter is silent for a lane
  that is muted and for a lane that is not soloed while another is, and the sound is back with the lane, a held key included. *Not done:* a click that solos one and takes the others' solo off, and a button that takes every solo off; a muted lane that stops
  rendering (its plugins' CPU); a mute or a bypass of an effect and of the master; a fade longer than a buffer.
  - **What it was.** The only way to hear one part of a session was to turn the volumes of the others down, which loses the numbers a person had set, or to take a node out. A volume is the user's number and a mute is a state, so it is a flag of its own.
  - **The laws** (`CLAUDE.md`, "Mute and solo"). A muted lane or group is never heard, and a mute wins over a solo (a lane that is muted and soloed is silent, and being soloed it silences what is not soloed all the same). While anything, a lane or a group, is soloed,
    only what is soloed is heard: a soloed lane, a lane in a soloed group, and a group that is soloed or has a soloed lane in it (its bus is heard for that lane, with its volume and effects, and its other members are not). A lane's mute is its own: a lane in a
    muted group is not muted itself (its meter reads) and the bus is cut. The audio thread lets a lane go, or brings it back, after its volume and its effects and before its meter, and a bus after its effects, by a linear ramp of its gain over the buffer (`fade`,
    5.3 ms at 256 frames); a gain that is 1 and stays so costs nothing, and one that is 0 and stays so puts zeros and multiplies by none, so that a plugin that puts out `NaN` is silenced by a mute. The scope taps of a node are before it (they show the signal of the
    node), the meters after.
  - **As built, in five commits.**
    - *The engine and the file* (`b832da3`): `GraphCommand::SetMute`, `SetSolo`, `SetGroupMute` and `SetGroupSolo`; the flags of a lane and of a group in the audio thread, with the gain they were heard at when the last buffer ended (`heard_at`); `fade`; the file's
      `mute` and `solo`, written only when on; the model's flags, the loader that sends what a file turns on and the saver; 18 tests.
    - *The intents, the history and the client layer* (`a40ee03`): `Intent::SetMute` and `SetSolo`, `Event::MuteSet` and `SoloSet`, the controller's `set_mute` and `set_solo` (refused for a node that is neither a lane nor a group), a step of the history for each (the words above, no
      key), the snapshot's and the graph's flags (`NodeKind::Instrument` and `Group` have `muted` and `solo`), the put-back of a lane and of a group, which carry them, the regenerated types and fixtures, the page's reducers, `Editor.setMute` and `setSolo`, and the mock
      engine's values and history.
    - *The terminal* (`4ca9667`): `M` and `S` on the Session tab's chain (the lane or the group the selected entry is of: a group's header, bus effect and modulator are the group's), a `[mute]` and a `[solo]` after the name in the tree, the two keys in the action bar of an
      instrument and of a group (their neighbours' labels shortened to fit 120 columns), the Help text, and the golden snapshots.
    - *The page* (`9af40cc`, and `2f96c5e`, a comment that said a file can have a rate below 0): the cell, the dimming, `audibleOf` and its fixture from the engine, the mock engine's meters and scope, `controlCount`, and the browser spec on the real program (see §9.3, "As built (step 53)").
    - *The docs* (this entry, D53 and `CLAUDE.md`).
  - **Found on the way.** The master's meter falls slowly, so a browser test that looks for silence has to wait for the master to be quiet before it: the first draft of the spec read -15 dB for a note of a lane that was not soloed, most likely the meter still falling from
    the note before (the spec's `heard` and `silent` wait for the quiet, and it passed 12 runs of 12 after that, and the mutants that break the engine's law are caught by it). The canvas moves to a lane that is added, and the buttons of the first lane were then under the bar (the spec shows all with the canvas's "Fit View" first). A brightening of a dimmed node under the
    pointer was meant to show that it is as ready as it was, and was taken out before the commit: a press of its mute is under the pointer, and the dimming would not have been seen. Two tests of the layout's arithmetic moved with the new cell (a lane's first eight
    parameters and its controls are twelve, three rows of four, and the ninth parameter is what begins a fourth: the pin test says so); and one of the 45 mutants of the page lived, the solo's title when it is on, until a test looked at it.
  - **Limits, as built.** A mute is a gain at the end of the lane or of the bus: the node renders as it did (its plugins' CPU is the same), which is what lets a held note and a tail be where they would be. A solo is additive; there is no exclusive solo. The page dims
    what cannot be heard but does not say why: a lane that is dimmed for a solo elsewhere has both of its buttons off, and one that is heard for the solo of its group has its own off too. The effect of a lane is dimmed with the lane, and a bus effect with the group.
    The mock engine has no fade, and its meters are its own numbers (a lane that is heard puts out what the mock puts out).
  - **Tests.** Rust: 1582 to 1621 (1761 to 1800 with the `web` feature, 25 on the real program as before; 1577 to 1616 and 1748 to 1787 without the default plugin formats): 5 of the two fixes that came first (f149's three and f147's two), 18 of the engine and the file, 9 of the intents
    and the history, 6 of the terminal and 1 that pins the page's `audibleOf` to the engine. Web: 2090 to 2141 in 96 to 98 files (19 in the second commit, 32 in the fourth); browsers 138 to 141 (three new, on the real program: a lane's mute and solo heard at the master and saved, the history of a
    press seen by a second page, and a group's mute). Mutants: two runs of 21 and 27 in the first commit, 30 of the controller and 27 of the client layer in the second, 23 in the third, and in the fourth 45 of the page, 7 of the engine's law against the fixture and 8 against the
    browser spec (4 of the page and 4 of the program); all caught in the end.
  - **Numbers at the end of the step.** Rust 1621 tests, 1800 with the `web` feature and 25 on the real program, 1616 without the default plugin formats and 1787 with `web`, and 3 of the shell's in each; web 2141 tests in 98 files; 141 in browsers; 444 commits on the branch before this entry.

- [x] **54. A delay (an extra: a built-in echo to build with).** *Done when:* `builtin:delay` is a built-in effect that every front-end can add from its catalog (the terminal's selector, the page's "fx" menu, a session file) and modulate like any other (a cable to `time`, `feedback`, `tone`, `pingpong`), with four
  presets; it puts out its echoes only, one `time` after the sound; a change of the time is a sweep in the pitch of the echoes and never a click; the catalog's fixtures and the selector's snapshot say it; there is an example session; and on the real program a delay put in from the page puts nothing out until its time has gone by
  (on the scope of its node, with the mix all the way up), and a preset moves the time. *Not done:* a time that follows the session's tempo, a modulation of its own (a chorus), a time on a scale that makes a small modulation easy to set, and the other effects that a rack wants (a chorus, a drive).
  - **What it was.** The built-ins were an oscillator, a filter and a reverb, so that an echo was something to look for among the plugins of the machine. A delay is the first thing a person who builds a sound wants after a filter, and the first parameter that wants a modulator (a `time` that wobbles is a tape echo).
  - **The delay** (`CLAUDE.md`, "Built-in plugins"). Two rings (the left side and the right), read at a delay that follows `time`; what is written is what is put in and the feedback, soft-clipped. `time` is in seconds, 0.001 to 2; `feedback` 0 to 0.95; `tone` 0 to 1 is a one-pole low-pass in the feedback only, exponential from 500 Hz to
    20 kHz and none from 0.999 up, so that the first echo is the sound as it was put in and each repeat is darker; `pingpong` 0 to 1 crosses the feedback to the other side and moves the sound that is put in to the left as the sum of the two sides (a mono sound starts on the left and goes from side to side). The delay follows `time`
    by a one-pole of 10 ms and no faster than half a sample of delay for each sample that goes by, so that a jump is a sweep in the pitch of the echoes, within an octave down and a fifth up, as on a tape echo; `feedback` and `pingpong` follow over 5 ms. A sound that is not a number is not put in (it would stay in the echoes
    for ever), and nothing is allocated while it runs. Presets: Slapback, Tape Echo, Ping-Pong, Dub Echo.
  - **As built, in four commits.**
    - *The soft clip is shared* (`2eb7676`, a refactoring): the reverb's `soft_clip` (and its test) moved to the module of the built-ins, since the delay needs the same bound on a feedback.
    - *The delay* (`4a11d49`): the plugin and its 15 tests, the registry, the catalog's fixtures (`builtins.catalog.json`, `.effects.json`, `.presets.json`), the effect selector's snapshot, the three tests of the page and the browser test that list the effects, `examples/echo.toml` (a saw through a filter into the ping-pong preset, with an LFO each on the cutoff and
      on the time, whose load is pinned), and a browser spec on the real program.
    - *The tree's depths* (`3a45877`, a fix found by looking at the example in the terminal): a depth was written as a whole percentage, so that 0.4% of a delay's time read (0%); a depth under 10% that is not whole has its decimal now.
    - *The docs* (this entry, D54 and `CLAUDE.md`).
  - **Found on the way.** A one-pole in 32 bits stops short of what it follows by as much as the steps of its number are, over its coefficient: the first version's delay sat at 4,800.117 samples for a time of 0.1 s, and the echo of an impulse was split over two samples (0.44 and 0.06); a test saw it, and the delay's place is a 64-bit number
    now (the feedback's and the ping-pong's smoothings follow numbers between 0 and 1 over 5 ms, where the same stop is under a hundred-thousandth, and are left). The scope of an effect's node is of what comes out of the node, which is the delay blended with the sound that went in by the mix: the first draft of the browser test read the dry sound at once from a delay at half mix, and the test puts the
    mix all the way up. A change of the feedback or the ping-pong that was tested at a zero of the sound did not show whether it was gone through (a step in a gain is a step in the echoes by as much as the sound is then): the test makes it where the sound is over 0.3. Five of the 35 mutants of the delay lived the first time (the tone of the right side, the tone's state across a preset, and the smoothing of the feedback, of the ping-pong and how
    fast it is), and two test changes that were meant to kill them were lost for a run because the script that made them did not write its file: the mutants said so. The whole browser suite found a list of the built-ins in `server.spec.ts`. The tree wrote a small depth as 0% (above).
  - **Limits, as built.** A modulator's depth is a fraction of the range, and the range of `time` is two seconds: the smallest step of the page's slider and of the terminal's keys, 0.01, is 20 ms of delay, so a subtle wobble (a few milliseconds) is set in the file (`examples/echo.toml` has one, at 0.004), and a cable made from the page starts at 0.5, a second either way, which is a wild sweep to be turned down.
    A log scale does not change that much (±5 ms at a time of 0.375 s is a depth of 0.0018 on the log scale from a thousandth of a second to two, and of 0.0025 on the linear), and a number that is not seconds says nothing on the knob; a wobble of its own is the answer (TODO in `CLAUDE.md`). The time is a number of seconds and does not follow the tempo. The echoes are stereo, as the chains are.
  - **Tests.** Rust: 1621 to 1639 (1800 to 1818 with the `web` feature, 25 on the real program as before; 1616 to 1634 and 1787 to 1805 without the default plugin formats): 15 of the delay, one of its parameters' metadata, one of the example's load and one of the tree's depths. Web: 2141 tests in 98 files as before (three of them changed); browsers 141 to 142 (one, on the real program: the delay's first echo on the scope of its node,
    after the time of a preset, and the knobs' values after a preset). Mutants: 35 of the delay (five of them lived the first time), 3 of the program against the browser spec and 8 of the tree's depths (one lived until a depth that rounds up was in the test); all caught in the end.
  - **Numbers at the end of the step.** Rust 1639 tests, 1818 with the `web` feature and 25 on the real program, 1634 without the default plugin formats and 1805 with `web`, and 3 of the shell's in each; web 2141 tests in 98 files; 142 in browsers; 448 commits on the branch before this entry.

- [x] **55. The delay's own wobble (an extra, the second of the delay).** *Done when:* the delay has a modulation of its own, `mod_rate` (0.05 to 10 Hz) and `mod_depth` (0 to 10 ms, 0 is off), in a section of its own, that wobbles the time of the echoes (a tape echo's, a chorus's), which the page and the terminal show like any parameters and a modulator can move; a session that has a delay and says
  nothing of it sounds as it did; and the presets say what the parameters are for. *Not done:* a time that follows the tempo, a waveform or a random wobble, a modulation that is a fraction of the time and not milliseconds, a finer step for a depth.
  - **What it was.** Step 54 found that a cable from an LFO to the `time` of a delay has a range of two seconds to move over, so that the few milliseconds that a wobble is are a depth of 0.001 to 0.005, which the page's slider and the terminal's keys (steps of 0.01, twenty milliseconds of delay) cannot set and a cable made from the page (0.5, a second either way) starts a long way from. A parameter of its
    own has the milliseconds, and a modulator can still move it (a `mod_depth` of 10 ms is a range a slider sets).
  - **The wobble** (`CLAUDE.md`, "Built-in plugins"). A sine moves the time of the left side and a cosine the right side's, a quarter of a cycle apart (so that the two sides are not the same echo, which is what makes a chorus wide), by `mod_depth` milliseconds each way, at `mod_rate` hertz. A change of the depth is gone through over 50 ms, since a depth that stepped at the top of the cycle would step the delay by up to 480
    samples; a change of the rate changes how fast the cycle goes round, and the place in it stays, so that it does not step. Tape Echo has 1.5 ms at 0.6 Hz and Dub Echo 0.8 ms at 0.3 Hz; the default is off.
  - **As built, in two commits.** *The wobble* (`132748a`): the two parameters, their section, the presets' values, the modulation in `process`, the catalog's fixtures and the effect selector's snapshot (the delay has 6 parameters), the browser spec's knob values after each preset, and `CLAUDE.md`; *the docs* (this entry and D55).
  - **Found on the way.** A test of where the echo of an impulse falls, with a delay that is moving, sums to 0.5013 over its two frames and not 0.5: it is read at a speed a little off one (the delay changes by 0.0025 of a sample for each that goes by at 0.05 Hz and 8 ms), which the tolerance now says. Nothing else: the 13 mutants were all caught by the first tests.
  - **Limits, as built.** The wobble is a sine, in milliseconds (not a fraction of the time: a chorus wants milliseconds and a tape echo's wobble is a speed, which is a fraction of the time, and one is not the other), and has no waveform, no random and no sync to the tempo. It costs a sine and a cosine for each sample (in 64 bits), whether it is on or not.
  - **Tests.** Rust: 1639 to 1642 (1818 to 1821 with the `web` feature, 25 on the real program as before; 1634 to 1637 and 1805 to 1808 without the default plugin formats): three of the wobble (where the two sides' echoes fall, how far the pitch is swept, and a change of the depth that does not click and gets there) and the metadata of the delay's parameters, which grew. Web: 2141 tests in 98 files as before; browsers 142 as before. Mutants: 13, all caught.
  - **Numbers at the end of the step.** Rust 1642 tests, 1821 with the `web` feature and 25 on the real program, 1637 without the default plugin formats and 1808 with `web`, and 3 of the shell's in each; web 2141 tests in 98 files; 142 in browsers; 450 commits on the branch before this entry.

- [x] **56. A unison and a pulse width for the oscillator (an extra: a fatter sound to build with).** *Done when:* the oscillator has a unison of 1 to 7 oscillators for each note with a spread and a width, and a pulse width for its square, as parameters that every front-end shows (the terminal's pane, the page's panel, a session file by name) and a modulator can move; they
  start at what changes nothing, so that a session from before them sounds as it did (a test holds a chord of each waveform to what it rendered before them); the width does not change how loud a channel is; and the fixtures, the snapshots and the tests that counted the oscillator's eight parameters say twelve. *Not done:* band-limiting the waveforms (they alias at high pitches, a
  narrow pulse more), a unison that is changed under a note without a click, the new parameters among the knobs a node shows by default, a random or a chosen start for the oscillators' phases.
  - **What it was.** The oscillator was a test tone with an envelope: one oscillator for each note, and a square that is always a half. A synth sound that is built from it needs the two things that make a plain oscillator thick and alive: several detuned copies of it, spread across the speakers (a unison, the sound of a supersaw), and a width of the pulse that a modulator can move (pulse-width modulation).
  - **The parameters** (`CLAUDE.md`, "Built-in plugins"). After `pan`, so that the first eight (the knobs of a node: the page's rule of the first eight) are the ones they were and no index moves: `unison` (a choice named by how many, 1 to 7; the value is the place in the list), `spread` (0 to 1 semitone, default 0.15), `width` (0 to 1, default 0.5), `pulse` (0.05 to 0.95, default 0.5). The oscillators of a unison are
    spread evenly from the lowest, on the left, to the highest, on the right; each is that many `spread`s from the note's pitch and that many `width`s across by a law of equal power, and the sum is brought down by the root of how many there are. A pulse that is not a half has its offset taken out and is scaled so that its higher side is at most 1. A unison of one, and a single channel, have no spread and no width.
  - **As built, in two commits.** *The parameters* (`e40ae64`): the voice's phases, the unison in `process`, the four parameters, the pinned chord and twelve other tests, the registry's metadata, the nine snapshots of the terminal, the ten fixtures (the oscillator's parameters are in most of them) and the eleven tests of the page and the terminal that counted eight, and `CLAUDE.md`; *the docs* (this entry and D56).
  - **Found on the way.** A balance (what the oscillator's `pan` is) took the voices at the side out of the other channel: five voices at half width were 0.83 of the level of one in the left channel, which the first level test saw; the width is a law of equal power now. The voices are in a hash map that is walked in an order that changes from run to run, so that the sum of a chord differs by a rounding from one run to the next
    (found when the digest of what the oscillator renders did not come out the same three times): the test compares to a millionth, and `CLAUDE.md` has a TODO for a fixed array. Two of the 22 mutants lived (the side of the lowest voice, and a seventh voice that was cut) until a test of each. Clippy found a float literal with more digits than its type keeps (machine-fixed) and a type of six fields.
  - **Limits, as built.** The waveforms are not band-limited, and a narrow pulse aliases more. The four parameters that are new to a node are in its panel and not on it (pin them, `unison` and `pulse` for a start): the node shows the first eight. A change of the unison while a note sounds adds or takes away an oscillator at once, which is a click; it is a choice and not meant to be swept. The phases are spread by a fixed step (0.618 of a cycle), so that a note is the same each time it is played.
  - **Tests.** Rust: 1642 to 1655 (1821 to 1834 with the `web` feature, 25 on the real program as before; 1637 to 1650 and 1808 to 1821 without the default plugin formats): the pinned chord, the pulse (five widths, and the other waveforms), three voices and seven, the level, the width and the level, the side of the lowest voice, a unison of one, the allocations and a single channel, and the parameters' ranges and roundings. Web: 2141 tests in 98 files as before (eleven of them changed); browsers 142 as before.
    Mutants: 22, all caught in the end.
  - **Numbers at the end of the step.** Rust 1655 tests, 1834 with the `web` feature and 25 on the real program, 1650 without the default plugin formats and 1821 with `web`, and 3 of the shell's in each; web 2141 tests in 98 files; 142 in browsers; 452 commits on the branch before this entry.

- [x] **57. A step sequencer (an extra: the most visual modulator that fits inside track A).** *Done when:* a modulator of a new kind, `steps`, goes through up to sixteen values from -1 to 1 at a rate and drives what it has cables to by the value of the step it is at, as an
  LFO does; it is made in a lane or in a group from the terminal, from the page (the "+" menu of a lane or a group, and the "~" menu of a knob) and from a session file, and is saved; the page draws it as bars that are painted with a drag and moved with the keys, with a
  playhead that follows the engine, a stroke across them being one step of the history; a note can start it again at its first step; and each layer's test holds what that layer says to what the engine does (the page's law to the engine's by a table that the engine writes).
  *Not done:* a rate that follows the tempo, a retrigger for the LFO, a grid of semitones for a melody (see the limits), a step that is more than a value (a gate, a tie, a chance).
  - **What it was.** The modulators were shapes in time that a person sets by a few numbers (an LFO, an envelope), a control that a hand or a note moves (a MIDI control, a velocity, a key) and a number that a person turns (a macro). None of them is a *pattern*, which is what makes a patch
    move in steps (a pitch that goes through a figure, a cutoff that goes through a rhythm); and a row of bars is what the browser can give that the terminal cannot, which is what the page is for (D3).
  - **The source** (`CLAUDE.md`, "Step sequencer modulator"). `ModSource::Steps` has sixteen values in a fixed array (`MAX_STEPS`: the audio thread does not allocate), how many of them are in use (`length`, 1 to 16: the steps past it keep their values, so that a sequence that is
    shortened and lengthened again is as it was), a `rate` in steps a second, a `glide` (the part of each step, at its end, that goes to the next step's value in a straight line; the last goes to the first, and not to a step that is not in use) and a `retrigger` (any note-on with a
    velocity puts it at its first step: a lane's own notes, a group's the notes of any of its members). It drives as an LFO does, bipolar, `base + value * depth * range`, and its rate has the port that an LFO's has (`ModSource::rate` / `set_rate` are one place for the two). Where it is
    (the step as the whole part of the number, how far into it as the fraction) is published beside its output, in a telemetry slot of its own.
  - **As built, in six commits plus two fixes.** *The engine* (`6281403`): `steps_output`, the source, the two commands (`SetModulatorSteps`, `SetModulatorPosition`), 18 tests and 22 mutants. *The model and the rest* (`713435e`): the kind, the settings, `SetSteps`, the file
    (`type = "steps"`: `steps`, `length`, `rate`, `glide`, `retrigger`, with their checks and the rate's default of 8, an LFO's being 1), the history (`Key::Steps`: a stroke is one step), the terminal's pane (a row for each step, a retrigger row, `Left` / `Right`) and popup entry, and the page's
    client state (`client/steps.ts`, pinned to the engine by `fixtures/steps.json`; the reducers, the selectors, the mock engine, the knobs and the choice on the node), 41 Rust tests and 37 mutants of the page's. *The position* (`0ebdf1b`): `ModulatorSlot::position` and its slot after
    the output's, told by the controller, the loader and undo's put-back (`modulator_slot_commands`), `SlotKind::ModPosition` in the layout, `check_ids`, the pusher and the mock engine, 9 Rust tests and 8 of the page's. *The bars* (`8ba22f6`): `ui/StepBars.tsx` (a `role="slider"` for each step; a stroke
    that paints, with the bars between two places of a quick stroke set along the line; the keys; a double click; the steps past the length dimmed; a playhead written to the DOM on the frames), `client/stepbars.ts`, `Drive.steps` (a `Control` of a list), the menus, the node's height and a browser spec, 55 tests
    and 37 mutants. *The terminal's playhead* (`ec739ae`): `RowLook` and `State::selected_playhead` (the pane marks the step the sequencer is at and dims the steps past its length), `step_at` (the rule that `steps_output` is made from too, which the page's `playheadStep` is pinned to by the fixture), 10 Rust tests and one of the page's, 25 mutants of the terminal and the engine and 5 of the page's pin. *The docs* (this commit). The two fixes: a filter's tail that ended at 1e-38 and never at zero (f160, `21b2cbe`, below) and the mock engine's value of a step sequencer's driven rate, which was 0 (`03d773f`).
  - **Decisions I made, which you may overrule (D57).** The values are bipolar, -1 to 1, as an LFO's output is (a step sequencer that only went up could not drive a pan, or a pitch, both ways, and a sign of the cable's depth turns one into the other); a new one is a rising staircase at 8 Hz that
    every note starts again (it shows what it is at once, and a note-driven patch wants the pattern to begin with the note); the length hides steps and does not delete them; all sixteen values are one intent (a stroke across the bars is one step of the history and one command to the engine; an intent
    for each step would be sixteen steps of undo for one stroke, or a merge that the history has no key for); only the rate can be driven by a cable (the glide, the length and the retrigger are a person's, as a macro's value is); "New step sequencer" is last in the terminal's popup, where a person's fingers know the others.
  - **Found on the way.** The first whole browser run after the second part failed the scope test once, and repeated it failed four in twelve: f160 (see `CLAUDE.md`, "Denormals"). The flush to zero of f149 made a filter's states stop at 1e-38 to 1e-35 for as long as they were watched (18 of 28 types and cutoffs;
    12 of 15 places of the tracer's LFO, none without the flush), so that the scope never read exactly zero; the filter lets go of its states under 1e-20 at the end of every block now, and the pair of tests of f149 uses a delay of a millisecond, whose repeats still run down through the denormals. I wrote a wrong
    finding number into the document and a wrong count into a commit message on the way ("22 of 28" for what is 24 of 28: both corrected before anything was pushed, and the numbers in the finding are the measured ones). Two of the page's mutants lived and taught: one because a test waited for a layout that was there and
    not for a frame that had come, one because a harness that re-renders at once cannot tell a stroke that goes on from the props from one that goes on from what it has made (there is a test with props that never change now). The browser spec found that a node that has just been added is still moving (the
    canvas pans to it for a quarter of a second), so that a pointer sent to where its bars were missed them (`still`, as the other specs have it).
  - **Limits, as built.** The steps are values and not notes: the bars are held to a hundredth when painted, which is a hundredth of the range of what they drive at depth 1, so that a sequence of pitches on the oscillator's `detune` (±24 semitones) is out of tune by twelve cents at depth 0.25 (a grid of
    semitones for what it drives is what a melody wants; the terminal takes any number typed into a step). The rate is in steps a second and does not follow the tempo (the engine has no tempo in its graph: only a pattern has one); it runs without a note being held. A glide is a straight line. A step has no gate,
    tie or chance, and a sequencer has one pattern. The terminal's pane marks the step the sequencer is at (`Step 3 ▶`, magenta) and dims those past the length, from the position that the telemetry carries, which it reads on every frame it draws (about ten a second: a sequencer at 8 Hz is seen to hop about a step a frame). A chord restarts it once (the notes of a chord are at the same buffer). The bars are about 15 pixels wide when the
    canvas is at its full size, small for a finger: a node is not made wider for them (every node is 304 pixels).
  - **Tests.** Rust: 1655 to 1673 for the engine (18), to 1714 for the model and the rest (41; 1834 to 1852 to 1895 with the `web` feature), to 1718 with the filter's fix (4), to 1727 for the position (9; 1908 with `web`; 1722 and 1895 without the default plugin formats), to 1737 for the terminal's playhead (10: three of `step_at` and seven of the pane; 1918 with `web`; 1732 and 1905 without); mutants: 22, 48 (47 caught, one did not build)
    and 14, all caught in the end, and 25 for the terminal's playhead and `step_at` (24 caught: the one that was not changes nothing that can be seen, since a modulator that is not a step sequencer has no step rows for a playhead to mark). The page: 2141 to 2180 (the second part), 2189 (the position and the mock's rate), 2244 (the bars: 13 of the helpers, 25 of the component, 4 of the control, 6 of the bars on a node against the mock engine, the menus, the height and 5 stories) and 2245 (the playhead's bar pinned to the engine's table) in 101 files; mutants 37, 14, 37 and 5, all caught in
    the end. Browsers: 142 to 145 (three of `e2e/steps.spec.ts`: the node, its height and a playhead that goes through the steps; a stroke that is one Undo and one Redo and whose steps are in the saved file; a note that puts the playhead on the first step, and the choice of off in the file; 24 of 24 in eight runs).
  - **Numbers at the end of the step.** Rust 1737 tests, 1918 with the `web` feature and 25 on the real program, 1732 without the default plugin formats and 1905 with `web`, and 3 of the shell's in each; web 2245 tests in 101 files; 145 in browsers; 462 commits on the branch before this entry.

- [x] **58. A noise (an extra: a source with no pitch to build with).** *Done when:* the oscillator's `waveform` has a fifth value, `noise`: white noise as loud as a saw, that every front-end offers (the terminal's row steps to it, the page's select lists it,
  a session file says `waveform = 4`), that the envelope, the volume, the pan and the unison shape as they shape a wave, and whose numbers do not depend on the order that a hash map walks the voices in; and a session from before it sounds as it did (the pinned chord of the
  four waveforms is as it was). *Not done:* a colour (pink and brown noise), noise in the same lane as a wave (two lanes and a group sum them), a noise that is the same for every note.
  - **What it was.** The oscillator had four waveforms, all of them with a pitch. A patch that wants wind, breath, a hi-hat or a riser needs a source that has none: a filter at a high resonance on noise is the whistle of wind, and the sound that is in use here is a saw through a resonant filter and a reverb with a very slow LFO on the cutoff.
  - **The waveform** (`CLAUDE.md`, "Built-in plugins"). Index 4, after the saw, so that the numbers that sessions saved for the four before it are what they were. The oscillator's `waveform` reads its labels from `WAVEFORMS`, so that the terminal's row, the page's select, `tang describe` and the catalog list the fifth without a change of their own; what did change is every place that knew the top
    as 3 or the labels as four (ten tests in Rust, three in the page, nine fixtures and the snapshot of the tracer, which have the oscillator's parameters). `Voice::noise` makes the numbers: a xorshift32 state for each oscillator of the voice, from -1 up to 1, started from a hash of how many voices have begun and the oscillator's place in the unison, never 0 (the one input of the hash that gives 0 is the 234,361,646th voice's sixth oscillator, which has a test).
  - **Decisions I made, which you may overrule (D58).** A waveform and not a plugin of its own; white and not coloured; the level of a saw; the numbers of each oscillator of each voice are its own and each key that is hit has noise of its own.
  - **As built, in two commits.** *The noise* (`0f22bb8`): the fifth waveform, the voices' states, ten tests and 14 mutants, the ten assertions and the fixtures that knew four; *the docs* (this entry and D58; D57's row in the table of decisions had no status cell, and has one now).
  - **Found on the way.** One of the first 14 mutants lived and taught: the test of a chord began all its notes at one frame, so that the voices were at the same place of their attacks and weighed the same, and numbers that a shared stream dealt out in the order of the map summed the same in any order; the notes begin at frames of their own now, and that mutant is caught. A second lived and is no fault of the test: every oscillator of a voice taking its numbers from the first one's state, one after the other, is as independent as several states (13 of 14 caught, the other
    is equivalent). Clippy found two `mut` that the new tests did not need (the matrix's first run, before anything was committed). The vst3-only run of the tests failed once, on a test of the scope ring that has nothing to do with the noise (f162, open in the build log: the message holds a copy of three runs of frames, the later two a ring's two and three laps on, so that its reader was stopped in the middle of its copy while the test's writer, fifty times faster than a sound card, went on); it passed when it was run again and in 40 runs alone and 20 runs of all the tests.
  - **Limits, as built.** White only, and the numbers are uniform and not Gaussian (the same spectrum, a flatter spread of levels: a crest factor of 1.73, which a distortion after it would show). `detune`, `spread` and `pulse` do nothing to it, and their knobs still turn. The page's mock engine does not know the waveform (its scope draws a tone for any), so that the noise is seen in a scope only against the real program. The numbers are made on the audio thread, a xorshift step for each oscillator of each voice for each sample, with no allocation (a test counts it).
  - **Tests.** Rust: 1737 to 1747 (1918 to 1928 with the `web` feature, 25 on the real program as before; 1732 to 1742 and 1905 to 1915 without the default plugin formats): the fifth value and the four before it, whiteness (the mean, the level against a saw's, no number in the range of a voice exceeded, no lag of 1 to 64 samples that leans) and the same power in the bass as in the treble, no pitch at three notes, the sum of a chord the same whatever order its voices are walked in (with and without a unison), a key that is hit again, a unison of noise (the level of one voice, the sides not one signal, a single voice the same on both sides), the envelope's attack and release, no allocation, and the seed that is not 0. Web: 2245 tests in 101 files as before (three of them changed); browsers 145 as before. Mutants: 14, 13 caught and one equivalent.
  - **Numbers at the end of the step.** Rust 1747 tests, 1928 with the `web` feature and 25 on the real program, 1742 without the default plugin formats and 1915 with `web`, and 3 of the shell's in each; web 2245 tests in 101 files; 145 in browsers; 464 commits on the branch before this entry.

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

A proposal that grounds this in the engine as it is (what can be reused and what cannot, the one idea that keeps the controller and the page
almost as they are, the order of work, and the ten decisions that are the user's) is `docs/track-b.md`. Nothing of it is started.

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
| D15 | React Flow draws the canvas; `toFlow` is a pure function of the graph, nodes are functions of their data with controls that read their own values, and the canvas is laid out again only when the structure of the session changes (a function of the places it put the nodes at last too, from step 44: D45) | Built (step 12) |
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
| D39 | A control is modulated by one intent that makes the modulator and the cable to it, not by the page asking two (a modulator, then a cable) and joining the steps. The TUI had joined the steps of two or three intents (`Controller::history_mark`, `join_steps_since`), which only a front-end that shares the controller's process can do: a browser's two requests are two revisions with whatever another client does between them, and a join would have been a protocol of its own (a step that a client begins and ends). One intent can be refused as a whole, since everything that a cable to the port is refused for is looked at before anything is made, with the modulator at the place it will have (`param_cable`, `field_cable`, which `AddModEdge` plans with too: the same rules of reach and the same words); a refusal makes nothing, uses no id and leaves no step. Its inverse is the removal of the one node, which takes its cable with it. The alternatives were a list of intents carried out as one step (more general, and each intent of it would need an all-or-nothing of its own) and a modulator that is made first and taken out again if the cable is refused (a revision and a step for what never was). The rack it goes in is the one that reaches the port (the lane's for a lane's plugin, the group's for a bus effect, a modulator's own for its settings), so that the page and the TUI say "for this control" and not "in this lane"; the depth of a MIDI control that learns (the whole range) is the front-end's, as it is the TUI's `l`, and the intent has none of its own. The page's menu asks it (`Editor.modulate`) and so does the TUI, so the history has one thing for it, in the one set of words (`modulate cutoff`, `learn cutoff`). What could drive a control is looked up when its menu is opened, not when anything changes, since every knob has the button | Built (step 37) |
| D40 | Save as is an intent that carries a name, and the controller makes the file of it by one rule that both front-ends ask. A page never names a place to write (§8): it says what the file is to be called, and the program says where it goes, beside the session's own, so that an absolute name or `..` cannot put it elsewhere. The rule is `session::plain_file_name`, in the module of the session file, since it is about files and not about the controller, and it is stricter than §8 had asked: besides a folder it refuses the characters that a file cannot have on one system or another and the names that Windows keeps (a session is a file that people move between systems), and more than 255 bytes. A file that is there is not written over, from either front-end: there is no undo for a file (the history is of the session, and a step that put a file back would have to keep the file), and the terminal's popup, which wrote over any name, is the front-end that changed. The one exception is the session's own file: a save as the name the session has is a save, which is what a person who types the name in the field means. The refusal is the write's own: any file but the session's own is created exclusively (`session::save_new`, `create_new`: the system refuses a path that is taken, a link that leads nowhere included, which the write does not follow), so that there is no look at the path that a program which makes the file in between could make out of date (it was a look at first, `symlink_metadata`, until f128 said so). A refusal makes no revision, tells nobody and changes nothing; a save as that is done is told as `Saved { file }` with the new name, which every client already took as a save, so a page needs no event of its own. It is no step of the history (a save is none either: what was done can still be undone, and the file is no part of it). The alternatives were a page that checks the name itself and sends a path (the page would hold a copy of the rules, and the server would have to trust it), a list of the directory's files for the page to check against (it could be out of date by the time the name is sent, and is a second thing to keep right), a save as that overwrites (a name typed by mistake loses a file) and a check in the page only (the TUI had `dir.join(name)`, which §8 had called not safe to reuse). The mock engine has the rule written out, which the controller's fixture of 67 names pins it to, name by name, since two rules written in two languages differ at the edges (what is a control character, what is white space) | Built (step 38) |
| D41 | What a cable to a modulator's own setting does is drawn as what a cable to a parameter does, by the engine's law for it, which is not the same: the engine writes a setting once for each cable that drives it, each from the value the user set, and the last write stands (`apply_cross_mod`), where the cables on a parameter add up (`apply_modulators_to_chain`). So the ring of the knob of a setting is the extent of the last cable alone (`settingExtent`: `modulationExtent` of the last of the cables, in the order of the graph's edges, which is the order the engine writes them in: a group's cables, then a lane's, each modulator's in the order of its rack), and the dot is the value that the engine says the setting has, read back from the setting once every write is applied (`publish_cross_mod_effective`, and for a group's cable to a member's modulator after the lanes have run, `publish_group_member_mod_effective`, since the member's own cross-mod, inside the lane, comes after the group's and wins). A MIDI control that is not bound, when it is the last of the cables, writes the user's value over the others (its offset is nothing, `Modulator::target_offset`), so it hides the ring of the cables before it, as it hides their effect. Every target has a telemetry slot whatever it drives (`ModTargetKind::has_effective_value` is gone), so that one rule holds for the model, the layout (which names the value at the port the cable ends at: `rate`, `attack`, ..., or the `depth` of the cable that is driven) and the mock engine, which has the law (`#settingEffective`) and the layout's order (`withEntry`) written out, pinned by tests and by the fixtures that the controller writes. The depth of a cable has no ring, since a slider has no room for one: it has a bar from the middle under it (`LiveBar`, which the panel's rows have under their sliders) for the depth that the engine has made of it, and the slider stays what the user set. The alternatives were a ring that sums the cables (it would draw swings that never happen: with two cables the first is written over), a ring that is the union of what each could do (it says where the value could go if the order were another, not what it does), an engine that sums its cross-mod writes as it sums the parameters' (it changes what an existing session sounds like, for the sake of a drawing), and a mark on the slider's own track (the travel of a native thumb is inset by half a thumb, by a measure that differs by browser, so a mark at its fraction of the width is off at the ends; the bar's scale has the same inset, which is accepted, as the panel's rows have it) | Built (step 39) |
| D42 | A parameter that a lane's modulators and its group's both drive is drawn as the lane's alone: the ring is `modulationExtent` of the lane's cables (`modulationsOn`), and the mock engine's value of it is made from them. The engine writes a group's offsets to a member's parameter before the lane renders (`apply_group_mod_writes`) and the lane writes its own after (`apply_modulators_to_chain`), each sum from the value the user set, so that what stands is the lane's and the group's is not added to it, whatever the lane's modulators are: a MIDI control that is not bound yet, which adds nothing, writes the user's value over the group's too (both pinned by engine tests, `a_lane_modulator_and_a_group_modulator_on_one_parameter_leave_what_the_lane_makes_of_it` and `a_lane_control_that_is_not_bound_yet_and_a_group_modulator_on_one_parameter_leave_the_value_the_user_set`). The page knows whose a modulator is from its node's `parent` (a group, or a lane), in `modulationsOn` and not in `modulationsAt`: `modulationsAt` is the settings' too, and for a setting the write that stands is the last, which is the lane's already, since the graph lists a group's cables first (D41). The dot was right throughout: the layout names the slot of the first cable to aim at a value, a lane's before a group's, and the lane's value is what the engine stores in it. The alternatives were a ring that adds both (it drew swings that never happen, which is what it did), the union of the two (it says where the value could go if the order were another, as D41 has it for a setting) and an engine that adds the group's offset to the lane's (it changes what an existing session sounds like, for the sake of a drawing) | Built (step 41) |
| D43 | The scope sends samples, and each front-end makes its own picture from them. The engine keeps the last 8192 frames of the output mix in a ring (`ScopeRing`: 64 KB, 170 ms at 48 kHz; the audio thread stores after the master meter, a reader copies the latest frames without a lock and takes a copy again if the writer overtook it), and the server sends the latest 2048 of each side (43 ms) on a channel of its own, `scope`, to a client that asks for it. The wave is the samples; the spectrum is made from them by the client (`client/fft.ts`; the terminal has its own FFT, in `scope_view.rs`), so that the one channel serves both pictures, and nothing is worked out for a client that shows neither. The channel is 16 KB thirty times a second for each client that asks, so that a page asks for it only while a scope is on it (`Client.watchTelemetry`: counted, so that two nodes that show it ask once and the last to go lets go; asked again on every connection, since a connection that was lost forgets what was asked). A picture of the wave starts where the left side goes up through the middle after having been 0.02 under it (the trigger), among the places that leave room for the whole picture, so that a wave that goes round again and again stands still; it needs more frames than the picture has, which is why 2048 are sent for a time base of 256 to 2048 samples (with none such, in silence, the latest picture is drawn). The time base is told in milliseconds from `Meta.sampleRate`, which is the controller's own (the rate that the plugins are loaded at). The choices of a scope, the view and the time base, are the viewer's: they are kept by the node and are neither saved nor sent, so that two browsers can show a wave and a spectrum at once and a scope's setting is no property of the sound. The alternatives were levels made by the engine (an FFT on a thread of the engine's for pictures that nobody may be showing, and a second frame format), a ring that holds a picture and not samples (a time base is a choice of the picture), a scope for each lane and group (64 KB and a store in every lane's `process` for pictures that nobody asked for; the master's is where the output is judged, and the layout could name the others later), the browser's Web Audio (the browser is a control surface, not an audio engine: D1) and a view that is saved in the session (it would put a person's choice of picture in the file that the others load) | Built (step 42) |
| D44 | A cable to a setting that is turned on a log scale moves it a fraction of the knob's travel. The knob of a rate (0.01 to 50 Hz) and of an attack, a decay and a release (0.001 to 10 s) runs over decades (`ModulatorField::taper`, `Taper::Log`, `MODULATOR_FIELD_SCALES`), the ring on it draws the depth as a fraction of the knob on both sides of the pointer, and the engine moves the setting there: the user's value goes to its place on the knob, the modulator offsets that place by its own law (an LFO plus or minus `depth`, an envelope from `depth` of the way down to the user's value, a MIDI control as it did) with the knob's travel as the range, and what comes out is a value again (`Modulator::modulated_value`). A swing is as far up the knob as down it, and an LFO at the default depth of 0.5 on an attack of 10 ms goes from 1 ms to 1 s, where it went from 1 ms to 5 s and sat at 1 ms for half of each turn. What is turned by distances, a sustain, the depth of a cable and the parameters of a plugin, goes by its range, as does a range that starts at nothing (there are no decades in it). The alternatives were the law that there was (a depth meant nothing at the bottom of the knob and everything at the top), a depth in octaves or in a ratio (a unit that no knob shows, and a different number for each setting, since the ranges are of different widths), and a law that is set by each setting's own taper in a table of the page's (the engine's `ModulatorField::taper` is the one place that says which settings are on a log scale, and the page reads it from what the controller generates) | Built (step 43) |
| D45 | The canvas keeps the nodes where they are, and the layout is given where it put them last. `toFlow(graph, remembered)` stays a pure function (of the graph and of the places it is given, `Flow.places`), the page holds the places from one change of the session to the next (`useLayout`), a node stays where it was while its slot is the same, and what needs room makes it by moving rows, strips whole (`makeRoom`); "Arrange" lays everything out from nothing, with the places that a person gave given back, and fits the view. The alternatives were a layout in which a cable makes no row (a cable between two modulators that are in one row has nowhere to run: it is why step 26 made the rows), the place of every node in the session (a `SetNodeUi` for each node that the layout would have moved: the file would be full of places that nobody gave, and a layout that is better later would not reach a session that has them: Open question 6), the difference between two layouts (a node goes as far as the layout moved it: a cable moves the strip, which is the complaint) and an animation from the old layout to the new (the nodes still move under the hand that drew the cable) | Built (step 44) |
| D46 | A random LFO draws its numbers from a hash of its seed and its cycle, not from a generator that it holds. The number of cycle `n` is `noise(seed, n)` (`plugin::chain::noise`, the top 24 bits of a 32-bit hash, from -1 up to but not including 1), a modulator's seed is the order in which modulators came into the audio graph, and the page's mock engine works the same hash out in TypeScript (`client/noise.ts`) and is pinned to it by a table that the engine writes. The numbers are a function of the seed and of how many times the LFO has gone round, so that a session that is loaded plays the same numbers each time, a test can say what they are, a smooth LFO has the next cycle's number before the cycle begins (it is `noise(seed, n + 1)`, with nothing to advance), the mock engine needs only the number of the cycle it is in, which is `floor(rate * seconds)`, and there is no state of a generator for undo to put back. The alternatives were a generator with a state that the modulator holds (xorshift: a state to advance, so that a smooth LFO keeps two numbers in hand; the mock engine would have to run the same generator in step with an engine whose clock it does not have), the system's randomness (not repeatable, so that no test can say a number, and a session sounds different each time), a seed in the session file (the same numbers after undo and for the browser's mock too, but a field in the format for what no one can hear; it comes with a button that draws new numbers, if one is wanted: Open question 7) and the node's id for a seed (stable, but the engine does not know ids: they are the model's, and every command would have to carry one). A seed that is the order of coming in means that what is added later, or put back by undo, draws other numbers | Built (step 45) |
| D47 | A node's scope is of a signal that the engine listens to only while somebody watches it, through a small pool of rings. The audio thread has a tap at the output of each lane's instrument (after its volume), of each effect (after its mix) and of each group (after its pan; a bus effect's is after its mix), which does nothing when the node has no ring, and `Telemetry` has `PROBES` = 8 rings of the scope's own kind (`probes`, 8 × 64 KB, made with the telemetry so that asking allocates nothing on the audio thread). The controller keeps who watches which nodes (`Controller::set_probes`: a `Watcher` is a client of the web interface, by the number of its connection, or the terminal, `Watcher::TERMINAL`) and which ring each watched node has (`reconcile_probes`, which tells the audio thread with `SetProbe` and `ClearProbes`, and runs again whenever the structure of the session changes, so that a ring goes with its node when the lanes and the effects change places); two watchers of one node share its ring, and a node that no ring is left for is given one when it is free. The frames are a telemetry frame of channel 4 whose `u32` is the node's id (a probe follows no layout), 2 × 2048 values, 30 times a second to each client that watches the node (`Client.watchProbe`: counted like `watchTelemetry`, and asked again on every connection). The page shows a scope in the node of an instrument that has a plugin, of an effect and of a group when its viewer presses the button in its header, and asks for the signal while the scope is there. Which nodes show a scope is the viewer's own, like which node's parameters are in the panel: it is not saved, not sent, and not in the history, and the layout is told, `toFlow(graph, remembered, scopes)` (a node that shows one is `NODE_SCOPE_HEIGHT` taller, and room is made for it as it is for a knob that is pinned). The terminal's Session tab shows the scope of the node under its chain cursor in a strip (`o`), and watches as a `Watcher` of its own while the strip is shown, so that it asks for the node under the cursor and for nothing else. The alternatives were a ring in every lane and group (D43's refusal: 64 KB and a store in every lane's `process` for pictures that nobody asked for), a ring that is made when it is asked for (the audio thread may not allocate: the controller would make it and hand it over, as it does a plugin, and take it back to drop), a ring for each client and node (the cost of the audio thread would be the number of people looking and not the number of nodes looked at), taps that the client places (what a node's output is, after its volume or its mix, is the engine's to say) and a scope that is in the session (a choice of picture is the viewer's: D43) | Built (steps 46 and 47) |
| D48 | A macro is a modulator whose source is one number, 0 to 1, that a person sets: the engine's `ModSource::Macro { value }` puts it out, and what it drives is moved as a CC moves it (`base + value x depth x range`, from nothing at rest up), so that a new macro moves nothing until it is turned and the depth of each cable is how much of the knob that parameter takes. Its value is a setting of the modulator (`ModulatorField::Value`, 0 to 1, linear) and goes to the engine as a new macro in the place of the old (`SetModulatorSource`: there is nothing else in a macro, so it has no command of its own and no cross-mod base to keep); the file has `type = "macro"` and `value`. The alternatives were an unbound MIDI control that has a value (a control that is not bound is armed again when a file is loaded, so a macro in a file would bind to the next controller that moved), a list of knobs on the lane, apart from the graph (a macro would be no node that a cable starts at, and a group could not have one), a value that goes both ways (the sign of the depth of a cable turns it round already, and a level that starts at nothing is what a CC and the page's knobs are) and a value that cables can drive (what that is for is a MIDI control that turns the macro, which needs a port on the node, a field of cross-mod, the rule of reach and the page's handle: one more kind of port, and f84 asks for a refactoring before a third: Open question 9) | Built (step 48) |
| D49 | The tempo is the session's own, and a pattern keeps the tempo it was recorded at. `SessionModel::bpm` is the tempo that patterns are recorded at (the next recording's, and its count-in's); the file has it as a top-level `bpm`, written only when a load would not come to that tempo anyway, so that the files of sessions that never set one are as they were; `SetBpm { bpm }` sets it (held to 20 to 300, a step of the history, told as `BpmSet`); and the `bpm` of a pattern is the tempo that its length in frames was counted in, which setting the session's tempo leaves alone. The alternatives were: the tempo kept only by the patterns, which is what it was (a session with no pattern lost the tempo, a session whose patterns were at different tempos left the audio thread at the last one's while the program said the first's, and the terminal's `b` wrote its tempo into every pattern, which changed the length of each when it was loaded); a tempo that a recorded pattern follows, with its events kept in beats and played at the tempo there is, which is what a sequencer does and what an engine that keeps patterns in frames cannot, and which changes what a pattern is in the file: Open question 10; and a tempo that the page does not set, the terminal's `b` alone, with which a page that is to record patterns could not say how fast. | Built (step 49) |
| D50 | What is done to a pattern is done on the lane, by three intents that are answered by one event, and is no step of the history. A pattern is the lane's own (the model's `InstrumentNode.pattern`, the engine's `PatternPlayer` of the lane; it has no `NodeId`), so `SetRecording`, `ClearPattern` and `SetPatternField` name the lane's node, and `PatternSet { node, pattern }` tells the whole of what the lane has afterwards (a `PatternInfo`, or null): a client replaces the lane's pattern and has nothing to merge, whichever intent it was. The notes are not in `PatternInfo`, only how many there are: a page that draws them asks for them with a message of its own. A recording that ends by itself is not an event: the audio thread reports it to the controller's `poll`, which folds it into the model, and the browsers are given a snapshot (`resync`), as they are for a MIDI control that was captured. The alternatives were: a node for the pattern (the TODO "patterns as graph nodes": a pattern is a source of notes for a lane, which a "keyboard as a node" would be too, and the two want deciding together; no cable ends at one today, so that nothing in the page needs it); one event for each intent (`RecordingSet`, `PatternCleared`, `PatternFieldSet`: three things for every client to apply, each with a part of the state, against one that is replaced); and steps of the history: a setting is easy to put back (the old value, or a clear when the lane had no pattern), but a clear and a recording take notes that only the model has, and the result of a recording arrives from the audio thread with no intent, so that undo of them is a design of its own (Open question 11). | Built (step 50) |
| D51 | The pattern is drawn as a block of fixed height under the controls of every lane, an empty lane too, with what a lane that has no pattern cannot use dimmed and not left out; a clear asks first, in words that say it is for good; the length is the page's number field. A block that came and went would change the height of the node, which the layout counts on (`PATTERN_HEIGHT`, measured in a browser) and which would move the nodes below it every time a recording ended; a lane that has no pattern needs the button that records one and the length that its first recording will take; and a clear that asks is what undo not having it asks for (Open question 11). The alternatives were: a node for the pattern beside the lane (D50's TODO: it would be a source of notes with a cable to the lane, which is what a keyboard as a node would be, and the two want deciding together); the controls in a panel behind a button in the header, as the parameters have (nothing would show that a recording goes on, and a record button is something to press at the moment, not to look for); the block only on a lane that has a pattern (a lane that has none could not record one); and no question before a clear, which is what the terminal's `d` does (the terminal is a key away from a recording, and a page's menu is a click away from a mouse that has gone wrong). | Built (step 51) |
| D52 | A velocity and a key are bound sources of the kind that a MIDI control is, with a kind each to make, and a key is bipolar around middle C. They are `MidiBindSource::Velocity` and `Key` (the file writes `midi_velocity` and `midi_key` on a `type = "midi"` modulator; the protocol has `{ type: "velocity" }` and `{ type: "key" }`), and `ModulatorKind::Velocity` and `Key` make them, bound at once and not armed: no controller is moved for a note's velocity, which the controller cannot tell from the note. A velocity goes up from nothing as a CC does; a key is the number of the note, centred where it adds nothing at middle C (a note is a place on the keyboard, and what a person wants of it is that the notes either side of the middle move a thing either way), with five octaves either way for the whole of the depth. They rest where they add nothing (`MidiBindSource::rest`), and keep the last note's value when it ends. The alternatives were: a source of its own (`ModSource::Note`): the file, the tree and the page's node already say a MIDI control, learning is a state that these never have, and the code that matches on a control's source is checked by the compiler either way, so that a variant of the kind that is there costs less than a kind that is beside it; a choice of source on a MIDI control that is there, so that a CC could be turned into a velocity: no intent changes what a control is bound to (it is learned or it comes from a file), and one would need undo of its own; learning a velocity from a note that is played: a note is not a control, and every note would bind it; a key from 0 at note 0 to 1 at note 127, as a CC: a base that a person set would be the value for the lowest note, and a note's pitch has no rest; and a velocity and a key per voice, which is what a patch's `Note` node is (`docs/track-b.md` §4) and what lane modulators, which run once a buffer, cannot be. | Built (step 52) |
| D53 | A mute and a solo are flags of a lane and of a group, kept apart from its volume and put into effect at the end of the lane and of the bus, as a gain that goes to 0 or back to 1 over one buffer, with a mute that wins over a solo and a solo that carries what belongs to it. They are `muted` and `solo` of `InstrumentNode` and `GroupNode` (the file's `mute` and `solo`, written only when on), set by the intents `SetMute` and `SetSolo`, and what is heard of them is `AudioGraph::process`'s to say: nothing that is muted; while anything is soloed, only a lane or a group that is soloed, a lane in a soloed group (a lane's flags are its own: a lane in a muted group is not muted, and its bus is) and a group that has a soloed lane in it (its bus carries that lane, and not the others in it). Everything goes on while a node is silent, the notes, the modulators, the patterns and the tails of the effects, so that a key that is held is heard again when the node is brought back and a mute is not a stop; the meters read after the gain and the scope taps before it. The page draws a node that cannot be heard dimmed and says so by the same law written again (`audibleOf`), pinned to a run of the engine over every combination of the flags of two groups and four lanes. The alternatives were: a mute that sets the volume to 0 (the number a person had set would be lost, a knob would say 0 for a node whose volume is 0.8, and a solo could not be made of it); a mute that stops the lane (a held note and a tail would be lost, the modulators would stop, and the lane could not be brought back where it would have been; it would save the plugins' CPU, which is for later and is in `CLAUDE.md` as a TODO); no ramp (a click at every press, at the edge of a loud sound); a solo that is exclusive (soloing a second lane to hear two together is the commonest use of it, and taking the others' solo off is a gesture to add and not a state); a solo of a group written into the flags of its members (a group's solo would be the states of other nodes, which a press on a member would have to unpick, and undo would be many steps); a lane in a muted group that is itself muted by it (its meter would read silence for a lane that plays, and nothing on the lane would say which of the two is cutting it); and the flags in the page's state only (a session saved with a lane muted is loaded with it muted, and the terminal and a second page see it). | Built (step 53) |
| D54 | A delay is a built-in effect that puts out its echoes only, with its time in seconds on a linear scale that is gone through at a limited slope. The blend with the sound that is put in is the host's `mix`, as the reverb's is (an effect that added the sound itself would double it under the mix, and what is heard first of a note would no longer be the note). The tone is in the feedback only, so that the first echo is the sound as it was put in and each repeat is darker; the ping-pong is one number, which crosses the feedback and moves the sound that is put in to the left as the sum of the two sides; a change of the time is a sweep in the pitch of the echoes (a one-pole of 10 ms and no faster than half a sample of delay for each sample), as on a tape echo; and the echoes are soft-clipped where they are fed back. The alternatives were: a time on a log scale (the filter's cutoff is a position on one; but what a modulator moves is a fraction of the range, and ±5 ms at a time of 0.375 s is a depth of 0.0018 from a thousandth of a second to two on the log scale and of 0.0025 on the linear: it does not make a wobble easier to set, and a number that is not seconds says nothing on a knob; a wobble wants a parameter of its own, which is a TODO); a time that is a note value of the session's tempo (the effect would need the tempo and a choice of divisions: the next thing, and a different parameter); a crossfade between two read heads at a change of the time (no pitch effect and no click, but a modulator on the time would not make the tape wobble that it is used for, and a jump of the knob would be a cut); no limit on the slope of the sweep (the exponential follow of a jump of 200 ms at 10 ms starts at 20 samples of delay for each sample, which reads the echoes backwards at the start of it); a ping-pong that is a switch (a gradation is what a modulator can move); and a dry sound that the effect mixes in itself (the first sentence). | Built (step 54) |
| D55 | The delay's modulation is a sine on the left side and a cosine on the right, in milliseconds, with its depth gone through over 50 ms. It is in the delay and not left to a modulator on `time` (D54): the range of `time` is two seconds, so that a wobble of a few milliseconds is a depth of 0.001 to 0.005, which the page's slider and the terminal's keys cannot set (0.01 is twenty milliseconds), and a parameter of its own has the milliseconds. A quarter of a cycle between the sides makes the two sides different echoes, which is what makes a chorus wide; milliseconds and not a fraction of the time because a chorus wants a few milliseconds whatever its time is. The alternatives were: a finer step for a modulator's depth everywhere (the right thing for every parameter with a wide range, plugins of the machine too, and a design of its own: a slider that is finer near 0, or a field to type in; it is the TODO that this leaves); a depth that is a fraction of the time (a tape echo's wobble is a variation of speed, which is a fraction of the time, and a chorus's is a few milliseconds: one parameter for both would be right for one); an LFO with a waveform and a sync (more to choose and nothing the first wobble needs); and a depth that steps (a click at the top of the cycle: the delay would step by up to 480 samples). | Built (step 55) |
| D56 | The oscillator gets a unison, a spread, a width and a pulse width as parameters after the ones it had, with starting values that change nothing. They are in the oscillator and not in a built-in of their own, and after `pan` and not among the first. The alternatives were: a built-in for the unison (a copy of the voice engine for four parameters, and a second kind of instrument in every list and menu); a lane for each oscillator of the unison (what a person can do today: seven lanes, each with its own envelope, volume and keys, and nothing to move them together but a group's modulators; this makes it one parameter); the parameters among the first eight (a node shows the first eight, but every session, fixture and test names the parameters by place or by name, and moving an index would move them all: the panel and a pin put these on a node, and a node that is to show them says so in the file); a balance for the width (it takes the voices at the side out of the other channel, 0.83 of the level at five voices and half the width, where a law of equal power keeps it); a random start for the phases of the oscillators of a note (a note that is not the same each time it is played, and a test that cannot hold it); and band-limiting the waveforms in the same step (it changes how the oscillator sounds, which is what the kernel refactoring of track B, B0, does, and that waits for the decisions in `docs/track-b.md`). | Built (step 56) |
| D57 | A step sequencer is a source of modulation of its own, with sixteen values in a fixed array and a length, a rate in steps a second, a glide and a retrigger, and its values are set all at once. A fixed array and a length, and not a list that grows: the audio thread does not allocate, and the steps past the length keep their values, so that a sequence that is shortened and lengthened again is as it was. Bipolar (-1 to 1) as an LFO is, and not 0 to 1 as an envelope and a control are: a pitch or a pan has two directions and the sign of a cable's depth is the one thing that would otherwise turn it. The values are one intent, `SetSteps`, and one command to the engine (`SetModulatorSteps`), and not a step of the history for each bar: a stroke across the bars sets many and is one Undo (`Key::Steps`, the changes of one thing a second or less apart). Only the rate has the port that a cable ends at: the glide, the length and the retrigger are a person's, as a macro's value is. A new one starts as a staircase at 8 Hz that a note starts again, which shows what it is at once. Its position is published as a second telemetry slot after its output, and not packed into the output's number, so that a trace of what it puts out and a playhead are not one reading. Alternatives left: an arpeggiator of notes (a source of notes is the keyboard-as-a-node idea of the future list: it needs a MIDI cable), 16 parameters of a built-in plugin (a modulator is not a plugin, and a cable cannot end at a list), a rate in beats (the engine has no tempo in its graph yet). Resolution: 0.01 on the page (a grid for pitch is the first limit in step 57). | Built (step 57) |
| D58 | The noise is a waveform of the oscillator, white, and each voice makes its own numbers. A waveform, and not a plugin of its own: the oscillator already has the envelope, the volume, the pan, the unison and the width that a noise wants, a lane has one source (a noise with a wave is two lanes and a group, which the page draws), and a value at the end of the list keeps every number that a session saved (3 is the saw that it was). White (every number from -1 to 1 equally likely, the level of a saw: the same RMS, a third of the square of the range), and not coloured: a colour is a filter's work (a low-pass after it makes it pink or brown by ear), a `color` parameter would be one more thing for the other four waveforms to have as a knob that does nothing, and its law is somebody's to decide (the proposal is a blend of white, a pink filter and a leaky integral, each brought to the level of white). The numbers of each oscillator of each voice are its own, from a state that starts at a hash of how many voices have begun and its place in the unison, and not from a stream that the voices share: the voices are summed in the order of a hash map, which changes from one run to the next, so that a shared stream would deal the numbers out differently each time, and the same notes would not sound the same twice. And each key that is hit has noise of its own, where the phases of a wave are spread by a fixed step so that a note is always the same: a hi-hat roll on one key would be a machine gun if every hit were the same burst. | Built (step 58) |

Open questions

1. Display text for external plugin parameters (§4.3).
2. Whether the `web` feature joins the Windows CI matrix. (It does, as of step 10,
   with `--no-default-features --features vst3,web`; the first run of that will say
   whether it builds there.)
3. Worker-thread plugin loading (phase 1): completion messages, cancellation,
   what the TUI shows while a load is pending.
4. Session browser and runtime session switching (phase 4): tearing down and
   rebuilding the graph while the stream runs.
5. Track B (§13): the decisions of `docs/track-b.md` §8: the direction (more built-ins first, the patch, or the kernels and then both),
   one signal type or typed cables, voices from the start, where the patch is drawn, our own DSP or FunDSP, the terminal's part.
6. Whether the places that the layout remembers (what the canvas looks like when cables have been drawn on it and it has not been arranged) should be the session's, so that a page that is opened again and a second browser show the canvas as it was left (D45).
7. Whether the seed of a random LFO should be the session's (a `seed` in its table, and a button that draws new numbers), so that the numbers of a modulator that undo puts back, of a session that is edited and loaded again, and of the browser's mock engine are the ones it had (D46).
8. Whether the scopes that a person has shown (the nodes' buttons) should be kept, and where: in the session beside `ui` and `pinned` (every page, and a page that is opened again, shows them; a person's own choice of pictures goes in the file that others load: D43's argument), or in the page's own storage (a reload keeps them, a second browser does not), or nowhere, as it is (a reload closes them all, and so does anything that takes the canvas off the page) (D47). The terminal's strip (`o` on the Session tab, step 47) is the same question: whether it is on is not kept either.
9. Whether the value of a macro should take cables: a MIDI control bound to it (a hardware knob turns the macro, and the knob on the screen follows) and an LFO (D48). It needs a port for the value on the node, a field of cross-mod in the engine and in the file (`mod_value`), the rule of reach and the page's handle, and the refactoring of f84 (the node's data keeps what a modulator drives as two lists) before a third kind of port. The other way is a MIDI control that has a value that a person can also set on the screen, so that a control is one node and a macro is a control that is not bound to hardware: the file's re-arming of a control that is not bound would have to say the difference.
10. Whether a pattern that has been recorded should follow the session's tempo (D49): its events kept in beats and played at the tempo there is, so that a change of the tempo makes it faster or slower, as it does in a sequencer. A pattern keeps the tempo it was recorded at today: its events are frames of the audio thread's, and its length is its beats counted at its own `bpm`. It would change the saved form of a pattern (`frame` becomes a position in beats, and `bpm` of a pattern means nothing), the engine's player and the page's drawing of a pattern, and a pattern that is played in another tempo than it was played in would sound as it was not played.
11. Whether what is done to a pattern should be undone by Undo (D50). A setting could be a step at once (the old value is read before the change, as for every setting; a setting that gave a lane its first pattern is put back as a clear). A clear and a recording take notes: the model has them (`PatternState.events`), so a step could hold them and put them back with the commands that the loader sends, but the result of a recording comes from the audio thread with no intent, so that the step that undoes it would be made in `poll` and not by `apply`, and a recording that is going on when the person presses Undo needs a rule (stop it? leave it?). Until then a clear is for good, as the terminal's `d` is, and the page's button should say so.
