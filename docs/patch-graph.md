# A patch graph for the session: audio and MIDI as cables

**Status: §11 is the plan. You said go on 2026-10-20 (*"lets go e2! I like this. I like it all!"*), which takes the recommendations of §11.7 as they are. P0, the oracle and the boundary (§11.6), is built (step 70 of `docs/web-ui.md`), and so is P1, the kernel, with today's lanes and groups running on it bit for bit (step 71); nothing else is.**
Written 2026-10-20 at the end of step 67 of `docs/web-ui.md`, from the code as it is then. **§11, added later the same day, is the design without a time limit
(you asked for the best way, whatever it costs to build) and replaces the recommendation of §4 and §9.1: the destination is a compiled graph, and the routing
table of E1 is a detour.** §1 to §10 are what I proposed before that, kept because the model, the file, the page and the terminal parts of them carry over. It starts from what you said that day: *"something we need
to think about sooner or later, is to have inputs and output, and have the connections modifyable. An example would be to connect two different osc to
the same reverb. I also think we should treat midi this way. I.e more modular in general."* `docs/web-ui.md` §13 ("Sources as nodes") has your two
earlier ideas (a keyboard as a node with MIDI cables, and audio input as a source), which this one contains. It is not track B (`docs/track-b.md`):
that is the voice *inside* one plugin, and this is how the plugins, the buses and the keyboards of the *session* are wired to each other.

## 1. What it would be, and what it would give you

Today the audio path of a Tang session is fixed by its structure: a lane is an instrument followed by a chain of effects, a group sums lanes and has a
chain of its own, and everything ends at the master. The notes go everywhere: every lane hears every key that is in its range. The only things you can
wire are modulation cables.

A patch graph makes the rest wireable. Every node has inputs and outputs, and the connections between them are **cables** that you draw, move and take
out, of three kinds: audio, MIDI and the modulation that exists. What that makes possible, and the session cannot do today:

- **Two oscillators into one reverb**: a cable from each oscillator's output to the reverb's input. (Today: both lanes in a group and the reverb in the
  group's bus. That works, but it is a membership that you choose from a list, and not a cable.)
- **Sends**: one lane feeding a dry path and a wet one with a level for each; a bus that feeds another bus (a reverb bus into a limiter bus).
- **Several keyboards and several instruments**: a split is two cables from one keyboard to two lanes, a layer is the same with the same keys, two
  keyboards into one synth is two cables into it. A keyboard is the computer's keys, the on-screen piano, or one of the MIDI devices that are plugged in.
- **Things between**: a pattern, an arpeggiator or a transposer as a node between the keys and the synth (the pattern recorder is inside every lane
  today).
- **Later**: an audio input as a source (`CLAUDE.md`, "Future ideas"; the user asked for it on 2026-10-28 for after step 96: "for future (after this) I want to be able to add my computer microphone as an input that I can connect to other devices via audio cable.", which is a source node with an output of audio and a cable from it, as P4's sources of notes are, and a first sketch is in `docs/web-ui.md` §13), and more than one output.

What it does not change: the modulation cables, the plugins, the lanes and groups that you see (a lane is a path in the graph, and a session that draws
no new cable is saved and shown as it is today), the terminal's tree, Undo.

## 2. What the session is today, and what is already a graph

Read from the code, not from memory:

- **Audio.** `AudioGraph::process` (`src/plugin/chain.rs`) runs each lane (its notes, its instrument, its volume, its effects) into one place, the
  master mix or its group's accumulator (`InstrumentLane.group: Option<usize>`), and then each group (the sum, its volume, its bus effects, its pan) into
  the master mix. A lane is in one group or none, a group feeds the master only, and groups do not nest: two levels, in two loops.
- **MIDI.** `NoteInput` (`src/note_input.rs`) is the one path that a hardware port, the virtual piano and a browser send notes by. It has one channel to
  the audio thread, and an event is `MidiEvent = (u64, [u8; 3])`: a frame and three bytes, with no source. The renderer drains the channel at the start
  of each block and hands the same slice to every lane, and the lane filters it by its range (`filter_midi`), remaps, runs its pattern, transposes,
  ticks its modulators and renders. The MPE routing by channel is inside that.
- **The model already says "graph".** `SessionModel::graph()` (`src/app/graph.rs`) projects the session into nodes with stable ids and ports
  (`Port::AudioIn`, `AudioOut`, `ModOut`, the parameters) and edges, and the page draws them. Two of the three kinds of edge are **derived**:
  `EdgeKind::Audio` ("there is nothing to edit") and `EdgeKind::Member`. Only the modulation edges have ids (`EdgeKind::Modulation { id, amount }`) and
  intents (`AddModEdge`, `RemoveEdge`, `SetEdgeDepth`). The page draws the audio edges as cables between `audioOut` and `audioIn` handles, and the handles
  are `isConnectable={false}` (`app/patch/nodes.tsx`). So the model, the protocol and the canvas have the shapes; what is missing is that the audio and
  the MIDI edges are things a person makes.
- **What is addressed by position.** Most of the engine's 68 `GraphCommand`s name a lane by its index and a plugin by its slot. The history's journals
  (`Inverse::Lane`, `Group`, `Effect`) hold the places that things had, and the file's `group = 0` is an index. This is what makes a free graph in the
  engine expensive (§4, E2).
- **What is not there.** A cycle check for audio (nothing can make one), a latency report in the plugin trait (the Drive's 23 frames are not made up
  for, which is why it starts fully wet), a source on a MIDI event, and a cable that has a gain.

## 3. The central idea: one kind of cable, three kinds of signal

A cable is `{ id, kind, from: (node, port), to: (node, port), … }`, and `kind` says what travels (an audio cable has a gain, a MIDI cable later a filter,
a modulation cable its depth, as now). The three share what a cable needs: ids that survive removals and come back with Undo, the intents that add and
remove them, the way the file names the nodes at its ends, the way the page draws and drags them (the handles for audio are on the nodes already) and the
way the terminal lists them.

The structure that exists becomes cables that the model derives for a session that draws none: a lane's end to the master (or to its group), the links of
a chain, the keys to every lane. **A file that is written today loads to the same cables, and a session that never draws a new one is saved as it is
written today** (`group = 0`, a lane's `range`). Nothing in the program has to be thrown away to start: the cables are added around what is there.

Track B's inner graph would use the same: its typed cables (audio, control, gate: `docs/track-b.md` §8, decision 2) are this model one level down, so
building this first decides the node, port and cable shapes that track B would otherwise decide alone. Neither waits for the other.

## 4. The engine: audio

**E1, a routing table on the lanes and the groups (what I recommend first).** Keep `InstrumentLane` and `Group`. Give each an `outputs` list in place of
`group: Option<usize>` (and the group's implicit master): `(target, gain)`, where a target is the master, a group's input, or the input of one effect
slot of a lane's or a group's chain. Then:

- The nodes run in an order that has each one after those that feed it. The controller sorts them when a cable is added or removed and **refuses a
  cycle before anything is sent** (`a cable there would make a loop`); the order goes to the audio thread with the change, so nothing allocates there.
- Each lane keeps its output for the block (`lane_buf` is shared and overwritten by the next lane today). A cable adds the source's output, times its
  gain, to its target: to the master mix, to a group's accumulator, or to the signal that is at an effect's input (for slot k, what comes out of slot
  k-1, or the instrument for slot 0). That is a sum at an input, and it is the only "mixer" there is.
- A cable's gain is gone to as the mute's is (`fade`: a linear ramp over the buffer), so connecting, disconnecting and turning it do not click.
- Mute, solo, the lane's meter and probes and its modulators stay the lane's and stay where they are. Muting a lane silences every cable out of it, which
  is what a person means.

The change is in `InstrumentLane::process`, in the two loops of `AudioGraph::process`, and in the engine side of `SetLaneGroup`; the other sixty-odd
commands, the plugins and the telemetry do not move. It is two steps (§8): cables to groups and the master (C1a), then cables into the input of an
effect (C1b), which is the literal "two oscillators into the same reverb": a cable from the second oscillator's output onto the reverb that is in the
first one's chain.

What E1 cannot do: an effect that belongs to no lane and no group (a group with a reverb in it is how it looks, and is a bus that has a name, a volume
and a pan); an effect with cables out to two places (a lane's cables leave from its end, or from a tap after an effect: decision 3); a chain that splits
and joins again inside one lane. Those want E2.

**E2, a compiled graph of nodes (only if E1 leaves a need).** The lane and the group dissolve into nodes (an instrument, an effect, a mixer with a volume
and a pan, the master), and the engine runs a plan that was compiled on the main thread and swapped in: the nodes in order, a buffer for each cable end,
the sum at each input. It is what the word *modular* means, and the price is high. The lane carries a range, a remap, a pattern, a transpose, a volume,
mute and solo, a meter, probe taps and modulators, and each has to become a property of a node, or a MIDI node in front of it. The 68 commands would
name nodes and not positions. Every journal of the history (a lane, a group and an effect are put back whole from what the audio thread hands back)
would be written again. The law of mute and solo, which the page pins to the engine over 4,096 cases, would be stated for a graph. The engine proper is
the first 5,300 lines of `chain.rs`, with 7,900 of tests round it. **I would not start there**, and E1 does not close it off: the cables, ports, ids,
intents, file and drawing that E1 needs are all that E2 would ask of the model and the page. *(That was written for a clock. §11 takes the clock out and
reaches the opposite answer for the engine: build E2, by the path of §11.6, and skip E1's routing table, which would be thrown away.)*

**What neither does at first**: feedback (a cycle is refused), and latency compensation. Two paths of different latency that are mixed again make a comb
filter; the plugin trait has no latency report, so the host cannot make it up, and the Drive already has the problem in a smaller way. I would write it
down as a limit and measure it when a plugin reports (decision 5).

## 5. The engine: MIDI

Today `NoteInput` puts `(frame, bytes)` on one channel and every lane takes what it likes. The change is a **source** on each event and a **mask** on each
lane:

- A source is a number (up to 64, so that a mask is a `u64`). The computer's keys and the browsers' keys and piano are one source; each hardware input is
  a source of its own (the program opens all of them by default, and `--midi-device` one; each is already a thread that holds its own `NoteInput`). A
  `NoteInput` is made for a source, and the channel carries `(source, frame, bytes)`. The Locked mode and the held notes that the piano shows stay common to
  every source, as they are today: the sources go in by one path, and the tag adds to it and takes nothing away.
- A lane has `sources: u64`, and its first step drops what its mask does not have, before `filter_midi`. The plugin trait does **not** change: it
  still takes `&[(u64, [u8; 3])]`, since the lane builds that list itself.
- The default is every source, which is today's behavior; a lane's mask is what its MIDI cables say once a session has any. A panic reaches every lane,
  whatever the masks are. The held notes that the piano's keys light for stay one set, for every source; the lane's own `active_note_channels` (the MPE
  routing) stay where they are, since they are what the lane has been sent.
- Range, remap, transpose and the pattern stay the lane's in C2 (they are what the lane does with the notes it hears). Later (C3) a range and a
  transpose can be properties of the cable, and the pattern a node between the keys and the instrument; that is a move of code that already works, and I
  would not make it before the cables are there.
- The modulators that follow notes (a velocity, a key, an envelope, a step sequencer that restarts) follow the notes of their lane today. They would get a
  MIDI input of their own, wired by default to what feeds their lane's instrument. In C2 that stays derived.

## 6. Model, file and intents

**Model and graph.** `Edge` gets what a cable needs: `EdgeKind::Audio` and `Member` get an id and a gain and stop being derived; a node kind for a keyboard
(`NodeKind::Keyboard { source }`, with `Port::MidiOut`) and `Port::MidiIn` on an instrument. Edge ids come from the one allocator as the modulation
cables' do, and come back under their ids with Undo (step 33's rule).

**Where a cable attaches in a chain.** The projection gives every effect an `audioIn` and an `audioOut` already. A lane's output is its last effect's
`audioOut` (the instrument's, when it has none). A cable *into* an effect's `audioIn` is C1b's. A cable *from* an effect that is not the last is a tap
(decision 3).

**The file** says what it says today for what it can say, and has new tables for the rest. Nodes at the ends of a cable are named by what they are
(`instrument.0`, `group.1`, `instrument.0.effect.2`, `keyboard.0`, `master`), the way a cross-mod target names a modulator by its place:

```toml
[[instrument]]
plugin = "builtin:osc"            # lane 0

[[instrument]]
plugin = "builtin:osc"            # lane 1

[[group]]
name = "Verb"
[[group.effect]]
plugin = "builtin:reverb"

[[cable]]                          # audio: lane 0 into the group's input, at 0.6
kind = "audio"
from = "instrument.0"
to = "group.0"
gain = 0.6

[[cable]]
kind = "audio"
from = "instrument.1"
to = "group.0"                     # (or to = "instrument.0.effect.0": into an effect's input, C1b)

[[keyboard]]                       # C2
name = "Keys"
source = "computer"                # or "device:Seaboard"

[[cable]]
kind = "midi"
from = "keyboard.0"
to = "instrument.0"
```

`group = 0` on a lane stays: it is exactly one audio cable from that lane to that group and none to the master, and is written when that is all the lane
has. A session that has no `[[keyboard]]` has the implicit one that every lane hears. The loader reads both forms into the same model.

**Intents** (new): `AddCable { kind, from, to, gain }` (refused, with nothing changed and no id used, for a cycle, a port that is not there, a kind that
does not fit the ports, a cable that is there already), `RemoveEdge` (extended from modulation cables to all three), `SetEdgeGain`, and
`AddNode { kind: keyboard }` / `RemoveNode`. `SetLaneGroup` stays, as a shortcut that takes out the lane's cable to its old group and adds one to the new
(the page's "Group" cell and the terminal's `g`). **Events and the snapshot**: each intent has its event, and `Snapshot::apply` learns each of them. Step 67
found a hole of that kind (f167: an LFO's retrigger had an event that the Rust copy of what the browsers are told did not know), so the first test of
every step here applies every intent's events to a snapshot and compares it with the session. **Undo**: a cable's inverse is the other intent, and the ids
come back as step 33 did it; the work is that the journals of a lane, a group and an effect also hold the cables that point at them, as they hold the
modulation cables now.

## 7. The page and the terminal

**The page.** A cable is dragged from a node's output handle to an input handle as a modulation cable is dragged to a knob's: the handles that take it
are marked while it is dragged (`tang-port--takes`), a drop where it does not belong draws nothing, and the controller's rule (a cycle, a kind that does
not fit) is pinned to the page's by a fixture, as `reach.json` pins the modulation cables'. An audio cable has a gain, as a modulation cable has a
depth: the same slider, on the cable. The groups are nodes that you see already; a group with a reverb in it is "the reverb" that two oscillators are
wired to. A keyboard is a node with a MIDI output (its name, its source, a light for what it is playing, from the `held` channel that the piano
uses). The strips (`toFlow`) lay a lane out in a row with its modulators above it; cables between strips are routed round the nodes by the same
`cableRoute` that does it for the depth cables, and "Arrange" lays the groups to the right of the lanes that feed them. A node's place is the session's
(`ui`), as now.

**The terminal** cannot draw a graph and I would not make it try. Each node that has cables lists them in the tree under itself (`→ Verb  0.60`,
`← Keys`), and a key that is free in the Session tab (`c`, checked against the key table) opens that node's cables: a list of what comes in and what goes
out, with the keys to add one (a popup that lists what can be wired to it, as `g` lists the groups), take one out and turn its gain. It reads and edits
the same model, so what is drawn in the page is in the list and the other way round (`tang --web` runs both on one controller).

## 8. Order of work

Each step is a few commits that leave the repository green, with tests first, as the others were. The first commit of each is the test that says what
the step is for, seen red.

- **C1a, audio cables to groups and the master.** The model and the file (`[[cable]]`, `group = n` as the same thing), the intents, the engine's outputs
  and order (E1), groups into groups, the page's drag, the terminal's `c` list, Undo. *Done when:* two lanes are wired into one reverb bus from the page
  and from the terminal, with a gain each; a loop is refused; the master's meter hears it, and a test renders it through the real engine; the file
  says it and loads to the same graph; Undo and Redo put the cables back; and a session from before loads and saves as it was. *What you can try:*
  "+ Group", a reverb in it, and a cable from each oscillator.
- **C1b, a cable into the input of an effect.** The engine injects a lane's output into another lane's chain at a slot, with the lanes sorted by who feeds
  whom. *Done when:* the second oscillator's cable ends on the reverb in the first one's chain, and it is heard; mute and solo of the source silence the
  cable, and a test says so.
- **C2, MIDI cables.** A source on the event and a mask on the lane, keyboard nodes (the computer, and a device for each input that is plugged in), the
  implicit keyboard for a session that has none, the intents, the page's keyboard node, the terminal's list. *Done when:* a split, a layer and two
  keyboards into one synth are built from cables, and a key of one keyboard is heard only by what it is wired to; a session from before sounds as it did.
- **C3, nodes between.** The pattern as a node, an arpeggiator, a transposer, a range and a transpose as properties of a cable, MIDI inputs on modulators.
  This is where the pattern recorder leaves the lane.
- **C4, only if wanted: the general engine** (E2), with the parallel effect paths, splits and joins and latency compensation that E1 cannot do.

C1 and C2 are independent of each other: the first is the audio the example is about, the second is what you said of MIDI; either can go first.

## 9. What I need from you

Each has my recommendation and what changes if you choose otherwise. *(§11.7 gives the twelve again for the design without a clock, with six more.)*

1. **How far.** (a) E1 first, and decide on E2 when you have used it; (b) straight to E2. *I recommend (a):* it gives the example in two steps with
   everything else in the program as it is, and what it builds (cables, ports, ids, intents, file, drawing, Undo of cables) is what E2 needs from the model
   and the page. (b) is one long step with nothing to try until it ends.
2. **MIDI before audio, or after.** *Recommend audio first* (C1), since "two oscillators into one reverb" was your example and it touches the engine less
   than the source tags do; but C2 can go first without any cost to C1.
3. **Where a cable leaves a lane.** (a) from the end of its chain only; (b) from any effect's output, a tap (a send after the filter and before the
   reverb). *Recommend (a), and (b) in C1b* where the engine has to know the slots anyway.
4. **Cycles.** *Refuse them* when the cable is drawn (the page marks what cannot take it). A cycle that is let through needs a block of delay somewhere and a
   rule about where, and a feedback patch is a different feature.
5. **Latency compensation.** *Not in C1 to C3*, written down as a limit with the Drive's 23 frames as the example. It needs a latency report in the plugin
   trait (CLAP, VST3 and LV2 have one; the built-ins would say theirs) and delay lines on the short paths. A comb filter between a dry path and a path
   through a plugin that has latency is the known cost until then.
6. **Is a cable into a group a membership?** Today the group's modulators reach the lanes that are its members, and a lane is in one group. *Recommend
   that a cable from a lane into a group makes the lane a member for that reach, and that a lane can be a member of several*: one rule, and `group = n`
   is the case of one. The other way (sends that are not memberships) gives two kinds of link into a group, which the page would have to tell apart.
7. **Fan-in and levels.** *Recommend a plain sum at an input, with a gain on each cable* (0 to 2, a send level), and no mixer node: a group is the mixer
   (volume, pan, effects). Sums can pass full scale; the `CLIP` light already says so.
8. **The law of mute and solo for routed audio.** Today (`CLAUDE.md`, "Mute and solo") a soloed lane, a lane in a soloed group and a group with a soloed
   lane in it are heard. For cables: *a node is heard if it is not muted and a non-muted path leads from it to the master, and while anything is soloed
   only the soloed nodes and what is downstream of them are heard, each carrying only what comes from the soloed ones.* It is the same law written for a
   graph; the page's copy of it, which a fixture pins to the engine, is written again with it.
9. **Old sessions.** *Recommend that nothing changes for them*: `group = n`, `range` and the implicit keyboard keep working, and a session is saved in the
   new form only when it has something that the old form cannot say. The alternative, a one-time conversion, makes every old file a different file.
10. **Where a key range lives.** *Recommend the lane in C2* (it is the lane's *Keys* cell today, and the terminal's `R`), *and the cable in C3*, when a
    split is two cables from one keyboard with a range on each.
11. **Several browsers' keys.** *Recommend one source for the computer* (the TUI's piano, every browser's keys and piano), as it is one path today. A
    keyboard for each browser is possible with the same machinery and I see no use for it yet.
12. **The terminal's part.** *Recommend lists* (§7). If it must draw the graph, C1 is about as much work again.

## 10. Risks, and what I would measure first

- **The canvas.** Cables that cross strips are harder to read than the strips. Routing round nodes (`cableRoute`), a layout that puts a bus to the right
  of what feeds it, and a gain you can read on the cable are part of C1a, and a screenshot of the example (two oscillators, a reverb bus, a keyboard) is
  how I would judge it before calling it done.
- **Undo of removal.** A lane, a group and an effect are put back whole, with the cables that point at them; each journal grows by the audio and MIDI
  cables, and the easiest thing to forget is the cable *from* a node that is put back (the removal took it out, and the node's own journal never held
  it). The tests of C1a take every removal with cables on both sides.
- **Real time.** A cable that is added or removed is a command that the audio thread takes between blocks, as every command is; the order is computed on
  the main thread; the gain ramps. I would count allocations on the audio thread as `notes_that_start_sound_and_end_do_not_allocate` does, with cables
  added while a note sounds.
- **A `NaN` or a huge sample from one source into a shared bus** spoils what every other lane has sent into it, where today it spoils its own lane and
  the master (a mute's zeros do silence it). A pass over a sum costs little, and I would measure it before adding it.
- **CPU.** A cable is an add and a multiply per sample; a bus with ten cables costs what ten lanes' summing costs now. I would measure it with the
  stress scene before saying so.
- **Level.** Two oscillators into one reverb is twice the level into it. A person will want the gains; the default of 1 is what the lanes did before.
- **The MIDI default.** An instrument with no MIDI cable that hears nothing is a trap for a person who deletes a cable by mistake; the implicit keyboard
  for old sessions and a light on a node that hears no keyboard are how I would soften it.
- **The size.** C1a touches the model, the engine, the file, the protocol, the page and the terminal, as `Modulate` did; it is the largest step since
  undo, and I would cut it into the engine and the model first (tested without a front-end), then the page, then the terminal, with the first commit of
  each a test that fails.

## 11. The design without a clock

*Added the same day, after you asked for the best design if it did not have to be quick to build: "the things that will lift this to new levels, without
any time constraints on how fast its developed. What is the best way to solve it? My goal as a truly modular (but still fast) system."* Where this section
disagrees with §4 and §9 it replaces them. The short answer: **the destination is E2, a compiled graph, and E1 (the routing table) is a detour.** Its engine
half would be written, tested and thrown away, and the reason I gave against E2 in §4 was its price, which you took out of the question. What is left of that
reason is its *risk*, and §11.6 answers it with an oracle and not with a smaller design.

### 11.1 The design in one paragraph

A **typed patch graph is the model, and the only structure**: nodes with typed ports, and cables with ids. It is **compiled on the main thread into an
immutable plan** that a small real-time kernel runs, over **node state that lives in an arena and outlives every plan**. Audio, events (MIDI, and what comes
after it) and control (modulation) are three kinds of port on one kind of node and one kind of cable. A lane, a group, a rack, a send and a split are
*templates*: patterns of nodes and cables that the program makes and you can open, and that the engine knows nothing about. The same kernel runs the inside of
a `Patch` node, once per voice, so that track B is this engine one level down and not a second one. It is modular because nothing in the engine knows what a
lane is, and fast because the work of a block is a flat list of calls over buffers that were made before, with silent work skipped and independent
branches able to run on more cores.

What each thing in the session becomes:

| Today | In the graph |
|---|---|
| a lane (`InstrumentLane`) | an *instrument rack*, a template: the instrument, its event nodes, its chain and a strip |
| a group | a *bus*, a template: a sum (the fan-in of an input), its chain and a strip |
| volume, pan, mute, solo, the lane's meter and taps | a `Strip` node; the compile gives each strip the gain it is heard at (the law of §9.8) |
| range, remap, pattern, transpose (in that order, in `InstrumentLane::process`) | four event nodes in front of the instrument, fused into one pass by the compile |
| the lane's and the group's modulators | modulator nodes with control outputs; where they sit is a layout, not a scope |
| `NoteInput`'s sources | keyboard and device nodes with event outputs |
| probes and meters | taps on ports and cables |
| 68 `GraphCommand`s | commands that name nodes: set a value, install a plan, insert a node, remove one |
| `Inverse::Lane`, `Group`, `Effect`, `Modulator` | one: the insertion of what the kernel handed back, and its cables |

### 11.2 Six ideas

**1. One model: typed ports, cables with ids, a compile step.**
A port is *audio* (a block of `f32` for each of n channels), *events* (notes, expression, controllers, each with a sample time) or *control* (one value for the
block, or one for each sample where the receiver can use it). A cable joins an output to an input of the same kind, and has what the three have in common today:
an id that survives removals and comes back with Undo, and a gain (audio) or the depth and the law that modulation cables have (`add` around the value you set;
`peak` for an envelope, whose peak is the value you set). An input that several cables end at sums them (audio), merges them by time (events) or adds their
offsets (control): fan-in needs no mixer node. A parameter is a control input, and exists as a port when a cable ends at it or it is pinned (a plugin with
2,000 parameters does not have 2,000 ports). `compile` checks the kinds and refuses a cycle that a feedback cable (one block of delay) does not cut; orders
the nodes (ties by creation order, so that a render is reproducible); finds the latency of every path and puts a delay on the shorter ones; gives every cable a
buffer by lifetime (graph colouring: a session of fifty nodes needs a handful); fuses what is always together (a dry/wet mix with its delayed dry, the event
nodes of a lane); and makes a plan.

```rust
// the model, on the main thread: what is edited, saved and undone
struct Patch { nodes: Map<NodeId, Node>, cables: Map<CableId, Cable> }
// what compile() makes of it: immutable, Send, and nothing in it allocates when it runs
struct Plan { ops: Vec<Op>, buffers: Layout, controls: Vec<ControlEdge>, latency: u32 }
// what the audio thread owns: the state, which outlives every plan
struct Kernel { nodes: Arena<Box<dyn Node>>, plan: Plan, pool: BufferPool }

trait Node: Send {
    fn ports(&self) -> &[PortSpec];            // audio (n channels) | events | control
    fn process(&mut self, io: &mut Io<'_>);    // one block: inputs, outputs, events with their sample times
    fn latency(&self) -> u32 { 0 }             // frames, so that the compile can compensate
    fn tail(&self) -> Tail { Tail::Unknown }   // so that silence can be skipped
    fn reset(&mut self);                       // a panic that ends a reverb's tail too
    fn save(&self) -> Option<Vec<u8>>;         // a plugin's own state, which a session loses today
    fn load(&mut self, state: &[u8]);
}
```

**2. Edits that neither click nor restart anything.**
The kernel owns the nodes in a generation-checked arena, and a plan only names slots. A change of structure sends a new plan, swapped in at a block boundary,
and the old plan goes back to the main thread to be dropped (as plugins do now). A new cable ramps in and one that is taken out ramps out over one block (the
fade of a mute, for every cable): reconnecting while a note rings does not click, and a reverb's tail is not cut by what happens around it. A node that is
removed is handed back *with its state*, so that Undo puts back the very node. Commands name nodes: **61 of today's 68 `GraphCommand`s name a lane, a slot,
a group, a member or an index** (counted), and the code that keeps positions in step when something comes or goes is **31 functions and 443 lines in
`chain.rs` and, mirrored, 20 functions and 304 lines in `model.rs`** (counted: `shift_*`, `fixup_*`, `remap_*`, `adjust_*`, `update_*_base` and their
helpers, outside the tests). With ids that code does not exist, and nor do its tests. Undo becomes a command pattern over a few primitives (add and remove a
node, add and remove a cable, set a value, set an attribute, move), in transactions.

**3. Events with a name for every note.**
An event is a sample time and a kind: a note on (with an id, a key, a channel and a velocity), a note off (by id), an expression on a note (pressure, slide,
tuning), a controller, a wheel, a transport message. MIDI, MPE included, is converted at the edge, and each hosted plugin gets what its format takes (CLAP and
VST3 have note ids and per-note expression, and CLAP per-note parameter modulation; LV2 gets MPE channels; I have not tried any of them in this code). Sources
and sinks are nodes: the computer's keys, each MIDI device (an unplugged one leaves its node and its cables, which reconnect when it comes back), a pattern, an
arpeggiator, a MIDI output. *Timing:* live notes are stamped 0 today (`NoteInput::send`) and start with the next block, which is up to 5.3 ms of jitter at 256
frames and 21 ms at 1,024; stamped on arrival against the audio clock and played one block later they have a constant latency and no jitter, and a note and a
pattern share one clock. *No hanging notes:* a cable that is removed or re-routed sends a note-off for each note it was holding, so that an edit cannot leave
one (the TODOs about undo and stuck notes in `CLAUDE.md`).

**4. Modulation is control cables, and a clock is a node.**
A modulator is a node (LFO, envelope, steps, macro, MIDI control, velocity, key, as now) whose output is a control port. The scope of a lane or a group goes: a
modulator can drive any parameter in the session, a parameter of another modulator, or the depth of a cable (the cross-mod that exists, with its depth
cables, is a port on the cable). Math nodes come with it: scale and offset, multiply (a VCA), slew, sample and hold, quantize to a scale, min and max, switch,
random. The oscillator's `quantize` (D69) is then one such node that goes on *any* cable, and the parameter stays as the short way for the oscillator. A
**transport node** (tempo, beat and bar position, start and stop; later a tap, MIDI clock or Link) is what tempo-synced things lock to: LFOs and step
sequencers that are in phase with the beat, and patterns stored in beats (the TODO of step 67, and Open question 10 of `docs/web-ui.md`). Control is one value
per block by default, as now; a port can ask for audio rate, which the builtin nodes (and track B) use, and a hosted plugin gets block values and smooths
them itself.

**5. Hosting that shows what a plugin has.**
A port for every bus a plugin declares: the sidechain input (silence today), every output of a multi-output instrument (the chain keeps the first stereo pair
of Pianoteq's sixteen and drops the rest), and events out for the plugins that are MIDI effects. The contract the graph needs and `Plugin` lacks: a *latency*
(so that the compile can delay the dry path, which ends the comb filter that is why the Drive starts fully wet), a *tail* (so that silence can be skipped), a
*reset* (so that a panic ends a reverb's tail too), a *state* to save and load (a sample loaded in a plugin's own window is not in a session today), and *events
out*. A node is also the place where a process boundary could go: a plugin in a process of its own, behind the same contract, so that one that crashes does
not take Tang with it. That is not in the plan; the contract does not stand in its way.

**6. Modules, and voices on the same kernel.**
A **module** is a subgraph with exposed ports and knobs, kept in a file and nested, and *flattened by the compile*, so that it costs nothing to run. The lane
and the group are the first two. What you see today is what the templates make, and what you add is a cable. A **`Patch` node** is track B: an inner graph that
the same compiler makes a plan of and the same kernel runs, once for each voice, with audio-rate control, and with the voices' state laid out so that the
builtin nodes can process several at once (SIMD). The session graph stays stereo: polyphony lives where the voices are. A note's id carries its per-note
expression into its voice, so that your Seaboard's pressure and slide reach one voice of a chord, which `CLAUDE.md` says plainly (under "MIDI routing") that
they cannot today.

### 11.3 What "fast" is made of

- **What costs is the plugins' DSP.** The graph adds, for each node and block, a virtual call and a few loops: a thousand nodes should be hundreds of
  microseconds of a 5.3 ms block (an estimate. P0 measured what today's engine costs, DSP and all: a session of 8 lanes of a unison of seven through four plugins and four modulators each, in two groups (78 plugins and modulators), is 363 µs a block of 256 frames when nothing sounds and 801 µs with a chord of six, 15% of the 5.33 ms; what a node costs over its DSP only a kernel with nodes that do nothing can show, and that is for P1 to measure before I claim it). A lane with two effects makes about eleven passes over
  its block today (four buffers zeroed, the volume, two copies, a blend for each effect whose mix is under 1, the fade, the sum); at 256 frames that is
  microseconds and not the problem, and the colouring and in-place work take most of it away.
- **Where the speed is, in the order of what it saves.** (1) *Doing less*: a subgraph whose input is silent and whose tail has ended is not run, so that a
  muted lane costs nothing (it costs all of it today: a TODO in `CLAUDE.md`). (2) *More cores*: the plan has the dependencies, a pool of real-time helper
  threads runs what is ready, and the sums are in a fixed order, so that a render is the same bit for bit whatever the number of threads (a plugin can be pinned
  to the audio thread; on macOS a helper should join the audio device's workgroup, which I would first check that the audio library lets us do). (3) *SIMD across
  voices* in `Patch` nodes. (4) Tidy memory.
- **The rules of the kernel.** No allocation, lock or system call on the audio thread; the work of a block is bounded by the size of the plan; a plan is data;
  state outlives plans; the order is deterministic. The tests of these that exist (`alloc_guard`, the exact renders) become property tests over random graphs.
- **What the canvas gets for free.** The kernel times each node (two clock reads a node per block: microseconds for a hundred), so each node can show how much
  of the block it takes, and a session that is too heavy says which plugin it is.

### 11.4 The page and the terminal

**The page.** The handles are real: audio, event and control ports have shapes and colours of their own, and while a cable is dragged the ports that take it
light (as `tang-port--takes` does for modulation); a drop on empty canvas opens a palette of the nodes that could take it ("insert a reverb here"); a cable
carries its level (modulation cables brighten with their modulator today, and audio cables do the same with the signal) and can have a probe on it (the
scope of step 46 moves from nodes to cables); a module collapses to a node and opens to its inside; the parts that are graph-shaped are laid out in layers, left
to right, with the places you give kept (`ui`), and the strips stay for what the templates make. `toFlow` (725 lines) has to learn all of it, and the page's
mock engine (`client/mock.ts`, 1,774 lines) has to learn the graph, pinned to the controller by the fixtures as it is now.

**The terminal** cannot draw a graph and need not: a **patchbay matrix** for each kind of cable (audio, events, control), outputs down the side and inputs
across the top, `Enter` on a crosspoint makes the cable or takes it out and `+` and `-` turn its gain; the lists of §7 stay for the cables of one node. A matrix
is how text has always edited a graph (the ALSA and JACK patchbays). Both read and write the one model, and `tang --web` runs both on one controller as it does now.

### 11.5 The file

The netlist is the form that the model has: nodes and cables by name, what the page draws, in text.

```toml
[[node]]
id = "keys"
kind = "keyboard"
source = "computer"

[[node]]
id = "osc1"
plugin = "builtin:osc"

[[node]]
id = "osc2"
plugin = "builtin:osc"

[[node]]
id = "verb"
plugin = "builtin:reverb"

[[cable]]                       # events: the keys to both oscillators
from = "keys.events"
to = ["osc1.events", "osc2.events"]

[[cable]]                       # audio: both oscillators into one reverb, the first at 0.6
from = "osc1.out"
to = "verb.in"
gain = 0.6

[[cable]]
from = "osc2.out"
to = "verb.in"

[[cable]]
from = "verb.out"
to = "master.in"
```

What is written today is the **sugar** (`[[instrument]]`, `[[group]]`, `group = 0`, `range`, the effects in a lane) and loads by being *lowered* to the nodes
and cables of its template. A session that is still exactly what the templates make is saved as it was written, and one that has gone beyond them is saved with
a netlist for the rest; both may be in one file. The netlist is also what makes a session something you can diff, merge, write by hand, generate, and reload
while the program runs (a file watcher turns the difference into intents, which are steps of Undo and are told to every browser), and with no device the same
kernel could render a session to a file (`tang render session.toml --seconds 30 out.wav`: the command does not exist yet), as the tests of the examples already render it in memory.

### 11.6 The way there: the old engine is the specification

The risk of a new engine is that it is not the old one. The program has the means to know: it has 48,800 lines of tests in the source files and 1,700 more in `tests/` (counted), the page's
fixtures that pin its laws to the engine's (`audible.json` has 4,096 cases), and the audio tests of the examples. So the old engine is kept as the oracle, and
the new one grows under today's structure until it equals it, and is then taken apart, step by step, with the oracle checking each step.

- **P0, the oracle and the boundary.** A corpus: every example session and a few hundred sessions *generated* from a seed (lanes, groups, effects, modulators
  of each kind, mutes and solos, patterns, ranges, remaps, a scripted run of notes with expression), rendered by the engine as it is, at three block sizes,
  kept as hashes of the samples (a few as WAV, to listen to). The audio thread's engine behind a trait that `Renderer` drives, so that two engines can be run
  on the same input. A benchmark (ns a block of the stress scene, allocations, a histogram of how close a block comes to its deadline), so that §11.3 gets numbers.
  *To try: nothing new.* **Built** (step 70 of `docs/web-ui.md`, `src/oracle/`; the numbers are there): the trait (`audio::Engine`); 20 sessions and 300 generated ones with
  scripts of notes and controllers, the generated ones also with edits made through the controller while they sound (a mute that changes, an effect moved or taken out, an
  Undo), each at 64, 256 and 1024 frames, 960 renders, kept as a hash of the samples and of the telemetry for the platform they were made on (macOS on Apple silicon: Linux
  and Windows are held to numbers, to 1%) and rendered through the callback's own `Renderer`; `oracle::agree`, which renders a scenario with two engines and names the first
  sample that differs; the benchmark (median, p95, p99 and longest block against the time a block has, and the calls of the allocator), and WAV files to listen to. The corpus
  is held to render the same bits twice (it did not: a hash map of the oscillator's voices and one in the note remapper, both fixed) and to ask the audio thread's allocator
  for nothing outside the first block and the blocks that an edit was made before (it did: the pattern player's lists, fixed). Of 24 mutants of the engine, all are caught: 17 by the file-driven part of the corpus,
  and 7 only by the edits (the step of a mute's fade is one of them), two of those by the whole corpus and not by the always-on part.
- **P1, the kernel, with today's structure as coarse nodes.** The graph, the compile, the plan and the kernel, with the lane and the group as two coarse
  nodes that contain today's code unchanged, a plan that has exactly today's edges, and the old commands through an adapter. It must equal the oracle bit for
  bit at every block size; then the default engine changes. *To try: the same sounds.*
  **How P1 is cut** (decided 2026-10-20 after reading `AudioGraph::process` again; each step is a commit, with the oracle and the suite green after it):
  1. `kernel/`, a workspace crate that has no interface and no knowledge of lanes: `Arena<T>` (a key is a slot and a generation, so a key of a node that is gone finds nothing and never
     finds another node; a slot whose generation would wrap is retired and never used again), tested alone.
  2. `Patch<I>`, `compile` and `Plan<I>`: nodes with audio ports and a *rank*, and cables. The order is the topological order that takes the least `(rank, insertion)` of the ready nodes at
     every step, so that it is a function of the patch and of nothing else (a cycle is an error); every value (a node's output, the sum at an input) gets a buffer by lifetime, the
     least number that fits (interval colouring, the lowest free buffer first, so that the same patch is laid out the same).
  3. `Plan::run` over a `Pool` (and `Kernel<N>`, an arena with a plan, for the crate's own tests and benchmark), the reference interpreter (a buffer for every cable, in the plain order) that it is held to
     bit for bit over seeded random graphs, a check that a block asks the allocator for nothing, and the benchmark of nodes that do nothing, which gives §11.3 its number.
  4. CI runs the crate on the three platforms.
  5. In `tang`: the lanes and groups stay in their vectors, which become `Ordered<T>` (`src/plugin/ordered.rs`): the `Vec` API and a slice's that the engine's code and tests already use, and for
     each item a key from the arena's allocator that stays with it while the others come and go and change places (the plan names keys; the commands still name places until P2). (This changed
     from what step 1 above said, which was to put the lanes and groups into the arena itself: with the items in the arena every one of the 136 places in the engine's code that uses the vectors (counted), the tests that push lanes into them,
     and the helpers that take slices of lanes and groups would have had to change in the same commit as the engine's loop, and the oracle could not have told which of the two was wrong. They go into the
     arena in P2, where the commands name keys and the position code goes.) The lane, the group and the mix become nodes that run today's code (`InstrumentLane::process`, `Group::finish` on its bus in
     place, the fade and the meters after them, as they are); `AudioGraph::process` is then: the commands, a plan made again if the wiring changed (found by reading it into a `Shape` before every
     block and comparing it with the one the plan was made for, so a knob that is turned compiles nothing and a lane that a test pushes into the vector is found as well as one that a command
     puts there), what is heard (the mutes and solos of all, worked out before any of them runs), the group modulators, which cross lanes and groups and stay in the adapter until P5 makes modulators
     nodes, `Plan::run`, and what the group modulators drove in the members, published. The old loop stays as a test-only `process_legacy`, and `oracle::agree` holds the plan to it.
  *Built so far* (steps 1 to 3, in `kernel/`): the arena (19 tests); `Patch`, `compile` and `Plan` (33 more); `Plan::run`, `Pool`, `Io`, `Kernel<N>` and the reference interpreter (17 more, among them the
  kernel against the interpreter over 400 seeded random patches, with failing nodes and plan swaps, bit for bit) and `tests/no_allocation.rs` (5: a block asks the allocator for nothing, the first one
  after a fit included, and a counter that is shown to count); every mutant of the three is caught (16 + 25 + 31, among them the five that make the executor allocate, by the allocation test alone).
  *What the graph costs* (`cargo test --release -p kernel --test benchmark -- --ignored --nocapture`, Apple M4 Pro, rustc 1.99, nodes that do nothing, median of 2,000 blocks): the bookkeeping of a
  block is 10 to 15 ns a node (0 frames); with stereo buffers, which have to be zeroed where a value begins and fed into the sums, 35 ns a node at 64 frames, 73 at 256 and 220 to 240 at 1,024 (it is
  memory, and P3's aliasing of a single cable and P7's skipping of silence are what take it away). A chain of 1,000 nodes is 78 µs a block of 256 frames (1.5% of the 5.33 ms), a fan-in of 1,000 sources
  55 µs, and a session's shape (64 lanes of four nodes, two groups: 259 nodes) 19 µs (0.35%).
  *Built* (step 5, `src/plugin/chain/nodes.rs`, `src/plugin/ordered.rs`, which P2 took out again: the vectors are plain and the plan names the model's ids): the engine is run by the plan. `oracle::agree` holds it to `process_legacy` bit for bit over the always-on corpus in every test run and
  over all 960 renders, edits included, at 64, 256 and 1,024 frames, in the ignored test; the stored hashes of P0, which the old engine made, hold for the plan too (960 of 960 to the bit on macOS on Apple
  silicon). It cost what the loop did (release, M4 Pro, medians of `oracle::bench`, which times both now): the stress session with a chord of six at 256 frames 815 µs the loop and 816 µs the plan, nothing played
  366 and 366, one oscillator and a chord 28 and 28, and no block of either asks the allocator for memory. 36 mutants of the nodes, the schedule and `process` and 18 of `Ordered` are all caught. What the oracle found on the way: the
  allocation check said that after the plan was made in the first block, every session asked the allocator once in its second (the scratch that the wiring is read into had no room: it was swapped, and is copied).
  **The rule that decides what is bit for bit:** the input of a port is a sum that starts at +0.0 and adds the producers in *plan order*, also for one producer. It is today's mix (`mix_buf` and
  a group's `accum` are zeroed and the lanes are added in), `0.0 + -0.0` is `+0.0` and the oracle hashes the bits, so a single cable is not a copy: a later step that makes one (P3, when the
  chain of a lane is a row of nodes) names the change and its reason in the list the end of this section asks for. The sums are in a fixed order whatever runs where, which is what lets a render be the
  same bits on any number of threads later (§11.3).
- **P2, ids for places.** Commands, the model and the history name nodes; the 51 fixup functions and their tests go; the lane and group accessors stay as
  views, so that the tests of `src/app/` (about 22,600 lines against 9,300 of code, counted) keep guarding what they guard while the
  inside is replaced.
  **Narrowed, 2026-10-20, after reading every one of the fixups and every place that uses a position (P1 was done, and P2 was next).** The 51 functions exist for one thing:
  keeping the *modulators'* references valid when structure changes (a target's chain slot, a group target's member ordinal, a sibling's index, the place of a cable whose depth another
  cable drives). **P5 replaces every one of those references with a cable between ports** (a modulator is a node, a parameter is a port), so rewriting them to ids now would be written
  twice: ids that are held by hand in a dozen kinds of reference, and then cables. What P3 and P4 need is less: stable ids for the nodes that audio and event cables will join, which are
  the lanes, the groups, the effects and the bus effects (a lane's instrument is named by the lane's id, as the model has it). So P2 is that, in three steps: (1) `NodeId` and `EdgeId` move to a
  module that the engine can use without reaching into the model; (2) the engine learns the model's ids: the commands that make a lane, a group, an effect or a bus effect carry the node's id,
  the engine keeps it, and a check that the engine's ids are the model's after every edit (the oracle's edits run it, and so do the tests of `src/app/` that compare the two) holds them
  together; (3) the plan names those ids and `Ordered`, which P1 made to give the plan keys, goes (an item that has an id needs none, and the plan finds it through a map made when the plan is). The
  commands that address things that are there stay positional, and go over to ids family by family when a step needs one to (P3's cables are by id from the start); the fixups go with P5.
  *Built* (2026-10-20, step 72 of `docs/web-ui.md`, in three commits). (1) `src/ids.rs`: `NodeId` and `EdgeId`, re-exported by the model. (2) `AddInstrument`, `AddGroup`, `InsertEffect` and
  `InsertGroupEffect` carry the model's id; a lane and a group keep it, and an id for each effect beside its mix (a lane that gives up its plugins keeps the mixes and the ids), and it goes
  where the node goes; the loader, the controller and the journal send them. They are held to the model's by `LaneFacts` and `GroupFacts` in every test of `src/app/` that compares the engine
  with the model (twelve failed first), by tests of the engine for effects put in, moved and taken out, and by the oracle, which compares the engine's lanes, groups and effect ids with the
  model's after every block of every render (`Engine::facts`): 960 renders, edits and Undo included. 26 mutants of the plumbing, all caught (one lived: `Controller::add_group` had no test
  on a real graph). (3) The plan names the model's ids (`Patch<NodeId>`, the mix is `NodeId::MASTER`), `Ordered` and its keys are gone (the vectors are plain), and a list of where each id
  is, sorted by id and made when the plan is, is what a block finds a node by; a node that is not at its place any more is silence, never another node. Two nodes of one id do not compile.
  16 mutants of that, all caught. The oracle renders the same bits (the stored hashes hold: 960 of 960, and the plan equals the loop), and the plan costs what it did (see step 72).
  *What it does not do, and why:* the commands that address a thing that is there still name places, and the 51 fixups stay; they exist to keep a *modulator's* references valid, and P5 makes
  those references cables between ports, so rewriting them to ids now would be written twice. The lanes and groups do not go into the arena yet: nothing needs it before P3 takes a lane apart.
- **P3, take the lane apart, and draw audio cables.** The lane splits into instrument, effects and strip (volume, pan, mute, solo, meter), the oracle checking
  every split; the compile finds the latencies and puts the delays; cables are drawn in the page, in the terminal's patchbay and in the file. *To try, and the
  first thing you can see:* two oscillators into one reverb, a send, a sidechain, a multi-output instrument to separate effects.
  **Cut, 2026-10-20 (P2 was done and P3 was next; each piece is a commit or a few, the oracle held to the old engine until the first cable is drawn, written here first so that it survives a restart):**
  - **P3a, the lane and the group as rows of nodes in the plan** (engine only, nothing to see, the same bits). The data stays in `InstrumentLane` and `Group` (the 51 fixups and the
    positional commands are P5's, and a lane that is taken apart in memory would put them all in the same commit); what changes is the plan: a lane is `Source` (the events through
    range, remap, pattern and transpose, the modulators, the instrument, the volume and the tap after it) → `Fx(i)` for each effect (the plugin, the blend by the mix, the tap) →
    `Strip` (the metronome's click, the fade of a mute or a solo, the meter, the learned controls); a group is `Bus` (the volume) → `Fx(i)` → `Strip` (pan, taps, fade, meter). The node
    bodies are the old code cut at the seams that it already has, as free functions over the parts, so that they are what moves when the data does; the old monolithic `process` stays
    as `process_legacy`'s (test builds) and is what `agree` holds the nodes to, until the first cable is drawn. A lane with no instrument runs none of its effects, as today (they
    make silence; *step 85 changed it: they run, on the silence of the head and on what cables bring them, so that they can stay in a lane whose instrument is taken out*). **The kernel learns one thing:** an input that takes one cable can be *the cable's buffer* (`PortSpec::single`: no sum, no zeroing, no feed), so that a chain of
    nodes costs what the old chain did and gives its bits exactly (the old chain does not make `0.0 + -0.0` of anything between its stages; the sum at a group's bus and at the mix
    stays a sum, since the old engine added into zeroed buffers there). An input that is `single` and has two cables is a sum, so P3b needs no other rule. The reference interpreter
    knows it, and the executor is held to it over the random patches.
    *Built* (2026-10-20, step 73 of `docs/web-ui.md`, in three commits). (1) The kernel's `PortSpec::single` (`889d3d5`): the compile gives an input that has one cable, the only one that leaves its output,
    that output's buffer (the output lives to the op that reads it; nothing is zeroed, fed or copied, and a negative zero stays one), the reference interpreter has the rule, the random patches draw
    half of their inputs `single` from a generator of their own, and the test nodes follow a negative zero end to end, so that a sum that should not be one shows (14 mutants, all caught). (2) The
    lane and the group as rows of nodes in the plan: `Stage::{Head, Fx, Tail, Master}` named by the model's ids, the stages of a chain `single` inputs, the group's bus and the mix sums, ranks the
    places (every node of a lane has the lane's), the plan's shape a list of ids that has the effects in it, and node bodies that are the old code cut where it was cut by what it did (`run_head`,
    `run_fx`, `run_tail`); `InstrumentLane::process` and `Group::finish` stay in test builds as the specification. The whole corpus (960 renders, edits included) is the bits of that specification
    and the stored hashes of the old engine; five tests hold what the corpus does not draw (the metronome's click, the effects of a lane that has no instrument, an instrument of one output and
    one of more than the chain has channels, a mix of 1, a volume with effects); and 39 mutants of the nodes, the plan and the shape are all caught but one that is no fault (the volume applied
    to the outputs of an instrument that are dropped anyway). The plan costs what the loop did (815 µs the loop and 819 µs the plan, a block of 256 frames of the stress session with a chord of
    six; no block allocates). (3) The docs. *One decision on the way (D73):* an instrument is given a buffer for each of its outputs and no more, also with no effects and a volume of 1 (the old
    fast path gave it the chain's whole width, the slow path its own), which changes nothing for the stereo instruments that the corpus has.
  - **P3b, audio cables in the engine.** Commands that add and take out a cable between two ports by id (`AddCable`, `RemoveCable`, `SetCableGain`), with a gain on each cable (a send
    level); a cable is a node of the plan of its own (`Stage::Cable(id)`: a gate that multiplies by the gain and by what its source's owner is heard at, the lane's or the group's, since a
    muted lane's sends are muted with it), so that it ramps over one block when it comes (from nothing), when it goes (the node stays for one block of ramping out) and when its gain
    changes, as a mute does, and fan-in at the destination is the sum in plan order (§11.3); a cycle is refused (the command fails and nothing changes: the plan is compiled on the side
    first). An output that has a cable and a chain link has two cables, so the chain link is a sum of one there (the kernel's rule for a `single` input), which is new behaviour and cannot be
    in a session that has no cable. The ends that a cable can have are named by the model's ids: the output of an instrument (a lane's head: its sound after the volume, a tap before the
    chain), of an effect, of a group's tail; the input of an effect, of a group's bus, of the master. The engine has the cables and nothing sends them yet. Tests of the engine alone: two
    lanes into one effect equal the two renders added, a send, a cable that comes and goes does not click (the click detector of §11.6 is made here), the plan is the same after the same
    edits in another order, a cycle is refused, a cable whose end goes with a lane or an effect goes too.
    *Built* (2026-10-20, step 74 of `docs/web-ui.md`, in two commits). `GraphCommand::{AddCable, RemoveCable, SetCableGain}` and the ends `CableFrom::{Instrument, Effect, Group}` and
    `CableTo::{Effect, GroupBus, Master}`, named by the model's ids; `Stage::Cable(EdgeId)` is a node with one input and one output that has the rank of the node that owns its start, and its body is
    `fade(input, at, goal)` of the code that lets a muted lane go (so a cable comes in from nothing, goes down to nothing and is turned over one buffer); the goal is the level that was set times what
    the owner of its start is heard at, worked out in `process` between the heard goals and the plan (`Schedule::set_cable_goals`, which finds the owner by the place that the plan has for it), so that
    nothing depends on whether the cable runs before or after its owner's tail; the output of a group has been let in or out already and is not gated again. The cables are kept by id and put in the
    patch in that order by `build_patch`, which `compile` and the check for a loop both use, so that the plan is a function of the wiring and not of the order of the commands; `AddCable` refuses (with
    a log line) an end that is not there, the id of a group for an instrument and the other way round, a level that is not a number, and a cable that makes the wiring not compile (a loop: made on the
    side first); a cable that is drawn again under its id goes on from where it was; `RemoveCable` brings one that is heard down over a buffer and one that is not heard goes at once; a cable whose
    end goes (a lane, a group or an effect taken out) is taken out when the plan is made again, and the shape that the plan was made for has the cables, their ends and their ids in it. 21 tests (16
    of the engine in `plugin::chain`, 5 of the plan in `plugin::chain::nodes`) by exact arithmetic on levels of powers of two, one of them 400 random wirings held to an evaluation that has no plan in it, 39 mutants of the cable, the plan's construction, the shape, the goals
    and the ends (all caught but one that is no fault: a cable to a lane's id as a group's bus is refused by the compile if the check for the end did not). A cable costs 0.45 µs a block of 256 frames,
    and an engine with none costs what it did (815 µs the loop and 817 µs the plan for the stress session with a chord of six). Nothing sends the commands until P3c; the commands and the two enums
    carry `cfg_attr(not(test), expect(dead_code, ...))`, which P3c takes off.
  - **P3c, the model, the intents, the history, the file.** **Cut in two (2026-10-20, when P3b was built), with P3d between them:** *P3c-1* is cables as sends: the model
    (`SessionModel.cables`: `AudioCable { id, from: CableFrom, to: CableTo, gain }`, the ids from the allocator that the modulation cables use), the graph a client gets (a new edge
    kind, `cable`, with its id and level; the derived `audio` and `member` edges stay as they are, and the ends are ports: an instrument's, an effect's and a group's `AudioOut` for a
    start, an effect's and a group's and the master's `AudioIn` for an end, the group's two being its output after its pan and fade and its bus), the intents `AddCable { from, to, gain }`
    and `SetCableGain { edge, gain }` and `RemoveEdge`, which takes an audio cable out too, the refusals by the controller before the engine (an end that is not there, a loop: found by
    compiling the wiring of the model, with `build_patch` over a description that the model and the engine both make), the cables that go with a lane, an effect or a group that is taken
    out (written down in the journal and put back by undo), the history (draw, set the level, take out), and the file (`[[cable]]` tables). *P3c-2* is what is left: the route of the end
    of a lane and a group (`route = "none"`) and the solo law with its head gate (the route is built, as P3c-2a, step 77, and the solo law, as P3c-2b, step 78). P3d is between them because cables as sends are what two oscillators into one reverb needs; the route
    and the solo law make it a patchbay. *The whole of P3c in one:* The model has audio cables (`EdgeId`s, a gain, undo that puts them back as they were, the graph a client gets has them as edges
    that can be taken out). *The form that they take beside the lane, the group and `group = n`* is my decision to make before building it, and it is this (overrule it before P3c starts): the
    templates stay, and a session has, besides them, **extra cables** (`SessionModel.cables`: an id, an output, an input and a gain) and, for the end of each lane and group, **a route**: by
    default to its group's bus, or to the master (today's `group = n`), or none (the template's own cable cut, so that a lane can be heard only through the reverb that another lane has). The
    cables of the templates that are not at a lane's or group's end (instrument to the first effect, effect to effect) are not edited: an effect is taken out or moved to change them (*since step 86 the link into an effect can be cut, `feed = "none"`: see `CLAUDE.md`, "The input of an effect"; since step 87 an effect can stand by itself, in a lane of its own that has no instrument (`AddFreeEffect`; the page draws no card for such a lane): see `CLAUDE.md`, "An effect that stands by itself"*). This is
    the netlist of §11.5 with the sugar kept: every extra cable and every route is a cable of the netlist, the file writes `[[cable]]` tables for them (`from = "instrument[0]"`, `to =
    "instrument[1].effect[0]"`, `"group[0]"`, `"master"`, `gain`) and `route = "none"` on a lane or group, and a session without any is the file it was. Cycles are refused when the cable is
    drawn. *Mute and solo for routed audio* (the law of §9.8 written for a graph, in the engine and in the page's copy of it, `audible.ts`, pinned by the same kind of fixture): a muted
    lane or group is not heard, its sends included (the gate of each cable it owns is its heard gain), and mute wins; while something is soloed, a lane or group that is not soloed, and not
    in a soloed group, does not put its own sound into its chain (a *head gate*; it is 1 whenever nothing is soloed, and a lane that has no cable ending in it has none), and what it
    carries out is heard only if it receives, through a cable, what an audible node sends: so that soloing the lane that sends to a reverb on another lane lets the reverb be heard, with
    only what the soloed lane sent. (A lane that nothing sends to is exactly as it is today: its tail is faded, and what is in its effects goes on.)
    *P3c-1 built* (2026-10-20, step 75 of `docs/web-ui.md`, in two commits). `SessionModel.cables` (`AudioCable { id, from: CableFrom, to: CableTo, gain }`, in the order of the ids, which are the
    modulation cables' series), `SessionModel::wiring` / `cable_problem` / `loop_if_lane_goes_to` / `loop_if_effect_goes_to` (the lanes and groups as `LaneWiring` / `GroupWiring`, which the engine's `compile`
    and the model both make, and `plugin::chain::cable_problem` / `wiring_loop`, which compile the wiring with the cable on the side with the kernel: what the model refuses is what the audio thread would refuse);
    the graph has `EdgeKind::Cable { id, gain }` after the links and before the modulation cables; `Intent::{AddCable, SetCableGain}`, `Event::CableGainSet`, `RemoveEdge` for an audio cable, and `SetLaneGroup` and
    `MoveEffect` refused when they would close a loop; the cables at a lane (its instrument and its effects), a group (its bus, its output and its bus effects) and an effect go first when it is taken out (`Inverse::AudioCable`,
    put back after the node, under their ids); the steps "draw an audio cable", "set the level of an audio cable" (`Key::Level`) and "remove an audio cable"; `[[cable]]` tables (`CablePlace`: `instrument[0]`,
    `instrument[0].effect[1]`, `group[0]`, `group[0].effect[0]`, `master`; `load_cables` refuses the places that are not there, a start that is the master, an end that is a lane, a cable twice and a loop, with the cable's
    place; `cables_to_save` writes them after the groups). The oracle has the engine's cables held to the model's after every block (`Facts`, `CableFacts`), the sessions `sends` and `patched` and the scenario
    `edited:cables`, and it found that a cable that is taken out left the plan at the start of the block after the command (a block with no command that made the plan): it leaves it at the end of its own block now. The
    page keeps the cables in its snapshot and leaves them out of the canvas (P3d draws them). Rust 2057 tests (2241 with `web`), a run of the real program through a WebSocket that hears a send double the
    master's meter, turns it, takes it out, undoes that, saves it and plays it from the file, and mutants of every part. *Decisions (D75):* one cable between two places (the level goes to 2); the level 0 to 2; a cable
    starts after the lane's volume; one series of ids; a session with a cable that cannot be had is refused; the cables go with their nodes; `[[cable]]` is a list of its own, in the order of the ids.
  - **P3d, the page.** *Built* (2026-10-22, step 76 of `docs/web-ui.md`, in five commits). The mock engine does what the controller does with audio cables (`client/wiring.ts`: the wiring of the plan read from the graph a client has, `cableEnds`, `cableLoops`,
    `laneLoops`, `effectLoops` and `cableTakers`; the intents, the history, the one series of ids), pinned to the controller by `fixtures/audioCables.json` (268 rows over `testutil::PATCHED`) and eight scripts of `history.json`. The canvas draws each cable as an
    amber edge `send:<id>`, as thick as it is loud, with a slider for its level at its middle and the button that takes it out (`SendEdge`, `CableLevelControl`); every node that sends has an output of audio on its right edge and every effect, group and the master an
    input on its left (`AudioPort`), a cable is dragged from the one to the other, the inputs that take it grow to a strip along their node's edge, and its line is amber and plain (`takersOf`, `canConnect`: the controller's rule, `cableTakers`, read from the graph); a
    menu on the badge's line of each header does it from the keyboard (`SendMenu`, `client/sends.ts`: Effects, Group buses and Mix). `examples/shared-reverb.toml` is *two oscillators into one reverb*, on the bus of a group that has no lane in it, with a test that
    its room rings on after the keys go up (an rms of 0.0024 from 1.6 s to 2.6 s) and does not without its cables (exactly 0), and that the room of both is the two rooms added (to 0.0003%). Web 2380 tests in 106 files, browsers 156 passed (`e2e/sends.spec.ts`: a cable
    dragged, the keyboard's menu, the history's words, and on the real program the cable in the session, in the file and put back by undo), mutants 82 of the canvas and 4 of the example, all caught but two that are no fault. *Decisions (D76):* the menu on the badge's line
    (a lane's row of buttons is full); amber for what a person drew, and its thickness its level; a level in percent, 0 to 200; the inputs that take a cable grow to a strip; the cable drawn over the nodes it passes; the line of a dragged cable by what the canvas says is
    dragged; the menu lists what takes a cable and nothing else; the example's room is the bus of a group with no lane in it.
  - **P3c-2a, the way out of a lane and of a group.** *Built* (2026-10-22, step 77 of `docs/web-ui.md`, in 3 commits). `route = "none"` on the table of a lane or a group (written only when it is none; a word that is not `out` or `none` refuses the session, with its place): `session::Route`, `InstrumentNode::route` and `GroupNode::route`, `GraphCommand::{SetLaneRoute, SetGroupRoute}` (the loader sends what a file cuts, before the cables), `build_patch` making no cable from the tail of a lane or a group that is cut (`Shape` has a `CUT` mark for those that are and none for the rest, so that a session that cuts none has the shape it had), `SessionModel::loop_if_route_is` and the engine's `Schedule::route_problem` (a way out that is given back where the audio would go round by the cables there are is refused; cutting one cannot close a loop), the intent `SetRoute` (a snapshot; a step of the history: "cut the way out of the lane of Oscillator", "give Pad its way out"), the graph with no link out of a chain that is cut, the terminal's `w` and `[no out]`, the page's button under the mute and the solo (`→ Master`, `no out`) and the mock engine pinned by the fixtures' `routes` and by a script of `history.json`. The oracle has the session `routed` and the scenario `edited:routes` (the old loop is not held to a session that cuts a way out) and 975 renders, and found nothing (the whole corpus is the same bits in a release build, and in a debug build but for twelve renders that were different before this step). `examples/shared-reverb.toml` with `route = "none"` on each oscillator is the room alone, to 7e-9 against an rms of 0.0135. Rust 2092 tests (2276 with `web`), web 2405 tests in 107 files, browsers 160 passed; mutants 64 of the Rust parts, 31 of the mock engine and the history and 24 of the page's button, all caught but those that are no fault. *Decisions (D77):* a way out has two words and not a place (where the audio goes instead is a cable from the end of the chain); the wiring changes at once (a ramp is a TODO); a way out is refused where it would loop and cutting one never is; a lane that is cut is not "not heard" for the mute and solo (the law for its cables is P3c-2b's); the file never writes `out`; the page's button is in the Mute / Solo cell; the terminal's key is `w`. *Later (step 84):* the link itself, the grey one from the end of a chain to the group's bus or the master, has a × at its middle on the page (`Disconnect Filter → Master`) that asks the same `SetRoute none` and puts the focus on the cell's button, which gives it back; the links inside a chain have none.
  - **P3c-2b, the solo law for what a cable carries.** *Built* (2026-10-22, step 78 of `docs/web-ui.md`, in 4 commits). `Schedule::set_gates` (in place of the heard goals and `set_cable_goals` of `AudioGraph::process`) works out once a block, after `refresh` and before the plan runs: what a lane or a group is let in by the solo for (`solo_heard`), what it is heard for as well, `live`, when a cable that is not told to go, at a level above 0, ends in its chain or its bus from something that is heard (the instrument of a lane that is let in and not muted, an effect of a lane or a group that is heard either way, the output of a group that is; a group is heard too for a lane in it that is, whose way out is not cut), by going through the cables until nothing changes (the least set; at most as many times as there are cables; nothing allocated), `heard_goal` (`out`: not muted, and let in or live), the cables' goals (their level by what their start is heard at) and the **head gate** (`head_goal`, applied in `run_head` after the scope tap): a lane that cables end in puts none of its own sound into its chain while the solo does not let it in, one ramp over a buffer that is at once while the lane is not heard and held while it is let go of, so that no bump of its own sound is heard when a cable is drawn or taken out. A session with no solo, or no cable that ends in a lane, has the bits it had (the 975 older renders of the oracle, in a release build); the oracle has the scenario `edited:solos` (978 renders, 50 always on), the engine's random wirings draw solos and check every sound against the law written as a recursion, and the page's `audibleOf`, `ownAudibleOf`, `heardOf` and `fedOf` are the law written again, pinned to the engine by `fixtures/audibleCables.json` (4,096 cases); a lane that is heard only for what it is sent has its instrument dimmed on the page and its effects not, and the mock engine's meters follow it. `examples/shared-reverb.toml` with the saw soloed is the session with the square muted, to the bit. Rust 2112 tests (2296 with `web`), web 2424 tests in 108 files, browsers 162 passed; mutants 62 of the engine's law (55 caught: 44 at once, five after tests that were missing and the six of the head gate as it is now; seven no fault) and 46 of the page's law, its meters and `audibleAt` (42 caught, 36 at once and six after tests that were missing; two removed with the dead code that they were in; two no fault). *Decisions (D78):* the solo goes one way, from what is soloed to what it sends to (the room alone, soloed, is silent); a lane that is heard only for what it is sent has none of its own sound, in its chain or in the cable of its instrument; a mute is not a head gate; a group is heard for a lane in it only if the lane's way out is not cut; the least set is the law; what a lane's effects hold of its own sound is heard when it is let in for a send; the head gate is a ramp like the others, except where a ramp would let a bump through; the page dims the instrument and not the effects.
  - **P3e, the terminal's patchbay.** *Built* (2026-10-22, step 79 of `docs/web-ui.md`, in 4 commits). The Patch tab (`src/tui/patch_view.rs`; the fourth tab, Help is the fifth): a matrix with a row for every output of audio (the instrument of each lane after its volume, each effect of each chain, each group, each effect of each bus: `CableFrom`) and a column for every input (each effect, each group's bus, the mix: `CableTo`), in the order of the lanes and then of the groups, and at each crosspoint the level of the cable that is drawn there, a dot where there is none and one can be drawn and a cross where the audio would go round (`SessionModel::cable_problem`, the rule that `Controller::add_audio_cable` refuses by, worked out for the crosspoints in view and kept until the wiring changes). `Enter` and `Space` draw a cable (`AddCable`, level 1) or take it out (`RemoveEdge`), `+` and `-` turn its level by 0.05 (`SetCableGain`), `e` types a level, `d` takes it out, `w` cuts or gives back the way out of the lane or group of the row (`SetRoute`), `M` and `S` mute and solo it; all of it through the intents that the page's cables are, so that it is a step of the history and is told to the browsers of `tang --web`, whose cables are on the tab with the next frame (it reads the model on every frame, and its cursor stays on the output and the input it was on). The first columns of a row say the way out (`▪G1`, `▪Mix`, `▫`), the mute and the solo of its lane or group, and the foot says what the cursor joins, what is there, and why a cable cannot be (or what the controller refused). A click, the wheel and the action bar are the mouse's. Rust 2163 tests (the TUI's 376 to 427), browsers 162 passed, mutants 121 of the tab and its place in the program, 108 caught by the first tests and 13 not: ten were tests that were missing (the cursor following an input that comes in before it, and held in the matrix when its input goes; the view following the cursor up, and not left with a blank at its end when there is more room, for the rows and for the columns; the rectangle of crosspoints that a click is on; a refusal of the level popup said in the tab; a click taking the refusal off the foot; the action bar's items each pressing their own key, which a click on `Enter` and one on `d` could not tell, since `Enter` takes a cable out too; the help's `1 2 3 4 5`; and the Session tab's own action bar, which no test had clicked since it was made), and three were clamps of the cursor keys that `settle` makes redundant, taken out. Six mutants more (the keys as they are, and the end of the view) were added: 127 in all, and all are caught but the three that were redundant code. *Decisions (D79):* a matrix and not a list of cables; the way out is a flag on the first row of a lane or a group and not a crosspoint; a group's row comes before its bus's; `Enter` toggles and `e` types; the cross is worked out for what is in view; Patch is the fourth tab and Help the fifth.
  - **P3f, latency.** *Built* (2026-10-22, step 80 of `docs/web-ui.md`, in 4 commits). `Plugin::latency` (frames; none by default; the built-in drive says 23, a CLAP plugin the number on its latency extension and a VST3 plugin `getLatencySamples`, both when it is activated at load, and an LV2 plugin the value of its `lv2:reportsLatency` port, every block; held to 131,072 frames; `tang describe` prints it), `kernel::align` (from the latency of each node: what arrives at a node is as late as the latest of its cables, all its inputs together, and what it makes is that and its own latency; each cable is to be delayed by the difference, so that the latency of the patch is its longest path and nothing is later than it has to be) with `kernel::with_delays` (a node on each cable that is to be delayed, with the rank of the node it starts at) and `kernel::DelayLine` (a ring that a block of any length goes through in place, kept through a change of length), `compile::layout` (the order that `compile` and `align` share), and in the engine `Stage::Delay` (a `Link` that the schedule keeps under the two nodes its cable joins, so that a plan that is made again keeps what it holds), the dry part of an effect that is late going through a line of the effect's own before its mix is made (so that a drive at a half mix is the sound 23 frames late, and no comb), the latencies in the `Shape` (a plugin that says another makes the plan again) and nothing in a session where nothing has latency (bit for bit what it was). The oracle has the engine without the compensation (`engine_uncompensated`, which the loop that the plan replaced is held to), the scenario `edited:latency` and the check that where nothing is late the two are one. Rust 2191 tests (2375 with `web`), the kernel's 109 and 7, mutants 75, 62 caught at once, nine after tests that were missing, one removed with the code it was in and three that are no fault. *Decisions (D80):* a latency is the plugin's to say and is held to 2.7 s; the paths are put in step with the latest and the session is as late as its longest path (nothing is advanced); a delay is a node of the plan and its line is kept under the two nodes it joins; the dry sound of an effect has a line of its own that runs whatever the mix is; a delay that comes in or goes out is a jump and not a crossfade; the drive is still put in fully wet (it is an insert); CLAP and VST3 are read at load and LV2 every block; the oracle holds the plan to the old loop with the compensation off.
  - **P3g, the ports that plugins have.** The outputs of a multi-output instrument and the sidechain input of an effect as ports (the chain takes the first stereo pair of Pianoteq's sixteen
    outputs today, and gives silence to a sidechain).
    - **The user's idea of 2026-10-27, built as step 95 (2026-10-28): an effect that does math on two audio cables.** "Can we make a FX that takes one or two audio cables as input and then do math on the inputs to create a new output? I am not thinking about just adding them but maybe to create an unique sound."
      What it needs is the small half of P3g: **an effect with two inputs of its own, A and B** (an effect has one today, the sum of what its chain and its cables bring, and a `Plugin` is given one input buffer), each a port that a cable ends at: `CableTo` would name the port, the graph an `audioIn` port for each of the two, the page two handles on the node, the plan two input ports of the node (the kernel's
      `NodeSpec` has as many as a node needs, and the alignment already puts all the inputs of a node in step, so a sidechain is in step with the main input), and the plugin trait a second input (`Plugin::inputs`, or a `process` that is given the list of them). A would be what the chain feeds it, B what a cable to the second port brings, and **B is normalled to an
      oscillator of its own while nothing is patched to it**, so that a module with one cable already sounds like something (a ring modulator with a carrier). One built-in, `builtin:math`, with a `mode` choice (a choice is a step from place to place for a cable, as the oscillator's waveform is) and two or three knobs whose meaning goes with the mode: **ring** (A × B), **AM** (A × (1 + d·B), `d` the depth),
      **min** and **max** of the two (a rectifier, a gate), **compare** (A above B is +1, else −1: a saw against a slower wave is pulse-width modulation, a sine against a sine is a burst), **bit logic** (XOR, AND and OR of A and B quantized to `bits` bits: the gritty, inharmonic sounds of a digital circuit), **fold** (A + k·B folded back at ±1: a wavefolder that another sound drives) and **sample and hold**
      (A held at each rising zero crossing of B: a clocked stair, a sample-rate reducer that another oscillator sets); later **follow** (an envelope follower on B that scales A: a sidechain, a ducker, with a vocoder band-wise) and cross-feedback, which needs a one-block delay on the cable (feedback cables are P3's, and not built). Every mode is stateless or has a little state (the held sample), so that it runs with no allocation and
      no denormal, and the oracle's session for it would be a ring modulator of two oscillators, which is exact (the sum and the difference of the two pitches). The questions that are the user's: **how many inputs** (two, or a number that the node grows: three is a mixer with a function), **whether A is the chain's feed or a port like B** (the free effect, `AddFreeEffect`, is bare and has no feed, so that both would be cables), and **whether the modes are one effect or many** (one with a choice is one node on the canvas and is what a cable to `mode` can sweep; many are smaller and clearer to read).
      **As built (step 95; `docs/web-ui.md` D95, `CLAUDE.md` "An effect with a second input").** The user left the three questions to me ("I will let you pick what it should contain"), and the answers are: **two inputs** (a node that grows them is the rest of P3g); **A is what the chain feeds and what the cables at the first port bring, and B is a port that only cables end at** (a free effect is bare, so that there both are cables); **one effect with a choice of twelve modes** (the page's node shows every knob, and a cable to `mode` steps through them). The port is `Port::AudioInSide` in the graph and `CableTo::EffectSide` in the engine: the second input port of the effect's node in the plan, there only for a plugin that says it has one (`Plugin::side_input`, with `Plugin::process_side` where the sketch had a list of inputs in `process`); the file says `.side` after the effect's place, the Patch tab has a column `↳B`, the page a second handle with its name, and the oracle a session and a scenario. B is a carrier of the effect's own while nothing is patched there (`pitch`, a note number, and `wave`), and goes to the cable's sound over 5 ms. The modes are `ring`, `am`, `fold`, `warp` (a delay that B moves: vibrato and FM, with no latency), `compare`, `min`, `max`, `xor`, `and`, `or`, `hold` and `follow` (built now and not later: a ducker, which `examples/pump.toml` uses with a ghost kick); cross-feedback is not built (it needs the feedback cables that P3 has not). What is left of P3g: a plugin of the three formats that has a sidechain bus (`process_side` is what an adapter would implement from its second bus), more than two inputs, and the outputs of a multi-output instrument as ports. (Step 95b, a parameter that says when it has an effect, `ParameterInfo::used_when`, is what the Math's carrier suggested and is no port: the knobs `pitch` and `wave` are dimmed while a cable is patched at B, as `amount`, `bits`, `attack` and `release` are in the modes that do not read them.)
- **P4, events.** The event with its note id; keyboard, device, pattern, range, remap and transpose as nodes; MPE; time stamps; no hanging notes. *To try:* a
  split, a layer, two keyboards into one synth, an arpeggiator between the keys and the synth.
  **Cut, 2026-10-22 (P3f was done and P4 was next; written here first so that it survives a restart):** the order of P3 again, the engine first and what can be seen after it, the
  old behaviour held bit for bit until the first cable is drawn, and the smallest thing that gives two keyboards two synths before the kernel learns a kind of port (§5's "C2": a source on
  each event and a mask on each lane; the events as cables of the kernel are P4f, and what the model, the file and the pages have by then stays as it is).
  - **P4a, sources and masks in the engine** (nothing to see, the same bits). An event on the channel to the audio thread has a *source* (`Source`, a number below 64: 0 is the keys, the
    computer's, the page's and every browser's, one source as they are one `NoteInput` path today; each MIDI device the program opens has a number of its own, handed out in the order they are
    first seen and kept for the run, so that a device that is unplugged and plugged in again is the source it was; `BROADCAST` is no source, and is what a panic is sent as). `NoteInput`
    is made for a source (`for_source`; `new` is the keys) and the audio callback hands the engine the block's events with their sources (`Events`: the events of a block and, beside them, the source of
    each, or none, which says that every event is from the keys, so that the plugin trait and the hundreds of tests that call `process` with a list of notes are as they were). A lane has `sources`,
    a mask of the sources it hears (`SourceSet`, every source by default, which is today's behaviour), and its first step drops what the mask does not have, before the range. **No hanging
    notes:** a lane keeps, for each source, what that source has sounding in it (the notes by channel, the channels whose pedal is down, the channels whose wheel is off centre), and when a
    source is taken out of its mask (`SetLaneSources`) or is gone (`ReleaseSource`: the unplugged device, which today leaves its keys held for ever) it lets go of what it holds from that source,
    a few events a block, as note-offs, a pedal up and a wheel centred, into the same list that the range, the remap, the pattern, the transpose and the modulators read, so that the pattern that a
    held key plays stops and a controller that learned a key lets go. A group's modulators hear the union of its members' (the events of a source that no member hears are not theirs).
    The tests are the engine's own: the oracle's corpus is held to the bits it had (the sessions have no source but the keys), and the scenarios that put masks in are P4b's, which has the
    intents to edit them with.
    *Built* (2026-10-22, step 81 of `docs/web-ui.md`, in 4 commits with the plan). `source::{Source, SourceSet, MidiEvent, Events}`: the channel to the audio thread carries `MidiEvent { source, frame, bytes }`, `NoteInput::new` is the path of the keys (source 0: the computer's keys,
    the page's piano and keys, every browser) and `NoteInput::for_source` that of a device, `midi::SourceTable` gives each MIDI device a number for the run by its port's name (in the order they are first seen, the 63rd and after sharing the last), the panic is sent as no
    source's (`Source::BROADCAST`, heard by every lane), and `audio::Renderer` (which has room for as many events as the channel holds, 1,024, where it had room for 64) gives the engine `Events`. `AudioGraph::process` is still the list of notes, as events from the keys,
    and `process_events` is the engine's. A lane has `sources` (every source until `GraphCommand::SetLaneSources` says another), `Sounding` for each source (the notes held by channel, the pedals down, the wheels off their centre, written down as events are passed on) and
    `release_sounding` (`GraphCommand::ReleaseSource` is the same for a source that is gone): the note-offs, then the pedal up and the wheel centred, first among the events that the remap, the pattern, the transpose and the modulators see, 32 a block at most. A group's modulators hear
    what some member hears. 45 new tests (the lane's and the graph's among them: 100 notes let go of 32 a block, the room shared by the sources, the pedal and the wheel after the notes where the notes use the room exactly), no block that lets go of a source asks the allocator for memory,
    and the whole corpus of the oracle is the same renders (the sessions have no source but the keys, and every lane hears every source). 87 mutants of the new code: 78 caught by the first tests, seven after tests that were missing, one that is no fault and one that went with the redundant check
    that hid it. Rust 2236 tests (2420 with `web`), the kernel's and the page's unchanged. *What is not built:* nothing sends the commands (P4b), and the MIDI manager does not notice a device that has gone (it is P4b's: a source that is gone is what `ReleaseSource` is for).
  - **P4b, MIDI cables in the model, the file, the intents and the history.** A source is a node of the graph that a person adds (`NodeKind::Source`: the keys, or a device by its name; the
    intent `AddSource`, taken out by `RemoveNode`, both steps of the history), with a port of events out. **None is there until one is added**, so that a session that has none is the file it was,
    every lane hears every source, and the canvas and the tests of the pages that have no use for MIDI routing are as they were. An event cable goes from a source node to a lane (the lane's node: an
    input of events, which only a source's cable ends at, as an input of audio is no lane's). *The default is the template's, as a lane that is in no group goes to the mix:* **a lane that no event cable
    ends at hears every source** (and the graph says so, with a link from each source node that there is to such a lane, as it has links for the audio path that nothing edits), and one that has some
    hears the sources they come from and no others (a device that is not there, unplugged, is not heard, and does not leave the lane to the others). In the file, the devices a session names are
    `[[device]]` tables (`name`, `ui`; the keys are `[ui] keys = { x, y }` and the cables that start at them) and the cables are `[[cable]]` tables, whose kind is told by their places as the audio
    cables' are (`from = "keys"` or `from = "device[0]"`, `to = "instrument[1]"`: an audio cable never ends at a lane and an event cable never ends anywhere else, and has no level); a device is
    found among the MIDI inputs by its name (the whole name first, and then the text in it, as `--midi-device` does). The intents are `AddCable` and `RemoveEdge` with ports of events, a step of the
    history each, the cables of a lane that goes written down and put back with it, the controller works the masks out from the cables and the numbers that the MIDI manager has given the devices
    (`DeviceWatcher::source_of`), sends `SetLaneSources` when either changes and `ReleaseSource` for a device that went, and `resync` tells the browsers when a device came. The oracle gets the
    scenario `edited:sources`.
    *Built* (2026-10-22, step 82 of `docs/web-ui.md`, in 5 commits with the docs). **The file has `[keys]` (a table with `ui`, and a cable that starts at `keys` makes it: not `[ui] keys` as the plan above had it), `[[device]]` (`name`, `ui`) and `[[cable]]` tables of notes** (`from = "keys"` or `"device[0]"`, `to = "instrument[1]"`, no `gain`;
    the audio cables and the cables of notes are one list and one series of ids, told by their places); the model has `SessionModel::sources` and `event_cables`; the graph has the node `Source` with a port `eventsOut`, a port `eventsIn` on a lane, the edges `Events { id }` and the derived `Hears` (from each source node to each lane that no cable ends at); the intents are `AddSource` and
    `AddCable`, `RemoveEdge` and `RemoveNode` for them, steps of the history that undo puts back at their places and under their ids; `session::device_name` is the one rule for a device's name, for the file and the intent. The controller works what each lane hears out (`lane_sources`: the keys are source 0, a device is the open ports that its name stands for, found by the whole name first and then by what is in it,
    and **none while it is not plugged in**) and sends `SetLaneSources` when it is not what the lane was told (`sync_lane_sources`) and `ReleaseSource` for a device that has gone (`DeviceWatcher::sources_of`; the plan wrote `source_of`). The MIDI manager notices a device that has come or gone (`midi::plan`: a port is its id and its name, so that a stale endpoint with no name does not hide a device that has gone)
    and, on macOS, sees the ports that are plugged in after the start (`midi::let_the_system_see_the_devices`: CoreMIDI tells a thread that does not run its run loop the ports that were there when it started and no others). The oracle has the scenario `edited:sources` (984 renders, 52 always on; the 981 older renders are the same bits) and a test through the real engine that a lane that is wired to a device lets go of the key it holds.
    The page's graph carries the nodes and the edges and its canvas leaves them out (`withoutNotes`); its mock engine refuses `addSource` and the ports of events. Rust 2318 tests (2502 with `web`), web 2431 tests. *Decisions (D82):* a source is a node that a person adds and none is there until one is (a session with none is the file it was); a lane that no cable ends at hears every source, and one that has some hears the sources they start at and no others; a device that is not plugged in is no source (the lane hears nothing, not everything); a device is a name, found by the
    whole name and then by what is in it; the keys are one source and one `[keys]` table; a cable of notes has no level; one series of ids and one list of cables; the controller works the sets out, since the engine has no cable of notes until P4f; the identity of a port is its id and its name; on macOS the manager turns the run loop once before it lists the ports. *What is not built:* the page and the terminal draw none (P4c, P4d), nothing tells a client which MIDI inputs are open, and no light shows what a source has sent.
  - **P4c, the page draws them.** The source nodes at the left, an output of events on each, a cable dragged to a lane's input of events, a menu for the keyboard (`From`), the lane that hears
    every source drawn as that, a light on a source that has sent something lately (a count of events for each source in the telemetry), the mock engine doing as the controller does.
    *Built* (2026-10-23, step 83 of `docs/web-ui.md`, in 5 commits with the docs). **Everything of the plan but the light.** A source is a node in a column of its own before the lanes (level with the middle of them), with an output of events on its right and the menu `♫`
    that sends its notes to a lane (`NotesToMenu`); a lane has an input of events on its left and the menu `♫` that takes the notes of a source into it (`NotesFromMenu`: not called `From`, since the button names what it does, "Take notes into X"); a cable of notes is
    dragged from the one to the other, the inputs that take it saying so, or chosen from either menu, drawn violet with the button that takes it out at its middle; **the link by which a lane that no cable ends at hears every source is drawn faint and dotted**, and a cable that is
    drawn to a lane takes its links off. The server says which MIDI inputs are open (`inputs { names }`: a message of its own, which the plan had as "a message of the open ports, as the catalog is"), the bar's "+ Source" offers the keys while the session has none and each open input
    that no device of the session stands for, and a device node says that it is plugged in or not by the controller's rule (`midi::matching`) written again in the page and pinned by a fixture that Rust writes. The mock engine does as the controller does, pinned by `fixtures/sources.json`
    (273 intents) and four scripts of `history.json`. *Decisions (D83):* see `docs/web-ui.md`. *What is not built:* the light on a source (a count of events for each source in the telemetry), a device that is not plugged in added from the page (it is added by a name that a client types), and the
    terminal's matrix (P4d).
  - **P4d, the terminal's patchbay has them:** a matrix of the sources down the side and the lanes across the top, in the Patch tab.
  - **P4e, time stamps.** A note is stamped when it arrives, against the audio clock, and played one block later, so that it has the latency of a block and no jitter (up to 5.3 ms at
    256 frames and 21 ms at 1,024 today: `NoteInput::send` stamps 0), and a note and a pattern share one clock.
  - **P4f, events as cables of the kernel.** A kind of port (`PortSpec::events`: a list of events with their frames, merged by time where cables meet, the pool and the executor and the
    reference interpreter knowing it, and the random patches drawing them), the sources and the head of a lane as nodes with ports of events, the mask of P4a become the cables that the plan has,
    and then the range, the remap, the transpose and the pattern as nodes of their own between a source and the instrument (§5's C3: a move of code that works), with an arpeggiator as the first
    node that is new.
  - **P4g, notes with names.** The note id (CLAP's and VST3's per-note expression, the built-in oscillator's voice found by it), so that a Seaboard's pressure and slide reach one voice of a chord
    (`CLAUDE.md`, "MIDI routing", says plainly that they cannot), and MIDI out.
- **P5, control.** Parameters as ports, modulators as nodes (the racks become layout), math nodes, the transport. *To try:* one LFO on two lanes, the quantizer
  on a cable, LFOs in phase with the beat.
  **Cut, 2026-10-23 (P4c and step 87 were done, and you said "lets move to P5" and, about the links of the audio path, "I dont think we should have the "no master" tracking per instrument/fx. if it
  has a cable (or more cables) it sends to those and if there is no cable than thats ok."; written here first so that it survives a restart; the steps after the first are cut again when they start, with
  what the code then says):**
  - **P5.0 (step 88), every link is a cable and none is tracked.** What a lane or a group links by itself (an instrument to its first effect, an effect to the next, the end of a chain to its group's bus
    or the mix, a group's own link to the mix) is drawn as a cable like the others, amber, with the × that takes it out, and is drawn again by dragging a cable between the same two ports. The
    buttons that said where a node's audio goes (`→ Master`, `no out`, `← Oscillator`, `no in`) are gone: a node sends where its cables go and nowhere if it has none, which is not a state of the node.
    An effect that stands by itself is added with no cable at all (a bare module, which cables go into and out of). *What it is and what it is not:* the links are still the template's (the model has
    `route` and `feed`, the file has them, and the plan makes the links from them), so the cable that the page draws for one is a *view* of it: it has no id and no level of its own (a send has
    both), and `AddCable` between the two ends of a link that is cut gives the link back (the controller turns it into `SetRoute` or `SetFeed`, which are the steps of the history it has always been)
    while between the two ends of one that is there it is refused (`there is a cable between those two already`). The engine, the file and the oracle are as they were; only `AddFreeEffect` changes what
    it makes (a lane whose way out is cut). **P5c makes the links cables with ids and levels, in the engine and the file, when the lane goes** (below): until then a lane's own links cannot be turned down, and cutting
    one is at once and not over a buffer (a click while a note rings). *Built in two parts.* **88a is built (2026-10-23: the controller's rule `link_or_cable`, the free effect made bare, the terminal's Patch tab with
    the links as `●` crosspoints, and the page's logic and mock engine; `CLAUDE.md`, "Links are cables")**; **88b is built (2026-10-26: the page's picture: every link is an amber cable with no slider and a × named
    `Remove cable A → B` that cuts it and puts the focus on the send menu of the node it started at, which draws it again; the buttons `→ Master`, `no out`, `← Oscillator` and `no in` are gone)**.
  - **P5a, targets by id in the model** (nothing to see, the same bits). A modulation target names the node and the port that it drives (`NodeId` and a parameter's index, a modulator's setting, the depth of a
    cable by its id) and not a place in a rack (a chain slot, a member's ordinal in a group, a sibling's index, a cable's place): the 20 functions that the model has to keep those places in step
    (`shift_*`, `fixup_*`, `adjust_*`, 304 lines) go with their tests, and the controller turns ids into the places that the audio thread's commands still name, as `endpoint_of` turns them back.
    **Built (2026-10-26, step 89 of `docs/web-ui.md`, in four commits).** *The safety net first:* the oracle holds the engine's racks (every modulator's cables by kind and depth) and what each cable is centred on to the
    model's after every block of every render (`Facts::racks`, `Facts::bases`: it found that the engine's depth of a cable whose depth another cable drives is the modulated value, so the depth is compared where no cable drives it), with four
    tests that show it can fail. *Then the model:* `ModTargetSlot` has `aim` (`Driven`: a plugin's node and parameter, a modulator's setting, the depth of a cable by its edge) and no `slot` and no `kind`; `SessionModel::kind_of(at, source, aim)`
    says the place the audio thread's racks name it by (`None` for what a rack does not reach, for a node that is not there and for a modulator's own settings), and is the one place that turns a node into a place: the controller's commands, the
    journal's restores and the file's `slot` use it. The 20 fix-ups of `model.rs` (8 `shift_*`, 8 `fixup_*`, 3 `remap_*` and `remap_index`: 304 lines) and the six lookups by place that went with them (`driven_by`, `drives_depth_of`,
    `group_member`, `member_of_rack`, `plugin_node`, `sibling_field`: 70 lines) are gone, with 14 of their tests; 9 functions of 109 lines are there (`kind_of`, `kind_of_target`, `plugin_slot`, `prune_dangling_cables`, `racks`, `cross_field`,
    `setting_of` and the `aim` of each of the two kinds of candidate target), and `model.rs` is 713 lines shorter (6,269 to 5,556). What a removal of a plugin, a lane or a modulator leaves pointing at nothing is taken out by one generic cascade
    (`prune_dangling_cables`, until nothing points at nothing) where there had been a function for each kind of place. *The loader* makes a cable of a file against the nodes there are: a rack's modulators are all in before any of its
    cables is made, so a cable can name a modulator or a cable's depth that comes later in the file, and a cable that drives nothing (a sibling that is not there, a modulator's own rate, the depth of a cable that is not made, a member's modulator that
    is not there) is **loaded without**, with a log line, where it had been kept with a place that meant nothing: the one change of behaviour, and only for files that have such a cable. The audio thread is as it was
    (its 31 place-keeping functions stay until P5b-1, which took them out: below), the commands and their order are the same, and so are the hashes of the whole corpus (999 renders, the stored file unchanged). 85 mutants of all that: 78 caught by the tests that were there, six gaps that are five tests now (and one extended), one that is no fault.
  - **P5b, control cables of the engine by id, and modulators that are not in a lane** (the same bits for every session the oracle has, a new scenario for what is new). The audio thread's targets name
    nodes too, found by the places that the plan makes (the 31 functions of `chain.rs` go); a modulator is a node of a session list, with the notes that it hears named (a lane's, a group's members',
    every source's), and drives any parameter of the session, any setting of any modulator and the depth of any cable. *To try:* one LFO on two lanes.
    **Cut, 2026-10-26 (P5a was done; written here first so that it survives a restart; cut again when each part starts, with what the code then says):**
    - **P5b-1, the engine names what a cable drives by node** (nothing to see, the same bits: the oracle holds it, as it held P5a's model). The engine's `Modulator` has the model's node and each of its
      cables the model's edge (`InsertModulator`, `AddModTarget` and `InsertModTarget` carry them, as `AddInstrument` carries a lane's node since P2), and `ModTargetKind` is `Param { plugin, param_index }`,
      `Setting { modulator, field }` and `Depth { edge }`: the audio thread finds the plugin in the lane or the bus whose node it is, and the modulator or the cable by its id, when it applies a rack
      (a few searches by id for each cable of each block, none of them allocating), where it found them by a place that 31 functions kept true (`fixup_*`, `shift_*`, `remap_*`, `adjust_*`, `update_*_base`: 443 lines of
      `chain.rs`, with their tests) and that the controller had to be told (`kind_of`, from P5a, goes from the commands). A cable whose plugin has gone is skipped by its rack and taken out by the controller, which
      sends the removal first, as it does. The oracle's `Facts::racks` and `bases` are the model's `Driven`s and centres exactly (no place to translate), the commands that name a rack and a modulator by place
      (`inst` and `mod_index`: the rack's order is the layout, and the facts hold the two to each other) stay until P5c. *Held by:* the whole corpus (999 renders) unchanged bit for bit, the engine's tests of
      what follows what (an effect that is taken out or moved, a lane that is swapped, taken out and put back, a member that joins or leaves a group, a modulator that goes in first) written again as "the cable
      is on the plugin it was on", and the mutants.
      **Built (2026-10-26, step 90 of `docs/web-ui.md`).** As cut, with one difference: the engine does not have a kind of its own, it has the model's `Driven` (which moved to `src/ids.rs` with `ModSetting`, and `app/model.rs`
      re-exports them), so that there is one vocabulary for what a cable drives and nothing to translate between the engine and the model; `ModTargetKind` and `CrossModField` stay as the places that the file and the terminal's
      popups name a choice by, and `SessionModel::kind_of` as the check of what a rack reaches. The engine's `ModTarget` has `edge` and `aim` and its `Modulator` has `node`; the audio thread finds a plugin by its node among the
      lane's own (`slot_in_chain`), a group's bus effects and its members' (`locate_in_group`), and a modulator or a cable by its id (`sibling_field`, `member_modulator_field`); `AudioGraph::centre` replaces the four
      `update_*_base`. 32 functions and 531 lines of `chain.rs` are gone (the 31 that were counted, and the engine's copy of the member ordinal), with 54 tests; ten functions and 11 tests came. A cable whose node is not there
      is skipped and publishes nothing, and does not go to what comes to its place. The whole corpus (999 renders) is the same bits, the stored file unchanged; the oracle's facts compare the engine's racks with the model's exactly.
      53 mutants of all that, in two runs: of the first 47, 39 were caught by the tests that were there, seven were gaps (six tests now, and a controller test that reads the node out of the commands) and one is no fault (a guard that did nothing, which is out of the code); six more for the calls of `centre` that had none; every one is caught.
    - **P5b-2, a modulator that is in no lane** (the model, the file, the intents, the history, the engine and the terminal). `ModulatorLocation::Free`: a session list of modulators (`SessionModel::free_modulators`,
      `AudioGraph::free_modulators`), applied at the top of the block, before the groups' and the lanes' (the nearer to the plugin, the stronger: a lane's own modulator on a parameter wins over a group's, which
      wins over a free one, as a group's has always won over nothing; two free modulators on one parameter add up). It hears **every source and every note** (no key range, nothing to filter by: a cable
      of notes into a modulator, to hear one keyboard, is for later), drives any parameter of any plugin of the session (`Param { plugin }` is by node, which is what lets it reach two lanes), the settings
      of any free modulator and the depth of any free cable (settings and cables of a lane's or a group's modulators: later). The intent is `AddFreeModulator { kind }` (as `AddFreeEffect` is for an effect: a
      step of the history, undone whole), `AddModEdge` and `Modulate` take a free modulator as they take a lane's, `RemoveNode` takes it out with its cables, and the graph a client gets has the node with no
      parent. The file writes it as a top-level `[[modulator]]` with the same keys as a lane's, and each cable as `[[modulator.target]]` with `to = "instrument[0].effect[1]"` (the place of the plugin, as an
      audio cable names its ends: `CablePlace`) and `param`, or `to = "modulator[1]"` and `mod_rate`..., and `depth`. The terminal's tree has a section for them, in magenta, above the first lane (`m` makes
      one with no lane selected, `t` lists every parameter of the session). *To try:* `+ LFO` in the bar, a cable from it to a knob of one lane and another to a knob of another lane.
      **Built (2026-10-26, step 91 of `docs/web-ui.md`).** As cut, with these differences. The ten pairs of commands that address a rack (`InsertModulator` and `InsertGroupModulator`, `AddModTarget` and `AddGroupModTarget`, …) were made ten first, naming the rack by
      `ModulatorLocation` (P5b-2a, `e40873c`, a refactor held by the oracle), so that the free rack was a variant and not ten commands more. A free cable's place in the file is **`to = "instrument[0].effect[1]"` and `param`** as cut, but a cable to a sibling free modulator has no `to`:
      it is `mod_rate = 1`, `mod_attack`…, `mod_depth = [m, c]` as a lane's (the plan's `to = "modulator[1]"` named what the key names already). The terminal's key is **`F`** (and `m` on a free modulator adds another), not `m` with no lane selected: the cursor of the tree is always on
      a lane's, a group's or a free modulator's entry. The place form is `ModTargetKind::FreeParam { plugin, param_index }`: the file and the terminal's popups name a free cable by the plugin's node and its parameter's own index, where a lane's and a group's name a slot, a member or a bus
      effect, and `kind_of(Free, ..)` is the check of what a free rack reaches (any plugin of the session, and the settings and cable depths of the other free modulators, which is the plan's: "settings and cables of a lane's or a group's modulators: later"). The free modulators tick first in
      `process_events` with `Events::all()` (every message of the block whatever its source), `apply_cross_mod` resolves the cables between them, and `apply_free_modulators` sets each parameter to the user's value plus the sum of the offsets of the cables on it, held to the parameter's range, after finding
      its plugin anywhere in the graph (`locate_anywhere`); a group's modulator and a lane's write after and win, each from the user's value and its own offsets alone (the plan's "the nearer to the plugin, the stronger"). `AddFreeModulator { kind }` is a step of the history (undo takes it out
      by `RemoveNode`); `Modulate` still makes a lane's or a group's modulator; `remove_lane` and `remove_group` take the free cables to their plugins out first, written down (`free_cables_into`), and the other removals already took every cable to a plugin (`cables_into_plugin` looks in every rack). The loader's
      `load_free_modulators` shares `make_rack` with the lane's and the group's. The page and the mock engine are P5b-3: the mock refuses the intent for now. An oracle session (`freemod`) and a scenario (`edited:freemods`, 19 edits) hold it: 1005 renders, the 999 older the same bits; the loop that the plan replaced
      does not tick free modulators, so the scenarios that have any are not held to it. `examples/shared-lfo.toml` is the case to try (one LFO, two lanes' filters), by the terminal. 87 mutants of it: 77 caught by the tests that were there, one by a hang, nine gaps that are six tests and an assertion now.
      **P5b-3 built (2026-10-27, step 92 of `docs/web-ui.md`).** As cut, with these differences. The bar has `+ Modulator` (a fifth button: the `Add ▾` that would take the buttons back to one is a change of its own), a menu of the kinds that a lane's "+" has, asking `AddFreeModulator`; the free modulators are a **row above the first lane** (a cable runs down to the knob it drives, and a modulator that
      drives a setting of another is a row above it, as in a lane's rack), laid out by `toFlow` with no room taken when there are none, and put above the first lane where that lane stayed when a canvas that has been laid out is given one. `reach.ts` has `ownerOf`, and `reachFrom` of a modulator that has no parent is every plugin of the session and `modulatorsInReach` the other modulators like it, so that the list at the foot of its node, a dragged cable and
      the `~` menu's "Attach" of **any** knob offer it; the knob's ring is the nearest rack's (`modulationsOn`). What the plan had as "`+ Modulator` in the bar, a free modulator among the Attach choices of a knob's `~` menu" is built; its "New …" still makes a lane's or a group's modulator (making a modulator that is nobody's for a knob is P5c's, when there is no lane). The mock engine has the intent and is pinned by three
      new scripts of `history.json` and by `freeReach.json`, which has the controller's reach for every modulator of a session with groups, lanes and two that have no parent. `e2e/freemodulators.spec.ts` has the page on the mock engine and the real program (the knobs of two lanes move with one LFO, and the file keeps the table). A crash that the browser tests of the real program found, in the MIDI manager since P4b (`let_the_system_see_the_devices`
      turned the main thread's run loop, where a JUCE plugin that the catalog scan was taking down has the source of its message queue), is fixed in its own commit (`29923ff`).
    - **P5b-3, the page draws them** (the layout, the editor, the mock engine and the browser specs). A free modulator is a node with no strip (a row of its own over the canvas, or where its `ui` says), its
      output has a cable to any knob of any node, and the bar has `+ Modulator`; the "Attach" list of a knob's `~` menu offers the free modulators that are there. The mock engine does what the engine and
      the controller do, pinned by fixtures that Rust writes, as for every step before it.
  - **P5c-0, every modulator global (the user's direction of 2026-10-27: "I think all modulators should be global like this, is that possible").** It is. A modulator of the session's own reaches everything that a lane's or a group's does (any plugin of the session) and lacked exactly one thing, **an ear**: a lane's modulator hears
    the notes that its lane's instrument is given (after the source filter, the key range, the remap, the pattern and the transpose, and the MPE routing of the member channels), a group's those of its members, and a free one heard every event. Staged so that nothing built is broken, each step held by the oracle:
    - **P5c-1, the ear (built 2026-10-27, step 93 of `docs/web-ui.md`).** `Modulator::ear` (the node of a lane, which is its instrument's node, or of a group; none is every event, and so is a node that went), `GraphCommand::SetModulatorEar`, `ModulatorSlot::ear`, `Intent::SetModulatorEar`, `hears = "instrument[0]"` in the
      file and `hears` on the modulator's node in the graph, a ♫ menu on the page's node (a cable from the lane would be a larger change of the canvas, and is a TODO) and `h` in the terminal. The lane's note pipeline is a function now (`InstrumentLane::prepare_events`), run ahead of the lane's head for a lane that a free modulator
      hears and by the head itself for one that none does, so that **an older session renders the same bits** (the 1005 renders of the oracle, and six more for `freeear` and `edited:freeears`). An envelope that is made to hear other notes lets go of the ones it held. A mutation run and a failure path found by reasoning (a block that ends early left
      a lane prepared for the next block) are in the step's entry.
    - **P5c-2, what is made is global (built 2026-10-27, step 94 of `docs/web-ui.md`).** `AddFreeModulator { kind, hears }`, `Modulate` (the knob's "New …"), the "+" of a lane and of a group and the terminal's `m` make a modulator of the session's own with the ear of the knob's lane or group (its cables are by node already); `AddModulator { parent }`
      stays an intent for the oracle and the tests. A loaded session's lane and group modulators are **moved into the session's rack with the ears they had** (`SessionConfig::move_modulators_to_the_session`, called after `session::load` by every mode: the file's `[[instrument.modulator]]` and `[[group.modulator]]` are read as before and moved, with their cables
      written as a free modulator's, and a save writes `[[modulator]]` with `hears`: the one change to the file format that this makes, with the old read for ever), the legacy racks staying compiled for one more step so that the oracle holds a moved session to the one that is not: **bit for bit but for three things that are decisions** (`Scenario::moved`;
      the corpus is moved, `unmoved()` is the file as written; 30 of 1011 renders, in 10 sessions, sound other than they did, and 648 more only have the cells of the telemetry in another order): **two modulators on one parameter, a lane's and a free one (or a group's), add up** (a lane's won over a group's, which won over a free one's: none of the user's sessions has the case,
      scanned), **a group modulator's cable to a member's modulator acts one block later** (the cables between the session's own are resolved after all of them have ticked; the plan of step 93 said the same block, which is the wrong way round: a group's cable to a member's modulator was applied before the member's ticked, and is applied after now) and **a group that is taken out, or a lane
      that leaves one, no longer takes the cables of what were its modulators to the members with it** (a modulator of the session's own keeps its cables by node; the ones that heard the group or the lane hear every note, and Undo gives the ear back). The moved benchmark's stress session costs what the unmoved one does (811 µs a block of 256 frames with a chord of six, against 806 µs and 804 µs for the loop that the plan replaced), with no allocation.
    - **P5c-3, the racks go (built 2026-10-28, step 96 of `docs/web-ui.md`; the user's "Yes, les do the dimming and then 96").** `ModulatorLocation::{Lane, Group}`, `InstrumentNode::modulators` and `GroupNode::modulators`, the engine's lane racks and group-modulator pass, `ModTargetKind::{PluginParam, GroupMember, GroupBus, GroupMemberMod}`, `group_cables_into_lane`, the loader's lane and group plans, the intent `AddModulator { parent, kind }` and the old form's writers
      are taken out of the model, the engine, the controller, the terminal and the page (9,293 lines of Rust and 1,513 of the page's, tests included, against 2,434 and 1,114 written): every modulator is the session's own (`ModulatorLocation::Free` stays as the one place that the commands name, until P5c makes them address a modulator by its node), `session::parse` moves a file's `[[instrument.modulator]]` and `[[group.modulator]]` (which are read for ever and not written)
      and the loader refuses a lane or a group that still has any. **The oracle's whole corpus renders the same bits** (the stored hashes are untouched), and the loop that the plan replaced, which could run a lane's and a group's racks and cannot run the session's own, is held to the 4 always-on scenarios that have no modulator (and the same share of the whole corpus): the modulated ones are held to the stored hashes, which the loop held them to until step 94.
      Behaviour that changed on purpose is D96 of `docs/web-ui.md`: `m` on a modulator in the terminal adds one that hears what it hears, the keys of "the lane or group this entry is of" go on a modulator's entry to the lane or group it hears, the ear's refusal is `only a modulator has an ear`, `SetLaneGroup` keeps the cables of the modulators that hear the group and is always a step of the history, and a holder's "×" takes the lane when the effect is on its own whether or not modulators hear it.
      The benchmark's stress session (32 modulators, all the session's own) costs 833 µs a block of 256 frames with a chord of six (811 µs moved and 806 µs unmoved at step 94: two runs). Then **P5c proper** below, when the lane dissolves.
  - **The values that the host keeps are ports (built 2026-10-28, step 97 of `docs/web-ui.md`; the user's "I cannot modulate everything, for example mix and some volume, is that fixable?").** A lane's volume, an effect's mix and a group's volume and pan are control ports of their nodes that
    modulation cables end at (`ModGain`, `Driven::Gain`, the ports `volume`, `mix` and `pan`, `gain = "mix"` beside `to` in the file), as a plugin's parameters are (named by the plugin and its index): in the netlist of P5c they are three of a node's control ports, and the lane's `volume = …` and the effect's `mix = …` are their values. The engine keeps the user's value and carries what the cables make of it for one buffer as a ramp (`volume_ramp`, `mix_ramps`, `pan_ramp`: the goal is the user's value and the sum of the offsets, held to the range, as for a parameter);
    a lane that has no instrument has no volume to drive. **The oracle's 1017 older renders are the same bits** (a session in which no cable ends at such a value has no drive and no ramp), and `gains` and `edited:gains` are six renders more.
  - **The audio input, before P5c (step 99).** The user's microphone ("for future (after this) I want to be able to add my computer microphone as an input that I can connect to other devices via audio cable.") is built as **an instrument plugin in a lane that starts bare** (`builtin:input`, `Plugin::starts_bare`, `route = "none"`), and not as the source node of the "Later" list above: the engine's audio cables start at a lane's instrument, an effect or a group, and a source node of its own is a change of the wiring, the ids, the file and the page that P5c makes for every node at once (in P5c's netlist it is a node with an output port and no input, as the keys and a MIDI device are nodes with outputs of events, and the lane that holds it today is the sugar that dissolves). A thread owns the cpal stream and fills a lock-free ring that the audio thread reads a block at a time; the first block starts it and a second without one stops it; the null backend gives it a tone. **The oracle's 1023 older renders are the same bits**; `input` and `edited:input` are six renders more.
  - **The groups hidden from the page, before P5c (step 103).** The user's "I think we should remove the groups (at least wihin the UI)" and, to the proposal of hiding them, "2. yes hide groups": the page no longer makes a group (no "+ Group" in the bar, no "new group" in a lane's choice) and shows a lane's choice of group only while the session has one; a group that a file has is still drawn, so that nothing sounds that cannot be seen. Nothing below the page changes (the terminal's `g`, the protocol, the controller, the model, the file and the engine are as they were), and the group leaves them in P5c, as the next bullet says: a loaded group becomes a lane that holds its bus effects, with a gain-and-pan module for its volume and pan (a built-in that does not exist yet). `examples/shared-reverb.toml`, the one example that had a group, has its reverb in a lane of its own, which renders the same bits.
  - **A chain of effects as a module, before P5c and P6 (step 104).** The user's "In the future, I want to be able to design a set of modules, save them as a stand-alone module that you can then load inside another instrument, like a submodule doing FX etc, (imagine input->reverb->delay->output) and you can load that as a module in another graph": the first half of it is built on the lane as the model has it. **The chain of effects of a lane is saved as a module** (`Intent::SaveModule`: a session file of a lane that has no instrument, its effects in order, each with its preset, values, mix, pins and feed, and the audio cables between them; one file for each module in a library directory, `~/.config/tang/modules/`), **listed** in the catalog as `module:<name>` and **added** whole, bare, in a lane of its own, by `AddFreeEffect` (one step of the history). It is a copy, with no modulators and no source of notes. The **sub-graph** that the user describes (any nodes, with the input and output ports that the module shows, loaded as one node of another graph, by reference, so that a module is edited in one place) is P6's "modules and a library of them", which needs P5c: with no lane in the model a module is a netlist with ports that are exposed, and what step 104 built is what that needs as well (the library of files that a person names, the catalog's entries, the intents and the history's words, and a file format that is the netlist's chain with its cables).
  - **Any plugins that are chosen, with the cables between them, as a module, before P5c and P6 (step 105).** The user's picture of three effects in lanes of their own joined by cables, "the goal is to save the graph of fx or instruments ... into its own module": what step 104 tied to P5c was a module as one node with ports, and by reference; **a module as a copy of nodes with the cables between them** needs neither, since the lane is only how the model holds a plugin (an effect that stands by itself is a lane with no instrument) and a session file holds any number of lanes with cables between their plugins. `Intent::SaveSelectionAsModule { nodes, name }` saves the instruments and effects that are chosen, from any lanes, with the audio cables that have both ends among them (the link after a stage that was left out is cut in the module); `AddFreeEffect { plugin: "module:<name>" }` adds it as a lane for each of its own, bare, in one step whose undo takes every lane out; the page chooses with a Select button and a box. **P5c** makes the lane go and a module a set of nodes (the file's lanes become nodes with ports); **P6** makes it a node with ports in another graph, by reference, with the module edited in one place. Not built at step 105: modulators in a module (a modulator that was chosen was left out and counted; step 106 builds them).
  - **The modulators that are chosen with the plugins, in a module, before P5c and P6 (step 106).** The user's "yes agree LFOs should be next!" (an LFO on a filter that is in the module was the likeliest thing that step 105 left out): the modulators are the session's own (step 96), so a module can hold them as it holds plugins, with nothing new in the model. `SaveSelectionAsModule` keeps the modulators that are chosen, each with the lane of the module that it hears (every note if it heard a group or a lane that is not in the module) and the cables whose ends are in the module (a parameter by name, a lane's volume, an effect's mix, a setting of another modulator, the depth of a cable that stays: a cable that drives the depth of one that goes goes with it); `AddFreeEffect` makes them after the lanes and their audio cables, in the same step, with the cables to a depth made in sweeps. **P5c** makes a modulator an ordinary node with an output (its ear a cable of notes into it); **P6** makes a module a node with ports in another graph, by reference. Not built: a source of notes or a cable of notes in a module.
  - **P5c, the lane and the group dissolve** (the file and the model). The model has nodes and cables and no lane: the way out and the chain are cables with ids and levels (the flags `route` and `feed`
    go, the engine's `cut_feeds` and `Route` too, the lane's tail is a strip that cables go out of), the file is a netlist with the sugar lowered when it is loaded and raised when it is saved
    (§11.5), and a cable that is taken out or drawn is over a buffer, not at once. Group membership goes with the scope of modulators (P5b): a bus is a node with fan-in.
  - **P5d, math nodes and the transport.** Scale and offset, multiply, slew, sample and hold, quantize to a scale (the oscillator's `quantize` as a node that goes on any cable), min and max,
    switch, random; a transport (tempo, beat and bar, start and stop) that tempo-synced things lock to. *To try:* the quantizer on a cable, LFOs in phase with the beat.
- **P6, modules and voices.** Modules and a library of them; the `Patch` node on the kernel (the kernels of `docs/track-b.md` §7, B0, first).
- **P7, scale.** Skipping silence and tails, the multicore executor, the DSP meter on every node (and the sandbox, if you want it).

*Bit for bit* where the order of the floating-point work is the same; where a chosen change alters it (a mix fused with its compensated dry, a sum in another
order) the test names it, with the reason and a tolerance, in a list that is reviewed with the change. The page's fixtures are not regenerated until the new
engine agrees with the old. Two more tests that the old engine cannot give, since it has no such graphs: a **reference interpreter** (slow and obviously right:
a buffer of its own for every cable, the nodes in a plain order) that the kernel must equal over random graphs, which finds the bugs of colouring, latency,
feedback cuts, fusion and parallel order; and a **click detector**: a sine rendered through random sequences of edits (cables added and taken out, nodes
inserted and removed, plans swapped), asserting that the output has no jump beyond a bound.

P0 to P2 change nothing you can see, and each makes the code smaller; the first new thing is P3. If you ever want a cable sooner, E1 stays possible, at the price
of throwing its engine half away.

### 11.7 §9 again, and what is new

| # | §9 said | Now |
|---|---|---|
| 1 | E1 first | **E2, by the path of §11.6.** E1's page, file, intents and Undo carry over; its engine half is the detour. |
| 2 | audio before MIDI | audio cables (P3), then events (P4): both stand on the kernel, so the order decides only what you see first. |
| 3 | cables leave from the end of a lane | from any node's output: in a graph a tap is a cable. |
| 4 | refuse cycles | refuse them, except through a feedback cable (one block of delay, drawn dashed, with the latency said). |
| 5 | no latency compensation | in P3: it is a property of the compile, and the plugin contract gains `latency`. |
| 6 | a cable into a group is a membership | there are no memberships: a bus is a node with fan-in, and the reach of a group's modulators goes with the scope of modulators. |
| 7 | a sum, with a gain on each cable | the same. |
| 8 | the law of mute and solo | the same law, stated for strips and paths; the compile gives each strip the gain it is heard at. |
| 9 | old sessions unchanged | the same: the sugar stays, and round-trips (§11.5). |
| 10 | the range on the lane, then the cable | a range node (a cable's own range is the same node, fused). |
| 11 | one source for the computer's keys | the same. |
| 12 | lists in the terminal | lists *and a patchbay matrix* (§11.4). |

New, with my recommendation (**all taken, 2026-10-20**: *"lets go e2! I like this. I like it all!"*):

13. **The file.** The netlist as the model's form, with the sugar kept and round-tripped (§11.5). *Recommend yes.* The other way, the netlist only, makes every
    session of today a different file.
14. **The event.** A richer event inside (note ids, expression), with MIDI at the edges. *Recommend yes:* it is what per-note expression needs, and a MIDI
    event inside would have to be translated back out of for every format that has notes by id.
15. **Polyphony.** Instances of the inner graph inside `Patch` nodes, the session graph stereo. *Recommend that.* The other way is poly cables through the whole
    session (a cable of up to 16 channels, as in VCV Rack): faster for builtin nodes and a heavier model, and a hosted plugin cannot be part of it.
16. **Cores.** Designed for from P1 (the order, the fixed sums), built last, a plugin pinnable to one thread. *Recommend that;* the alternative is one thread for ever.
17. **Crates.** The kernel, the nodes and the host adapters in a crate of their own that has no interface in it, from P1. *Recommend yes:* it is how the
    kernel is tested, benchmarked and fuzzed alone, with no interface to build.
18. **Library or ours.** Our own kernel for the session graph (a library would not know hosting, events, latency, taps or ids), and FunDSP considered only for
    the DSP inside `Patch` nodes, after the experiment that `docs/track-b.md` §8.6 asks for. *Recommend that.*

### 11.8 Risks

- **Too general to use.** A graph editor that lets you wire anything is harder than strips that cannot be wired wrongly. The templates must make today's session
  with no cable drawn, the page must open on the strips, and the graph must be something you grow into.
- **Compile correctness** (colouring, latency, feedback, fusion, parallel order): the reference interpreter and the click detector, and a plan that is checked
  before it is sent.
- **Events.** The note models of the three plugin formats differ, and the adapters are where the bugs will be; each gets tests with the real plugins on this
  machine (Pianoteq, the ROLI ones), as the `#[ignore]`d tests do.
- **Tails and swaps.** A node that is removed loses its tail, and one that is re-routed keeps it; the ramps hide a step and not a long reverb being cut. The page
  has to say which it is (taking a reverb out is not the same as bypassing it).
- **The size of the flip.** The engine is the smaller half: `src/app/` is about 9,300 lines of code and 22,600 of tests, and the page has its mock engine
  (1,774 lines), `toFlow` (725) and 17 fixtures that pin them to the controller. The tests are the net, and the accessors that stay as views are how it
  stays one net.
- **The layout of an arbitrary graph** in the page: a layered layout for the graph-shaped parts, kept places, the strips for the templates.
- **Real-time threads** (priority inversion, wake-up time, power): last, off by default until it is measured on this machine.
- **Scope.** P5 to P7 are each a feature of their own. P0 to P3 first, because everything else stands on them.
