# A patch graph for the session: audio and MIDI as cables

**Status: a proposal. Nothing of it is built, and none of it will be started before you have decided what §9 asks.**
Written 2026-10-20 at the end of step 67 of `docs/web-ui.md`, from the code as it is then. It starts from what you said that day: *"something we need
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
- **Later**: an audio input as a source (`CLAUDE.md`, "Future ideas"), and more than one output.

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
intents, file and drawing that E1 needs are all that E2 would ask of the model and the page.

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

Each has my recommendation and what changes if you choose otherwise.

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
