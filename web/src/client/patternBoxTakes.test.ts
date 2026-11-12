import { describe, expect, test } from "vitest";

import type { PatternNote, PatternStatus, Snapshot } from "../protocol";
import { patternBoxTakesFixture, patternBoxesFixture } from "./fixtures";
import { MockConnection, MockServer } from "./mock";

// What the mock engine makes of takes is what the real player makes of them (`fixtures/patternBoxTakes.json`, written by `BoxPlayer` itself in `patternbox::tests`): the same scenes are done to a box of the mock, at the same beats, and what the box is doing is
// looked at after each sixteenth of a beat, and the notes that it has. The box is the plain one of `patternBoxes.json` (no cable of notes ends at it: it records every input, the keys), whose loop, settings and notes are the scene's.
const BOX = 5;
const KEYS_NODE = 2;
/** 120 beats a minute: a beat is half a second. */
const BEAT = 0.5;
const GRID = 16;

/** A value as text, the keys of an object in order of their names (the player's JSON has them so, and the mock's notes in the order they were made in). */
const canonical = (value: unknown): string => JSON.stringify(value, (_key, part: unknown) => (part !== null && typeof part === "object" && !Array.isArray(part) ? Object.fromEntries(Object.entries(part).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) : part));

/** The session of a scene: the box made as the scene says. */
function sceneSnapshot(scene: (typeof patternBoxTakesFixture.cases)[number]): Snapshot {
  const base = patternBoxesFixture.cases.find((each) => each.name === "boxes")?.snapshot;
  if (!base) throw new Error("the fixture has a session with boxes");
  return {
    ...base,
    graph: {
      ...base.graph,
      nodes: base.graph.nodes.map((n) => (n.id === BOX && n.kind === "pattern" ? { ...n, lengthBeats: scene.length, looping: scene.looping, overdub: scene.overdub, notes: scene.notes } : n)),
    },
  };
}

/** What the mock does with a scene: what the box is doing after each sixteenth of a beat (each change, with the beat), and the notes it has then (each change, with the beat). */
function played(scene: (typeof patternBoxTakesFixture.cases)[number]) {
  const time = { seconds: 1000 };
  const server = new MockServer({ snapshot: sceneSnapshot(scene), clock: () => time.seconds });
  const connection = new MockConnection(server);
  const box = (): { status: PatternStatus; notes: PatternNote[] } => {
    const found = server.snapshot.graph.nodes.find((n) => n.id === BOX);
    if (found?.kind !== "pattern") throw new Error("the box is there");
    return { status: found.status, notes: found.notes };
  };
  const statuses: [number, PatternStatus][] = [[0, box().status]];
  const changes: { beat: number; notes: PatternNote[] }[] = [];
  const blocks = Math.round(scene.beats * GRID);
  for (let block = 0; block < blocks; block += 1) {
    const beat = block / GRID;
    // (What is done at this beat is done at the start of the block: the transport first, then the keys, as the scene lists them.)
    for (const op of scene.script.filter((each) => each.beat === beat)) {
      if (op.do === "on") server.receive(connection, JSON.stringify({ type: "noteOn", note: op.note, velocity: op.velocity }));
      else if (op.do === "off") server.receive(connection, JSON.stringify({ type: "noteOff", note: op.note }));
      else server.apply({ type: "patternBoxTransport", node: BOX, action: op.do === "finish" ? "finishRecording" : op.do });
    }
    time.seconds = 1000 + ((block + 1) / GRID) * BEAT;
    const before = box();
    server.settle();
    const after = box();
    if (after.status !== statuses.at(-1)?.[1]) statuses.push([(block + 1) / GRID, after.status]);
    if (canonical(after.notes) !== canonical(before.notes)) changes.push({ beat: (block + 1) / GRID, notes: after.notes });
  }
  return { statuses, changes, final: box().notes, server };
}

describe("a box of the mock engine", () => {
  test("does what the real player does with each scene of the fixture: what it is doing after each sixteenth of a beat, the notes that each pass that ended made of it, and the notes it has at the end", () => {
    const wrong: string[] = [];
    for (const scene of patternBoxTakesFixture.cases) {
      const got = played(scene);
      if (canonical(got.statuses) !== canonical(scene.statuses)) wrong.push(`${scene.name}: statuses are ${JSON.stringify(got.statuses)}, not ${JSON.stringify(scene.statuses)}`);
      // (A pass that makes the notes that the box has already is no change of the session: the mock tells only what changed, so those of the real player's passes that did not change them are left out.)
      let had: PatternNote[] = scene.notes;
      const real = scene.passes.flatMap((pass) => {
        const changed = canonical(pass.notes) !== canonical(had);
        had = pass.notes;
        return changed ? [pass] : [];
      });
      if (canonical(got.changes) !== canonical(real)) wrong.push(`${scene.name}: the notes changed ${JSON.stringify(got.changes)}, and the real player's passes say ${JSON.stringify(real)}`);
      if (canonical(got.final) !== canonical(scene.final)) wrong.push(`${scene.name}: the notes at the end are ${JSON.stringify(got.final)}, not ${JSON.stringify(scene.final)}`);
    }
    expect(wrong).toEqual([]);
  });

  test("has been tried with every way a box records and plays: the fixture is not thin", () => {
    const scenes = patternBoxTakesFixture.cases;
    expect(scenes.length).toBeGreaterThanOrEqual(14);
    const statuses = new Set(scenes.flatMap((scene) => scene.statuses.map(([, status]) => status)));
    for (const status of ["stopped", "playing", "countingIn", "recording"] as const) expect(statuses.has(status), status).toBe(true);
    const does = new Set(scenes.flatMap((scene) => scene.script.map((op) => op.do)));
    for (const op of ["record", "play", "stop", "finish", "on", "off"] as const) expect(does.has(op), op).toBe(true);
    expect(scenes.some((scene) => scene.overdub && scene.passes.length >= 2)).toBe(true);
    expect(scenes.some((scene) => !scene.looping && scene.passes.length === 1)).toBe(true);
    expect(scenes.some((scene) => scene.notes.length > 0 && scene.passes.length === 0)).toBe(true);
    expect(scenes.some((scene) => scene.statuses.some(([, status], i) => status === "countingIn" && scene.statuses[i + 1]?.[1] === "stopped"))).toBe(true);
    // (A take that is a whole pass of notes: every note of it has a length and a velocity.)
    for (const scene of scenes) for (const pass of scene.passes) for (const each of pass.notes) expect(each.length > 0 && each.velocity >= 1, scene.name).toBe(true);
    expect(KEYS_NODE).toBe(2);
  });
});
