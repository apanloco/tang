import { createContext, useContext, useEffect, useMemo } from "react";
import type { ReactNode } from "react";
import { useStore } from "zustand";
import { useStoreWithEqualityFn } from "zustand/traditional";

import type { Client, ClientState } from "../client/connection";
import { Drive } from "../client/drive";
import { pageFrames } from "../client/frameloop";
import type { FrameLoop } from "../client/frameloop";
import type { Intent } from "../protocol";

/** What the views of the page have in common: the client, the frames they draw on, and the controls that are turned. */
interface Services {
  client: Client;
  frames: FrameLoop;
  drive: Drive;
}

const ServicesContext = createContext<Services | null>(null);

/** What is done about an intent that did not get through; there is nowhere on the page to say it yet. */
function reportRefused(error: unknown, intent: Intent): void {
  console.warn(`${intent.type} was not done:`, error instanceof Error ? error.message : error);
}

/**
 * Gives the views below it a client, the frame loop they draw their live values on (the
 * page's own unless a test brings one) and a `Drive` that sends their changes through the
 * client; and keeps the client connected for as long as it is on the page.
 */
export function ClientProvider({ client, frames = pageFrames, children }: { client: Client; frames?: FrameLoop; children: ReactNode }) {
  useEffect(() => {
    client.start();
    return () => {
      client.stop();
    };
  }, [client]);
  const services = useMemo<Services>(
    () => ({ client, frames, drive: new Drive({ frames, send: (intent) => client.request(intent), onError: reportRefused }) }),
    [client, frames],
  );
  return <ServicesContext.Provider value={services}>{children}</ServicesContext.Provider>;
}

function useServices(): Services {
  const services = useContext(ServicesContext);
  if (!services) throw new Error("useClient is used below a ClientProvider");
  return services;
}

export function useClient(): Client {
  return useServices().client;
}

/** The controls of the session: where a view that turns a parameter sets it. */
export function useDrive(): Drive {
  return useServices().drive;
}

/** The loop a view that draws a live value subscribes to. */
export function useFrames(): FrameLoop {
  return useServices().frames;
}

/** A part of the client's state; the view renders again when that part changes. */
export function useClientState<T>(select: (state: ClientState) => T): T {
  return useStore(useClient().store, select);
}

/**
 * A part of the client's state that is an object made anew each time (or one whose
 * identity changes with things the view does not show): the view renders again only when
 * `equal` says that what it shows is not the same any more.
 */
export function useClientSelection<T>(select: (state: ClientState) => T, equal: (a: T, b: T) => boolean): T {
  return useStoreWithEqualityFn(useClient().store, select, equal);
}
