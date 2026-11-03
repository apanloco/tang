import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// `npm run dev` serves the app on 5173 and forwards the WebSocket to a `tang serve`
// on its default address, or on the one in $TANG_SERVER (start it with `--web-origin
// http://localhost:5173`: the server only talks to pages it knows). `npm run build`
// writes `dist/`, which the Rust program serves (see `docs/web-ui.md` §9.2).
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/ws": { target: process.env.TANG_SERVER ?? "ws://127.0.0.1:8765", ws: true, changeOrigin: true },
    },
  },
  build: {
    outDir: "dist",
    // `dist/` keeps a `.gitkeep` (the Rust build wants the folder to exist), so it is
    // cleaned by `scripts/clean-dist.mjs` rather than emptied by Vite.
    emptyOutDir: false,
    sourcemap: true,
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: ["src/test-setup.ts"],
    // A runner of CI has a few slow cores for 135 files at a time: arranging the sessions of the fixtures takes up to 7 s there, where it takes a second or two on a laptop.
    testTimeout: process.env.CI ? 30_000 : 5_000,
  },
});
