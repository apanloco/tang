import { Profiler } from "react";
import type { ReactNode } from "react";

declare global {
  interface Window {
    /** How many times React has committed an update to the page: read by the browser tests that check that telemetry does not make it. */
    tangCommits?: number;
  }
}

function countCommit(): void {
  window.tangCommits = (window.tangCommits ?? 0) + 1;
}

/**
 * Counts the commits React makes below it, in `window.tangCommits`, in a development build
 * (React does not profile a production build). The performance test of the browser holds
 * that the count does not move while telemetry streams (docs/web-ui.md §9.4).
 */
export function Instrumented({ children }: { children: ReactNode }) {
  if (!import.meta.env.DEV) return children;
  return (
    <Profiler id="page" onRender={countCommit}>
      {children}
    </Profiler>
  );
}
