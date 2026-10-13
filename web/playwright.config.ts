import { defineConfig, devices } from "@playwright/test";

// Not the usual ports (8765 for `tang serve`, 5173 for Vite), so that a run of the tests
// does not meet a server somebody has running, or leave one behind.
export const TANG = 18765;
/** The session file the real program serves in the tests: a copy of the tracer, which a test may save. */
export const SESSION_FILE = ".e2e-session/tracer.toml";
export const VITE = 15173;

export default defineConfig({
  testDir: "e2e",
  // One at a time: the tests of the real program share its one session, which a test that turns a
  // knob changes for every page that is open, so they must not overlap (and each puts it back).
  fullyParallel: false,
  workers: 1,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: { baseURL: `http://127.0.0.1:${VITE}`, trace: "retain-on-failure" },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: [
    {
      // The real program (`cargo build --features web`), on the null audio backend: no sound
      // card and no MIDI port is opened, so nothing is heard. It serves a copy of the tracer scene
      // (the tests' own, not `examples/tracer.toml`, which a person can save over; a test that
      // saves the session writes the copy).
      command: `mkdir -p .e2e-session && cp src/client/fixtures/tracer.toml .e2e-session/tracer.toml && ../target/debug/tang --audio-device none --midi-device "no such device" serve .e2e-session/tracer.toml --web-bind 127.0.0.1:${TANG} --web-origin http://127.0.0.1:${VITE}`,
      url: `http://127.0.0.1:${TANG}/`,
      reuseExistingServer: !process.env.CI,
      timeout: 120_000,
    },
    {
      command: `npm run dev -- --host 127.0.0.1 --port ${VITE} --strictPort`,
      url: `http://127.0.0.1:${VITE}/`,
      env: { TANG_SERVER: `ws://127.0.0.1:${TANG}` },
      reuseExistingServer: !process.env.CI,
    },
  ],
});
