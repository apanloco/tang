# Track B: a patchable synth (`builtin:patch`)

**Status: a proposal. Nothing of it is built, and none of it will be started before you have decided what §8 asks (the first
part, B0, changes how `builtin:osc` sounds, so it waits too).**
Written 2026-10-15 at the end of step 38 of `docs/web-ui.md`, from the code as it is then. §13 of that document sketches the idea in
a paragraph; this one says what the engine has and lacks, what I would build in what order, and which decisions are yours.

## 1. What it would be, and what it would give you

Today a Tang sound is an instrument (a plugin) followed by effects, with modulators that move knobs. The page draws that as strips of
nodes with cables, and the cables are *modulation*: an LFO, an envelope or a MIDI control sets a parameter, once per audio buffer.
The built-ins are a one-oscillator instrument (`builtin:osc`), a filter and a reverb, so a rich sound needs an installed plugin.

Track B is an instrument whose **inside you draw**: oscillators, filters, envelopes, LFOs, amplifiers and mixers, wired with cables
that carry audio-rate signals, one copy of the graph for every note that sounds. To the host it is one more plugin
(`builtin:patch`), so lanes, groups, effects, lane modulators, the piano, Undo and the terminal keep working as they do.

What it makes possible, and the engine cannot do today:

- **Audio-rate modulation**: FM and ring modulation, an oscillator that sweeps another's pitch, a filter that an envelope opens
  within one note. Lane modulators run once per buffer (256 frames on macOS, 1024 on Linux: 5 to 21 ms at 48 kHz), which is too
  slow for any of it.
- **A modulator per note**: an envelope or LFO that belongs to a voice, so two notes of a chord have each their own.
- **Per-note expression** from a controller such as your Seaboard. `CLAUDE.md` says it plainly under "MIDI routing": expression on
  channels 2 to 16 reaches the lane that is sounding that note, but modulators are lane-global, so a chord collapses to one value
  ("true per-voice expression would need voice-level modulation"). A patch is voice-level modulation.
- **Sounds built from nothing**: drums (noise and an envelope), basses, pads (several detuned oscillators), without a plugin.

What it does not change: the page's lane strip, the effects after the instrument (a patch is the *voice*; the reverb and
any other effect stay in the lane's chain, where they already are), and the terminal's way of showing and turning parameters.

## 2. What the engine has, and what it lacks

Read from the code, not from memory:

- **The plugin contract** (`src/plugin/mod.rs`): `Plugin::process(midi_events, audio_in, audio_out)` renders a block. MIDI arrives as
  `(frame offset, [u8; 3])`, so note-ons are sample-accurate inside the block. Parameters are listed by `parameters()`
  (`ParameterInfo`: index, name, range, default, enum labels, unit, group, step count), read and written by index, and
  `set_parameter` is called between blocks.
- **Parameter indices are the plugin's own**, and the model keeps them apart from row positions: cables, pins and `SetParam` name a
  parameter by its index, and a test gives a row the index 42 to show that the position is not it
  (`set_param_names_the_plugins_own_parameter_index`; the terminal passes `param.index` on). The CLAP adapter numbers its
  parameters by position and maps them to CLAP's ids itself, so no plugin has indices that are not 0..n today: a patch would be the
  first, which the model is written for and B2 has to test end to end, in the page and the terminal too.
- **Lane modulators** tick once per buffer and write through `set_parameter`. A patch's inner envelope and LFO would run per
  sample; the lane's would still work on the patch's parameters, at block rate.
- **No allocation on the audio thread.** `alloc_guard` (test builds) counts a thread's allocations, and the audio graph's `process`
  has tests that it allocates nothing once it is running (a lane with a constant instrument, an effect and modulators). A patch
  engine has to meet the same test. (`builtin:osc` did not meet it until `1edde0c`: see the next point.)
- **Hot swap exists**: plugins, and since step 34 whole lanes, travel to and from the audio thread over bounded channels, and the
  controller waits for a thing to come back before it drops it. A compiled patch would go the same way.
- **The built-in DSP is not reusable as it stands.** `builtin:osc` keeps its voices in a `HashMap<u8, Voice>` made with
  `HashMap::new()`, so a note-on that took a new entry allocated on the audio thread (found while writing this, and fixed in
  `1edde0c`: the map has room for every key now; the allocation tests of the audio graph use a constant instrument, so the
  oscillator has a test of its own), and its saw and square are naive, which alias. `builtin:filter` has four good models
  (state-variable, ladder, acid, MS-20, measured against a harness), but the models and the smoothing of the parameters sit in
  one stereo `process` of about 160 lines, not in mono per-sample kernels that a voice could own. Both need refactoring before a
  patch node can use them (§7, B0).
- **The page** draws what `SessionModel::graph()` projects (nodes, ports, edges), and every control is keyed by a plugin parameter
  index: knobs, pins, the `~` modulate menu, the ⚙ panel, Undo. That is the part to keep (§3).
- **Telemetry** is a fixed array of 4,096 atomic slots that the model hands out, never reusing one. A patch node that shows a live
  value would take a slot, so a patch of 30 nodes is 30 of them.

## 3. The central idea: every inner parameter is a host parameter; only the structure is new

A patch node (an oscillator, a filter, an envelope) has parameters like any plugin's. If the patch plugin lists **every node's
parameters as its own**, with an index that stays with the node (say `node serial × 256 + the parameter's place`, sparse and stable
as the nodes come and go), then nearly everything that exists works for patches without being touched:

- `SetParam`, `SetPinned`, the knobs, the ⚙ panel, presets of the whole patch, Undo of a turned knob;
- lane modulators and MIDI learn aimed at an inner parameter, with their rings on the knob;
- the terminal, which lists a plugin's parameters and turns them (a flat list that the `/` filter narrows by name: so each
  parameter's name has to carry its node's, `Osc 1: detune`; the ⚙ panel of the page is the same, and neither uses a parameter's
  `group` today);
- the session file's `params`.

What is new is **structure**: the nodes that are in the patch and the cables between them. Those need their own intents (add and
remove a node, draw and remove a cable), a place in the session file, and an inner view in the page. Nothing else in the controller
has to learn what a patch is.

## 4. The engine

**One signal type.** Every cable carries an audio-rate `f32` per sample, as a modular synthesizer's does. Pitch is in octaves
(or semitones: a convention to choose once), a gate is 0 or 1, an envelope is 0 to 1. The page can colour a cable by what it comes from
without the engine having types. (The alternative, typed cables for audio, control and gate, is safer to wire and costs more code and
more kinds of ports; §8 asks.)

**Nodes** (first set; each a small struct with fixed-size state and a `process` over a block):

| Node | Ports | Notes |
|---|---|---|
| `Note` | out: pitch, gate, velocity, pressure, bend | the voice's own notes; pressure and bend per voice from the note's MIDI channel, which is the per-note expression above |
| `Osc` | in: pitch, fm, pwm; out | sine, triangle, saw, square/pulse; PolyBLEP for saw and square; octave, semitone, fine, level |
| `Noise` | out | white and pink |
| `Filter` | in: signal, cutoff cv; out | the existing models as per-sample kernels; cutoff in octaves so a cable adds to it |
| `Amp` | in: signal, level; out | a VCA: a product, which is also a ring modulator |
| `Env` | in: gate; out | ADSR, per voice |
| `Lfo` | out | free-running or restarted on a note; rate in Hz or from a cable |
| `Mix` | in: 4; out | a sum with a level for each input |
| `Scale` | in; out | `a·x + b`, which turns an envelope into a pitch sweep |
| `Output` | in: left, right | where the voice ends; pan |

Later, in the order you want them: delay, sample and hold, slew, waveshaper, crossfade, comparator, a wavetable oscillator, a
one-sample delay that makes feedback legal.

**Voices.** A pool of a fixed number (8 or 16: a parameter of the patch, allocated at load) of voices, each with its own copy of the
graph's state. A note-on takes a free voice or steals the oldest; a note-off lowers the voice's gate; a voice is free when its
gate is down and the `Output` has been quiet (below −80 dB, say) for a moment. The graph is evaluated a voice at a time over the
whole block, so the cost is *voices × nodes × block*, and a block's loop is tight; how big that is has to be measured (§9).
Unison (several detuned oscillators in one voice), mono and legato modes come after.

**Compile.** Edits are made to a description of the patch on the main thread and **compiled** into an immutable program: nodes in an
evaluation order, one buffer for each cable (reused where lifetimes allow), the parameter table. A cable that closes a cycle is
refused unless it passes a one-sample delay node. The audio thread gets the program by the swap that plugins use, and returns the old
one. Whether a note that sounds survives an edit is a decision (§8, edits while playing).

**Quality.** Parameters are smoothed inside the nodes, as the built-ins do, since `set_parameter` is block-rate and would zipper;
anything that saturates (the filters' drive) is oversampled as today; oscillators are band-limited.

**Tests.** The same kinds as the filter's: a render of each node against a known value, no NaN or denormal in a long run, the
allocation count of `process` is zero (`alloc_guard`), voices are stolen and freed as they should be, and a patch built in a test
gives the same samples as the same patch loaded from a file.

### Build, or take a library?

FunDSP (`fundsp`, Rust, MIT or Apache-2.0, version 0.23.0 of 2026-01-07 on its crate page, 704 commits on its repository) is the one
to look at. From its documentation and README (docs.rs `fundsp::net::Net`; github.com/SamiPerttu/fundsp), **verified**: anti-aliased
oscillators (PolyBLEP, wavetable and DSF families), Simper state-variable filters and a Moog ladder, `adsr_live`, white, pink and brown
noise; a dynamic graph (`Net`) that is edited on a *frontend* and sent to a real-time-safe *backend* with `backend()` and `commit()`;
node replacement with a crossfade; cycles are detected.
**Not verified** (the pages I read do not say, and the API reference of 0.23.0 did not build on docs.rs, so what I read is the guide
text on the page of an older build): whether `commit()` allocates; whether nodes can be removed and cables disconnected, and how; what a voice costs; and how
a node's parameters would be exposed by a stable index (it has its own `Shared` variables, which are atomics). So the honest answer is
"possible, and worth a small experiment before deciding": the host-side parts of §3 (stable parameter indices, structure intents,
the model, the page) are ours either way, and what a library saves is the DSP of §4's table. §8 asks.

## 5. Model, file and intents

**Session file.** A patch lives in the instrument's table, beside `params` and `pattern`:

```toml
[[instrument]]
plugin = "builtin:patch"

[instrument.patch]
voices = 8

[[instrument.patch.node]]
name = "Note"
type = "note"

[[instrument.patch.node]]
name = "Osc 1"
type = "osc"
params = { waveform = 3, detune = 0.1 }
ui = { x = 300, y = 80 }

[[instrument.patch.node]]
name = "Env 1"
type = "env"

[[instrument.patch.cable]]
from = "Note.pitch"
to = "Osc 1.pitch"
```

Node names are unique in a patch and are what cables and parameter names use; the loader builds the program from this, and `plugin::load`
(which takes a source string only) gets a way to be given the definition (a variant for the patch, or a `configure` on the trait that
others ignore). Positions are `ui`, as for every node.

**Model and graph.** A patch node is a node of the graph whose parent is the lane's instrument node, with its parameters like a
plugin's (so §3 holds), and a new edge kind for a signal cable. `SessionModel::graph()` projects them; telemetry slots for the nodes
that show a live value come from the same allocator.

**Intents** (new): `AddPatchNode { parent, kind }`, `RemoveNode` for one, `AddSignalEdge { from, to }`, `RemoveEdge` for one,
`SetPatchVoices`. A refusal makes nothing and uses no id, as everywhere. **Undo** for the structure follows what steps 33 to 37 did
(inverses read from the model before the change, ids that come back as they were); I would ship the first editing step with
structure edits as barriers, as steps 32 and 33 started, and make them steps after.

## 6. The page and the terminal

**The inner view.** The instrument node of a patch has an "Open patch" button; the canvas then shows the patch, with the same node,
port, cable and knob components, a breadcrumb ("Lane 1 › Patch") and a way back. The lane strip never has to draw twenty nodes in
a row. (The alternative, nodes flattened into the strip, is in §8.) A palette adds nodes; a cable is dragged from an output to an
input as modulation cables are today; each node shows its knobs (pinned or the first eight, as now), and an `Env` or `Lfo` node
draws what it puts out, as a lane modulator's node does (`LiveTrace`, `LiveEnvelope`). The `~` button on an inner knob still makes a
*lane* modulator for it: two kinds of modulation, block-rate and lane-wide, and audio-rate and per-voice, side by side, which the
page has to explain by colour and name.

**The terminal** lists the patch's parameters, each named after its node, and turns them, with no work of its own. Drawing the graph in the
terminal is a different amount of work and I would not do it (§8): the terminal shows the structure as a read-only list.

## 7. Order of work

Each step is a few commits that leave the repository green, with tests first, as the others were.

- **B0, kernels first (worth doing whatever you decide).** Make the filter's four models and a band-limited oscillator into
  per-sample kernels with their own tests; `builtin:filter` and `builtin:osc` use them and sound the same (the filter's harness
  shows it). This is the refactoring `CLAUDE.md` asks me to tell you of before something new is built on them.
- **B1, the engine, no page.** `builtin:patch` with the nodes of §4, voices, the compile and the swap, loaded from a session file
  and played from the keyboard in `tang play`; measured: CPU for 16 voices of 20 nodes, allocations of `process`, a few patches as
  golden renders. This is where FunDSP would be tried against our own nodes. It ends with a decision on the numbers.
- **B2, the controller and the page read it.** Patch nodes and cables in the graph and the snapshot, parameters as host
  parameters, the inner view drawn and knobs turning (no editing of structure yet), the terminal listing; browser tests on a real
  patch.
- **B3, editing.** The intents, the palette, drag a cable, remove, rename; structure edits first as barriers of the history.
- **B4, Undo of the structure, more nodes, presets** (patches as files you can load into a lane), per-voice expression
  from the MPE channels, unison.

## 8. What I need from you

Each has my recommendation and what changes if you choose otherwise.

1. **Direction.** (a) more built-in instruments and effects first, in the rack the page already has (a delay, a chorus, a drive, a
   wavetable or FM oscillator): no new UI, an immediate wider palette; (b) the patch (B1 and on); (c) the kernels of B0, then a few
   built-ins on them, then the patch. *I recommend (c): B0 is needed by both, and the built-ins are what you can try by ear while B1 is
   built.* With (a) alone the sound grows but audio-rate modulation, per-voice envelopes and per-note expression never come.
2. **One signal type or typed cables.** *Recommend one* (audio rate everywhere), with colour by source. Typed cables stop a gate going
   into a pitch input and cost a port type, a rule table in the controller and more in the page.
3. **Voices from the start.** *Recommend yes*: per-voice state is the point of the patch, and bolting it on later changes every node.
   Eight voices by default, 16 at most to begin with, steal the oldest.
4. **Where the patch is drawn.** *Recommend the inner view* (§6). Flattening the nodes into the lane's strip keeps the one canvas but
   a patch of twenty nodes makes the strip enormous and the layout (`toFlow`) has to learn rows of rows.
5. **All inner parameters are host parameters** (§3). *Recommend yes.* The alternative, exposing a chosen few as macros, makes the
   rest unreachable for lane modulators, MIDI learn and the terminal, and doubles the model.
6. **Our own DSP, or FunDSP.** *Recommend deciding after the B1 experiment* (§4): the facts I could verify are good, and the ones I
   could not (allocation on commit, removing nodes, cost per voice, exposing a parameter by index) decide it.
7. **The terminal's part.** *Recommend a read-only list of the structure* and full control of the parameters, and editing the graph
   in the page only. If the terminal must edit the graph too, B3 is about as much work again.
8. **An edit while notes sound.** Options: the new program takes over and keeps the state of nodes that have the same name and kind
   (a note goes on through an edit that does not touch it), a short crossfade between the old and new program, or all voices
   released at the edit. *Recommend state kept by node name, released as the fallback.* The crossfade is the most robust and the
   most work (two programs run at once).
9. **Where a patch lives.** *Recommend the session file first* (§5); a library of patch files that any lane can load comes with B4.
10. **The name.** `builtin:patch`, or `builtin:modular`, or something of yours.

## 9. Risks, and what I would measure first

- **CPU.** Interpreting a graph for each voice costs more than the built-ins' straight loops. The block loop amortizes it, but the
  number is unknown until B1 measures it; the answer sets the voice limit and whether nodes need a fused fast path.
- **Denormals and NaN** in feedback and in filters: tested for in B1, and the oscillators and envelopes are written not to make them.
- **Canvas complexity.** Cables that cross in an inner view are harder to read than the strips: auto-layout, a placement that
  respects the cable direction (left to right), and a way to see a cable's signal are part of B2 and B3, not afterthoughts.
- **Undo of structure** is the largest piece of B4: journals that put a node back with its cables and its state, as for lanes.
- **Stable parameter indices** have to survive a node's removal and re-adding by Undo (ids come back, step 33), and files saved by
  an older build must load.
- **Names and sessions.** Node names are unique and are what files and parameters use; a rename has to rename the parameters too.
