# Web UI — design

Status: **planned**. Nothing here is implemented yet. `CLAUDE.md` carries the
short version and the TODOs; this file is the long form. Update both as steps
land (tick the boxes in §12, move TODOs out of `CLAUDE.md` into the relevant
section). Decisions are listed in §14 with their status.

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
  when something is written to it), and events for what the audio thread reports
  to the controller (a finished pattern recording, a captured MIDI-learn control):
  they change the model, and a connected client sees them at its next snapshot.

### 3.3 Real-time note lane

`app::NoteInput` is one lock-free component that does what the `MidiManager`
callback and `VirtualPiano` each do today: `PianoFilter` gate → `HeldNotes`
update → `try_send` on the MIDI channel. The browser's notes arrive on the
WebSocket, are handled in the web runtime and call `NoteInput` directly, with no
controller round trip. The runtime tracks the notes each connection holds and
sends note-offs when the connection drops, so a closed tab cannot leave notes
stuck.

## 4. Model

### 4.1 Session and IDs

`app::Session` holds the instruments, groups, global BPM and a mirror of the
piano scale/mode, plus `next_id`. Every node gets a `NodeId(u32)`: allocated by
the session, never reused during a run, not persisted. Modulation edges get
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
  on plugin nodes, `ModField(rate | attack | …)` on modulators (cross-mod).
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
ui = { x = 320, y = 120, pinned = ["cutoff", "resonance"] }

[[instrument.modulator]]
type = "lfo"
ui = { x = 320, y = 20 }
```

The same on `[[group]]`, `[[group.effect]]` and `[[group.modulator]]`; the master
node's position goes in a top-level `[ui]` table. `pinned` lists the parameters
shown as knobs and ports on the node (plugins can have hundreds; the inspector
lists all of them). Attaching a cable pins its target automatically.

## 5. Intents and events

`Intent` is everything a front-end can ask for, addressed by `NodeId`. The TUI
resolves cursor → ids and then calls `apply`.

- Params and gains: `SetParam`, `SetGain { Volume | Mix | GroupVolume |
  GroupPan }`, `LoadPreset` (built in step 6; the rest of the list is not).
- Structure: `AddInstrument`, `ReplaceInstrument`, `AddEffect { parent }`,
  `RemoveNode`, `MoveEffect`, `SetRange`, `AssignGroup`, `AddGroup`,
  `RenameGroup`.
- Modulation: `AddModulator`, `SetModulatorField`, `AddModEdge`, `RemoveEdge`,
  `SetEdgeDepth`, `LearnMidi`.
- Patterns: `RecordPattern`, `ClearPattern`, `SetPatternField`.
- Globals: `SetBpm`, `SetScale`, `SetPianoMode`, `Save`, `SaveAs`, `Undo`,
  `Redo`.
- Layout: `SetNodeUi` (not undoable; marks the session dirty).

`apply` returns `Applied { rev, events, undo }` or an `IntentError` (`NotFound`,
`Invalid`, `Busy`, `Io`, …); today `Applied` has no `undo` and the errors are
`NotFound` and `Invalid`. Every request carries a client `req_id` for the
ack/err reply. Note on/off bypasses the controller (§3.3).

Events carry a monotonically increasing `rev` (in the code it sits on `Applied`,
one per intent; on the wire (step 8) `StampedEvent` is the revision and the event
in one object, `Applied::stamped()` makes them): `ParamSet`, `NodeAdded`,
`NodeUpdated`, `NodeRemoved`, `EdgeAdded`, `EdgeUpdated`, `EdgeRemoved`,
`OrderChanged`, `Meta` (dirty, path, BPM, piano), `PluginLoading`,
`PluginLoaded`, `PluginFailed`, `CatalogBatch`, `PatternRecorded`,
`MidiLearned`, `Devices`, `Resync`. Structural events carry whole replacement
nodes so client reducers stay trivial; value events are tiny. A client that sees
a gap in `rev` refetches a snapshot.

**Undo / redo.** Each `Applied.undo` holds inverse intents. Structural removals
also carry the removed subtree (with its modulation edges) and the plugin
instance recovered from the engine's return channel, so undoing a removal is
instant and does not reload the plugin. Invertibility is part of the intent
design from phase 0; the undo UI lands in phase 1.

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
  nobody subscribes. Held notes go as a 16-byte bitset, on change.
  **As built (step 10):** the channels are `meters` (kind 0) and `modulation`
  (kind 1), little endian, `flags` 0. `web::pusher` makes a frame per wanted
  channel every `Config::telemetry_period` (33 ms) from `Shared::layout()` and the
  atomics, and the hub queues it for the clients that asked
  (`{"type":"telemetry","channels":["meters"]}`, replacing what they asked for
  before; `[]` stops). A clip count goes as a count in an `f32` (exact up to 2^24
  buffers). A frame names the `rev` of the layout it follows; the layout is in the
  snapshot, and a `layout` message tells the subscribed clients when it changes
  (a frame of a revision the client has not been told of is to be ignored). The
  held-notes channel is not built.
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
    `snapshot { snapshot }` (the answer to `subscribe`), `event { event }` (a
    `StampedEvent`), `layout { layout }`, `ack { reqId, rev }`,
    `rejected { reqId, error }`, `closing { error }` (the last thing before the
    server closes), with `error = { code, message }`, `code` one of `notFound`,
    `invalid`, `badRequest`, `unauthorized`, `rateLimited`, `overloaded`.
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
`LiveBar`, `Meter` and `ClipLight`, each a function of its props that draws what changes
every frame by writing to the DOM; what was listed here as `Slider`, `Node`, `Port` and
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
- Nodes show their pinned parameters as knobs; everything else is in the
  inspector. Attaching a cable pins the target.
- The limits are the engine's (§2): serial audio path, block-rate lane-global
  modulation.

**As built (step 12).** `PatchView` draws `toFlow(graph)` with React Flow: a pure
function of the graph that gives each instrument a strip (the instrument, then its
effects, in columns of 376 px), its modulators above the strip over the node each drives
(side by side when they drive the same), each group to the right of the longest lane level
with the middle of its members, and the master at the end in the middle of it all. Nodes are
304 px wide, a header is 44 px and a row of four controls 108 px; the layout assumes those and
`patch.css` keeps to them. A node shows its plugin's first eight parameters and every one a
cable ends at ("pinned" until the session keeps pinning), then its own gain; each control
has a port on its top edge, quiet until a cable ends at it. An audio cable is grey, a
modulation cable magenta, dashed and thicker the deeper it is; a cable that would end at a
modulator's rate, stage or depth has no handle yet and is not drawn. The canvas is laid out
again when the structure of the session changes (`sameStructure`: nodes, their places in the
tree, names, the parameters of their plugins, cables) and not for a value. Nodes cannot be
moved, selected or cabled by hand: that is for the steps that persist layout and edit the
graph. React Flow gives a node that can be neither dragged nor selected
`pointer-events: none`, which put its pane over every knob; `patch.css` takes the pointer
back for the nodes (a browser found that; jsdom has no CSS).

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
draws a live value: a knob's dot, a modulator's bar, the master's meter and CLIP light
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
   browser reflect each other's changes.
1. **Session parity:** outline and inspector, chain editing, presets, pickers,
   save, undo / redo, off-thread plugin loads, parameter metadata for the
   CLAP / VST3 / LV2 backends. *Exit:* build a session in the browser and load
   it in the TUI unchanged.
2. **Piano parity:** scale / lock, held notes, chords, BPM. *Exit:* play from
   the browser keyboard; held notes and chords match the TUI.
3. **Modulation, groups, patterns, MIDI-learn on the canvas.** *Exit:*
   everything the TUI's `t` / `m` / `g` / `l` flows do, by drawing cables.
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
    produce them.
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
  - **The golden JSON** of the snapshot of `examples/tracer.toml` (a saw
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
    (f25). And a gesture's end could send a second intent in a frame that had sent one (f26,
    §9.4): `commit` now waits for the next frame when this control has already sent in this
    one.
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
    settings are shown but not editable (there is no intent for them yet). The modulation
    cable crosses the header of the node it drives, because its port is on the knob below
    the header.
  - **Dev loop additions.** `?engine=mock&scene=stress` serves the big session from the mock.
    `npm run e2e` runs the browser tests one at a time (they share the real program's one
    session, and each puts the filter back as the example has it); the commit count needs the
    Vite dev server, which is what Playwright starts.
- [ ] **13. TUI and web together.** `tang --web[=addr]`: the TUI loop reads
  terminal events on a reader thread and `select!`s over them, web requests,
  controller notifications and a tick. Changes in either front-end show up in
  the other. *Done when:* a manual parity check passes both ways and a test
  drives both paths against one controller.

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
| D8 | Telemetry via lock-free slots, binary frames; undo via inverse intents + recovered plugin instances | Proposed |
| D9 | Short design + TODOs in `CLAUDE.md`, long form here | Assumed |
| D10 | `tang serve` listens on `127.0.0.1:8765` by default; beyond loopback `--web-bind` and a token made for the run | Built (step 10) |
| D11 | One current-thread tokio runtime on its own thread for the server and the pusher; `ctrlc` for stopping a headless process | Built (step 10) |
| D12 | The client's store is vanilla zustand (no React in `client/`), the mock engine speaks the protocol through the same `Transport` as a socket, and the fixture the mock serves is generated from the Rust types and kept current by a Rust test | Built (step 11) |
| D13 | `web/dist` is embedded with `rust-embed`, with a committed `.gitkeep` so that the program builds without Node; the interface's content security policy has no `unsafe-*` | Built (step 11) |
| D14 | Telemetry never passes through React: one frame loop, readers that find their slot once for each layout, DOM writes only when a value moved; controls are controlled components, and what a control shows while the server catches up, and the one intent a frame, are the `Drive`'s | Built (step 12) |
| D15 | React Flow draws the canvas; `toFlow` is a pure function of the graph, nodes are functions of their data with controls that read their own values, and the canvas is laid out again only when the structure of the session changes | Built (step 12) |

Open questions

1. Display text for external plugin parameters (§4.3).
2. Whether the `web` feature joins the Windows CI matrix. (It does, as of step 10,
   with `--no-default-features --features vst3,web`; the first run of that will say
   whether it builds there.)
3. Worker-thread plugin loading (phase 1): completion messages, cancellation,
   what the TUI shows while a load is pending.
4. Session browser and runtime session switching (phase 4): tearing down and
   rebuilding the graph while the stream runs.
