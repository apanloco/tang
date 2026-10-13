// A program of its own, for a test that changes what the shared one could not be put back from: a lane
// cannot be taken out from the page, so a test that adds one starts a program that is thrown away with
// it (on the null audio backend, so nothing is heard, and a copy of the tracer in a directory of its own,
// which the test may save). The program serves the page itself, as `embedded.spec.ts` has it.
import { spawn } from "node:child_process";
import { copyFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";

import { test as base } from "@playwright/test";

export interface OwnTang {
  /** Where the program serves the page. */
  url: string;
  /** The session file it saves to. */
  file: string;
}

export const test = base.extend<{ own: OwnTang }>({
  // (Playwright's fixture function is given `use`, which is not named so here: it is not a React hook.)
  // eslint-disable-next-line no-empty-pattern -- a fixture that needs no other fixture still has to take the (empty) first argument
  own: async ({}, provide) => {
    const dir = mkdtempSync(join(tmpdir(), "tang-e2e-"));
    const file = join(dir, "tracer.toml");
    copyFileSync(join("src", "client", "fixtures", "tracer.toml"), file);
    const child = spawn(join("..", "target", "debug", "tang"), ["--audio-device", "none", "--midi-device", "no such device", "serve", file, "--web-bind", "127.0.0.1:0"], {
      stdio: ["ignore", "pipe", "ignore"],
    });
    try {
      // Where it serves is the first line it writes.
      const url = await new Promise<string>((resolve, reject) => {
        const timer = setTimeout(() => {
          reject(new Error("the program did not say where it serves within a minute"));
        }, 60_000);
        createInterface({ input: child.stdout }).on("line", (line) => {
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
      await provide({ url, file });
    } finally {
      child.kill();
      rmSync(dir, { recursive: true, force: true });
    }
  },
});
