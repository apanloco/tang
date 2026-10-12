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
  `play` does). Both are fixed in step 5.

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
    fn poll(&mut self) -> bool;          // notifications, catalog batches, loader
                                         // completions, reap returned plugins,
                                         // MIDI hot-plug; true if state changed
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

### 4.3 Parameter metadata

`ParameterInfo` is `index, name, min, max, default, labels`. Add, all optional:
`unit`, `group` (path), `step_count`, `flags` (hidden / read-only / stepped).
Builtins fill them in phase 0; the CLAP, VST3 and LV2 backends in phase 1 (VST3
units and step counts, CLAP module paths and flags, LV2 port groups and scale
points). Without them knobs read "0.37" instead of "1.2 kHz" and a big plugin's
parameters are one flat list.

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
  GroupPan }`, `LoadPreset`.
- Structure: `AddInstrument`, `ReplaceInstrument`, `AddEffect { parent }`,
  `RemoveNode`, `MoveEffect`, `SetRange`, `AssignGroup`, `AddGroup`,
  `RenameGroup`.
- Modulation: `AddModulator`, `SetModulatorField`, `AddModEdge`, `RemoveEdge`,
  `SetEdgeDepth`, `LearnMidi`.
- Patterns: `RecordPattern`, `ClearPattern`, `SetPatternField`.
- Globals: `SetBpm`, `SetScale`, `SetPianoMode`, `Save`, `SaveAs`, `Undo`,
  `Redo`.
- Layout: `SetNodeUi` (not undoable; marks the session dirty).

`apply` returns `Applied { events, undo }` or an `IntentError` (`NotFound`,
`Invalid`, `Busy`, `Io`, …). Every request carries a client `req_id` for the
ack/err reply. Note on/off bypasses the controller (§3.3).

Events carry a monotonically increasing `rev`: `ParamSet`, `NodeAdded`,
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

Telemetry publishes engine truth: modulator outputs (once per buffer, from
`Modulator::tick`), effective parameter values (the `clamp(base + Σ offsets)`
result written in `apply_modulators_to_chain` and the group equivalents), and
per-lane / group / master peak and clip. A scope ring can follow later.

- **Mechanism.** A preallocated `Telemetry { slots: Box<[AtomicU32]> }` (f32
  bits, relaxed ordering). The controller allocates slot numbers and passes
  them to the engine in the commands that create lanes, groups, modulators and
  targets, so index shifts never re-key a slot. The audio thread only does
  relaxed stores: no allocation, no locks. If slots run out, that item has no
  telemetry (logged); audio is unaffected.
- **Layout.** A `TelemetryLayout { rev, entries: [{ slot, node, port, kind }] }`
  event tells clients what each slot means. It changes only when the structure
  does.
- **Wire.** WebSocket binary frames: `u8 kind | u8 flags | u16 n |
  u32 layout_rev | n × f32`, where value *i* belongs to layout entry *i*. About
  30 Hz per subscribed channel group (`mod`, `meters`); nothing is sent when
  nobody subscribes. Held notes go as a 16-byte bitset, on change.
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
- The protocol is versioned (`protocol` integer in `Hello`); the client refuses
  a mismatch with a clear message.
- HTTP serves only the SPA and `/ws`; there is no REST API.

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

## 10. Testing and CI

- **TUI safety net first:** golden-render tests through ratatui's `TestBackend`
  (`insta`) before any refactoring starts.
- **Rust:** controller tests against a stub command receiver; golden session
  round trips; telemetry tests that drive `AudioGraph::process` without cpal (as
  the `chain.rs` tests already do); golden JSON for the protocol; a freshness
  check on the generated TS types; web integration tests (WebSocket client ↔
  controller ↔ null audio).
- **Null audio backend** (`--audio-device none`): a timer-driven `process()`
  thread, so `tang serve`, CI and end-to-end tests run without audio hardware.
- **Web:** Vitest for the client layer, Storybook for components, Playwright
  end-to-end against `tang serve --audio-device none`.
- **CI:** a new job (Linux first, macOS later) with Node: `npm ci`, build,
  `npm test`, then `cargo clippy` / `cargo test --features web`. The existing
  matrix, including Windows' `--no-default-features --features vst3`, is
  untouched; whether `web` joins it is open.

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

- [ ] **1. TUI safety net.** Make `render` generic over `Backend`; extract
  `State::new` (used by `run` and by tests); add `insta` golden-render tests
  (`TestBackend`) for `examples/split.toml` (chain + parameter pane), a selected
  modulator, a group with a bus effect and a group modulator, an open selector
  popup, and the Piano tab. Fixtures are built by hand from builtin plugins until
  step 3 provides the real loader. *Done when:* snapshots are committed and
  `cargo test` is green, with no behavior change.
- [ ] **2. Extract the model** into `src/app/model.rs` (not `core`: a module
  named `core` shadows the std crate): `PluginSlot`, `ParamSlot`, `ParamKind`,
  `PatternState`, `InstrumentNode`, `GroupNode`, `ModSourceSlot`,
  `ModulatorSlot`, `ModTargetSlot`, plus the pure helpers
  (`default_effect_mix`, `format_from_id`, the cross-mod / group-ordinal
  fix-ups, `tui_lane_member_ordinal`, `mod_source_slot_to_graph`,
  `to_plugin_slot`, `to_modulator_slot`, `modulator_param_*`). The TUI imports
  them. *Done when:* snapshots unchanged; new unit tests cover the fix-up
  helpers (they have none today).
- [ ] **3. Extract load** into `app::load::load_session`: the group, instrument,
  effect, pattern and transpose loading from `run_session`, plus
  `load_lane_modulators`, `load_group_modulators`, `build_mod_source`,
  `member_mod_field_info`. The `tui::Loaded*` structs disappear (the loader
  returns the model). `run_session` becomes wiring (channels, engine, splash,
  mode switch); the splash reports through a progress callback; `play` uses the
  same loader. *Done when:* `examples/basic.toml` and `split.toml` load against
  a stub command receiver in tests (asserting the model and the command
  stream); snapshots, now fed by the real loader, unchanged.
- [ ] **4. Extract save** into `Session::to_save()` (the body of
  `State::save_session_to`); `session::save` stays the file writer. *Done when:*
  load → save → load round-trips pass for the examples, groups, modulators and
  MIDI-learn.
- [ ] **5. Controller v0.** `app::Controller` takes ownership of the session,
  `cmd_tx`, runtime, catalog (and its scan thread), session path, dirty flag,
  the shared `Arc`s, the notification receivers and the `MidiManager`; `State`
  holds a `Controller`. `Controller::poll()` runs every TUI iteration: it drains
  pattern / learn / catalog **and reaps `return_rx` and polls MIDI hot-plug**
  (the two lifecycle bugs in §2). Moves the cursor-free methods
  (`save_session*`, `load_plugin_slot`, `extend_catalog`). *Done when:*
  snapshots unchanged; a test shows `poll()` reaps returned plugins; hot-plug
  verified by hand (connect a controller while the TUI runs).
- [ ] **6. Intents v0.** `NodeId`s on model nodes and TUI tree entries;
  `Intent` / `Event` / `Applied` / `IntentError` and `rev`; first family
  `SetParam`, `SetGain` (volume, mix, group volume, pan) and `LoadPreset`. The
  TUI's parameter and gain edit paths resolve cursor → ids and call `apply`;
  the clamping and enum rounding in `adjust_param` / `set_param_value` /
  `set_group_bus_param`, and the preset-value mirroring, move into the
  controller. *Done when:* tests cover each intent (model, commands, events,
  invalid ids); snapshots unchanged; param tweaks and preset loads behave as
  before in a manual run.
- [ ] **7. Engine telemetry.** `Telemetry` (atomic slots); slot ids carried by
  `AddInstrument`, `AddGroup`, `InsertModulator`, `InsertGroupModulator`,
  `AddModTarget`, `AddGroupModTarget`; the audio thread publishes modulator
  outputs, effective values and per-lane / group / master peak and clip. The TUI
  gets the `CLIP` indicator from `CLAUDE.md` as the first consumer, which
  validates the plumbing and closes a TODO. *Done when:* `chain.rs` tests drive
  `process()` and assert slot values (LFO + target, envelope, MIDI-learn, group
  modulator writes), and the audio path gains no allocation or locking.
- [ ] **8. Graph projection and protocol types.** `Session::graph()`; serde
  types for `Snapshot`, `Event`, `Intent`, `TelemetryLayout`; the
  `ParameterInfo` v2 fields filled in for builtins; `ts-rs` export (feature
  `web`) into `web/src/protocol/generated/`. *Done when:* a golden-JSON test for
  a known session passes and the TS freshness check is wired up.
- [ ] **9. Null audio backend.** `--audio-device none` starts a timer-driven
  thread that calls `AudioGraph::process` at the buffer cadence; shared by
  `serve`, tests and CI. *Done when:* the engine runs on a machine without an
  audio device, and a test pumps notes through it and reads telemetry.
- [ ] **10. `web` feature and `tang serve`.** The axum / tokio runtime; `/`
  (embedded placeholder page) and `/ws`; `Hello`, `Origin` / `Host` checks,
  token for non-loopback; hub fan-out with bounded per-client queues; snapshot
  on subscribe; requests into the owner loop (headless loop = `select!` over
  intents, controller notifications and a tick); the telemetry pusher;
  `NoteInput` (adopted by `MidiManager` and `VirtualPiano` too) with
  per-connection note-off on disconnect. *Done when:* integration tests
  (WebSocket client ↔ controller ↔ null audio) cover connect → snapshot →
  `SetParam` → event + engine-side effect, `Origin` rejection, slow-client
  eviction and stuck-note release.
- [ ] **11. `web/` skeleton and CI.** Vite + React + strict TypeScript +
  ESLint; the `client` layer (connection, store, reducers, telemetry buffers)
  and the mock engine; `ui` tokens; Storybook; Vitest; the Playwright harness;
  `.gitignore` entries; a Linux CI job that builds `web/`, runs the web tests,
  then `cargo clippy` / `cargo test --features web`; `rust-embed` wiring.
  *Done when:* CI is green, and `npm run dev` against both the mock engine and
  `tang serve` shows the connection state and the raw session.
- [ ] **12. Tracer UI.** `examples/tracer.toml` (saw oscillator → filter, LFO →
  cutoff); a canvas with the lane strip, a modulator node and a cable; a `Knob`
  with value / ring / dot; telemetry-driven animation outside React's render
  path; parameter drag → `SetParam`; Playwright perf test v0. *Done when:*
  dragging the cutoff knob changes the sound, the LFO visibly drives the knob in
  sync with what you hear, and the perf test enforces the §9.4 budgets for that
  scene.
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

Open questions

1. Display text for external plugin parameters (§4.3).
2. Whether the `web` feature joins the Windows CI matrix.
3. Worker-thread plugin loading (phase 1): completion messages, cancellation,
   what the TUI shows while a load is pending.
4. Session browser and runtime session switching (phase 4): tearing down and
   rebuilding the graph while the stream runs.
