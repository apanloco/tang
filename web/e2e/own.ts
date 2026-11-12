// A program of its own, for a test that changes what the shared one could not be put back from: a lane
// cannot be taken out from the page, so a test that adds one starts a program that is thrown away with
// it (on the null audio backend, so nothing is heard, and a copy of the tracer in a directory of its own,
// which the test may save). The program serves the page itself, as `embedded.spec.ts` has it.
// The page has no way to make a group (step 103), so a test that is about what the page does with one starts the
// program on the tracer's file with groups added to it (`testWithGroups`: a group has no key but its name, and
// is called by its place, `Group 1`, when it has none).
import { spawn } from "node:child_process";
import { closeSync, mkdtempSync, openSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";

import { test as base } from "@playwright/test";

/**
 * What a saved file has for each modulator of the session, in the order of the file, as it is written: the text of each top-level `[[modulator]]` table (its keys, the place of the lane or group it hears, and its
 * `[[modulator.target]]` tables) up to the next table that is not one of its own targets. Every modulator is the session's own since step 94, and a session that is loaded has the modulators of its lanes and groups moved
 * there, so that the first is the tracer's LFO and a modulator that is made after it is the one after it.
 */
export function modulatorsIn(saved: string): string[] {
  return saved
    .split(/^\[\[modulator\]\]\n/m)
    .slice(1)
    .map((part) => part.split(/^\[(?!\[modulator\.target\]\])/m)[0] ?? "");
}

/** What a file has to say for `count` groups, empty, which are added to the end of the tracer's file. */
export const withGroups = (count: number): string => "\n[[group]]\n".repeat(count);

export interface OwnTang {
  /** Where the program serves the page. */
  url: string;
  /** The session file it saves to. */
  file: string;
  /** The directory of its library of modules (`TANG_MODULES_DIR`): its own, so that no test reads or writes the one of the person who runs them. */
  library: string;
}

/** `test` for a program that is started on the tracer's file with `added` at the end of it. */
function programOn(added: string) {
  return base.extend<{ own: OwnTang }>({
    // (Playwright's fixture function is given `use`, which is not named so here: it is not a React hook.)
    // eslint-disable-next-line no-empty-pattern -- a fixture that needs no other fixture still has to take the (empty) first argument
    own: async ({}, provide, testInfo) => {
      const dir = mkdtempSync(join(tmpdir(), "tang-e2e-"));
      const file = join(dir, "tracer.toml");
      writeFileSync(file, readFileSync(join("src", "client", "fixtures", "tracer.toml"), "utf8") + added);
      // (What it logs is kept with a test that fails: the program is gone by the time anyone looks.)
      const log = join(dir, "tang.log");
      const logFile = openSync(log, "w");
      const library = join(dir, "modules");
      // (`-v info`: a server tells of warnings and errors and no more unless it is told to, and what it did is what a test that failed is read by.)
      const child = spawn(join("..", "target", "debug", "tang"), ["--audio-device", "none", "--midi-device", "no such device", "-v", "info", "serve", file, "--web-bind", "127.0.0.1:0"], {
        stdio: ["ignore", "pipe", logFile],
        env: { ...process.env, TANG_MODULES_DIR: library },
      });
      try {
        const output = child.stdout;
        if (!output) throw new Error("the program has no output to read where it serves from");
        // Where it serves is the first line it writes.
        const url = await new Promise<string>((resolve, reject) => {
          const timer = setTimeout(() => {
            reject(new Error("the program did not say where it serves within a minute"));
          }, 60_000);
          createInterface({ input: output }).on("line", (line) => {
            const found = /^Web interface: (http:\/\/\S+)/.exec(line);
            if (found?.[1]) {
              clearTimeout(timer);
              resolve(found[1]);
            }
          });
          child.on("exit", (code) => {
            clearTimeout(timer);
            reject(new Error(`the program ended (${String(code)}) before it said where it serves`));
          });
        });
        await provide({ url, file, library });
      } finally {
        child.kill();
        closeSync(logFile);
        if (testInfo.status !== testInfo.expectedStatus) await testInfo.attach("tang.log", { path: log });
        rmSync(dir, { recursive: true, force: true });
      }
    },
  });
}

export const test = programOn("");

/** `test` for a program that is started on the tracer's file with `count` groups added to it (the page has no way to make one). */
export const testWithGroups = (count: number): typeof test => programOn(withGroups(count));

/**
 * What a file has to say for a pattern box called Bass, four beats long, with one note in it, and a cable of notes from the box to the tracer's lane: added to the end of the tracer's file (step 124; the file format is "Pattern boxes
 * (`[[pattern]]`)" in `docs/reference/session-file.md`). The keys are not a node of the file: a box that no cable of notes ends at records every input, the keys among them.
 */
export const withPatterns = (): string => `
[[pattern]]
name = "Bass"
length_beats = 4.0
notes = [
  { at = 0.0, len = 0.5, note = "C3", vel = 100 },
]

[[cable]]
from = "pattern[0]"
to = "instrument[0]"
`;

/** `test` for a program that is started on the tracer's file with a pattern box added to it (`withPatterns`). */
export const testWithPatterns = (): typeof test => programOn(withPatterns());

/**
 * What a file has to say for the keys, a note splitter called Bass line (it picks the lowest note), a second lane (an oscillator, next to the tracer's) and the cables of notes from the keys to the splitter and from its pick to the tracer's lane:
 * added to the end of the tracer's file (step 128; the file format is "Note splitters (`[[splitter]]`)" in `docs/reference/session-file.md`). Nothing is cabled from its second output: the second lane hears every source until a cable says so.
 */
export const withSplitters = (): string => `
[keys]

[[splitter]]
name = "Bass line"

[[instrument]]
plugin = "builtin:osc"

[[cable]]
from = "keys"
to = "splitter[0]"

[[cable]]
from = "splitter[0]"
to = "instrument[0]"
`;

/** `test` for a program that is started on the tracer's file with a note splitter, the keys and a second lane added to it (`withSplitters`). */
export const testWithSplitters = (): typeof test => programOn(withSplitters());
