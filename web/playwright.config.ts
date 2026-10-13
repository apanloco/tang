import { defineConfig, devices } from "@playwright/test";

// Not the usual ports (8765 for `tang serve`, 5173 for Vite), so that a run of the tests
// does not meet a server somebody has running, or leave one behind.
export const TANG = 18765;
export const VITE = 15173;

export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: { baseURL: `http://127.0.0.1:${VITE}`, trace: "retain-on-failure" },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: [
    {
      // The real program (`cargo build --features web`), on the null audio backend: no sound
      // card and no MIDI port is opened, so nothing is heard.
      command: `../target/debug/tang --audio-device none --midi-device "no such device" serve ../examples/tracer.toml --web-bind 127.0.0.1:${TANG} --web-origin http://127.0.0.1:${VITE}`,
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
