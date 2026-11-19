// Whether the page shows how much of the audio thread's time the plugins and the whole block take (step 131): a choice of the person at this browser, kept in its storage, on until it is turned off.
// While it is off nothing asks the server for the loads.
import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";

const KEY = "tang.loadsShown";

function stored(): boolean {
  try {
    return globalThis.localStorage.getItem(KEY) !== "false";
  } catch {
    return true;
  }
}

export const loadsShown = createStore<{ shown: boolean }>(() => ({ shown: stored() }));

/** Show the loads or hide them, here and the next time the page is opened in this browser. */
export function showLoads(shown: boolean): void {
  loadsShown.setState({ shown });
  try {
    globalThis.localStorage.setItem(KEY, String(shown));
  } catch {
    // (A browser that keeps nothing still shows or hides them until the page is closed.)
  }
}

export function useLoadsShown(): boolean {
  return useStore(loadsShown, (state) => state.shown);
}
