import { describe, expect, test } from "vitest";

import type { Intent } from "../protocol";
import { filenamesFixture, tracerSnapshot } from "./fixtures";
import { MockServer } from "./mock";

// Saving the session as another file (`Intent::SaveAs`), against the mock engine: what the controller does (`save_as_intent`), with a directory that the mock has the
// files of. The words of a refusal of a name are the controller's (`filenames.test.ts` has them all).
const saveAs = (name: string): Intent => ({ type: "saveAs", name });
const fresh = (files?: string[]): MockServer => new MockServer({ snapshot: tracerSnapshot, ...(files ? { files } : {}) });

describe("the mock engine's save as", () => {
  test("is told as a save is, with the name the session has now, and the session has that file from then on and nothing unsaved", () => {
    const server = fresh();
    server.apply({ type: "setLocked", locked: true });
    expect(server.snapshot.meta).toMatchObject({ file: "tracer.toml", dirty: true });
    const told = server.apply(saveAs("copy"));
    expect(told).toEqual({ events: [{ type: "saved", file: "copy.toml", rev: 2 }], restructured: false });
    expect(server.snapshot.meta).toMatchObject({ file: "copy.toml", dirty: false });
    expect(server.snapshot.rev).toBe(2);
  });

  test("is a save, for the name the session has already (with the extension or without it, spaces round it), though that file is there", () => {
    const server = fresh();
    for (const given of ["tracer", "tracer.toml", "  tracer  "]) {
      server.apply({ type: "setLocked", locked: !server.snapshot.meta.piano.locked });
      expect(server.apply(saveAs(given)).events).toMatchObject([{ type: "saved", file: "tracer.toml" }]);
      expect(server.snapshot.meta.dirty).toBe(false);
    }
  });

  test("is refused for the file of another that is there, in the controller's words, and nothing changes", () => {
    const server = fresh(["other.toml"]);
    server.apply({ type: "setLocked", locked: true });
    const before = server.snapshot;
    for (const given of ["other", "other.toml", " other "]) expect(() => server.apply(saveAs(given)), given).toThrow("a file called other.toml is there already");
    expect(server.snapshot).toBe(before);
  });

  test("leaves its own old file in the directory: another save as to the name the session had is refused, since that file is there", () => {
    const server = fresh();
    server.apply(saveAs("copy"));
    expect(() => server.apply(saveAs("tracer"))).toThrow("a file called tracer.toml is there already");
    // (And what was written is there too: a save as to the name it had is a save.)
    expect(server.apply(saveAs("copy")).events).toMatchObject([{ type: "saved", file: "copy.toml" }]);
  });

  test("leaves each file it wrote in the directory too: a save as to the name of one of them, from another file of the session, is refused", () => {
    const server = fresh();
    server.apply(saveAs("first"));
    server.apply(saveAs("second"));
    expect(server.snapshot.meta.file).toBe("second.toml");
    expect(() => server.apply(saveAs("first"))).toThrow("a file called first.toml is there already");
    expect(() => server.apply(saveAs("tracer"))).toThrow("a file called tracer.toml is there already");
    expect(server.snapshot.meta.file).toBe("second.toml");
  });

  test("is refused for every name of the controller's fixture that is no plain file name, in its words, and is the file it makes for the others", () => {
    for (const entry of filenamesFixture.names) {
      const server = fresh();
      if ("error" in entry) {
        expect(() => server.apply(saveAs(entry.given)), JSON.stringify(entry.given)).toThrow(entry.error);
        expect(server.snapshot, JSON.stringify(entry.given)).toBe(tracerSnapshot);
      } else {
        expect(server.apply(saveAs(entry.given)).events, JSON.stringify(entry.given)).toMatchObject([{ type: "saved", file: entry.file }]);
      }
    }
  });

  test("is no step of the history and does not forget it", () => {
    const server = fresh();
    server.apply({ type: "setLocked", locked: true });
    const history = server.history;
    expect(history.undo).not.toBeNull();
    server.apply(saveAs("copy"));
    expect(server.history).toEqual(history);
    server.apply({ type: "undo" });
    expect(server.snapshot.meta.dirty).toBe(true);
  });
});
