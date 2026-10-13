import { createContext, useContext, useEffect, useMemo } from "react";
import type { ReactNode } from "react";
import { useStore } from "zustand";
import { useStoreWithEqualityFn } from "zustand/traditional";

import type { Client, ClientState } from "../client/connection";
import { Drive } from "../client/drive";
import { Editor, refusal } from "../client/editor";
import { pageFrames } from "../client/frameloop";
import type { FrameLoop } from "../client/frameloop";
import { Notices } from "../client/notices";
import { Reveals } from "../client/reveals";

/** What the views of the page have in common: the client, the frames they draw on, the controls that are turned, and what is done to the structure. */
interface Services {
  client: Client;
  frames: FrameLoop;
  drive: Drive;
  /** What the page has to tell the person that nobody asked it for: what could not be done. */
  notices: Notices;
  editor: Editor;
  /** What the page has added and wants shown: the canvas moves to it. */
  reveals: Reveals;
}

const ServicesContext = createContext<Services | null>(null);

/**
 * Gives the views below it a client, the frame loop they draw their live values on (the
 * page's own unless a test brings one), a `Drive` that sends their changes through the
 * client and an `Editor` that adds and takes out modulators and cables (both say what
 * the server refused as a notice); and keeps the client connected for as long as it is on the page.
 */
export function ClientProvider({ client, frames = pageFrames, children }: { client: Client; frames?: FrameLoop; children: ReactNode }) {
  useEffect(() => {
    client.start();
    return () => {
      client.stop();
    };
  }, [client]);
  const services = useMemo<Services>(() => {
    const notices = new Notices();
    const reveals = new Reveals();
    const drive = new Drive({ frames, send: (intent) => client.request(intent), onError: (error, intent) => notices.say(refusal(intent, error)) });
    return { client, frames, drive, notices, reveals, editor: new Editor(client, notices, reveals) };
  }, [client, frames]);
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

/** What the page says of what could not be done. */
export function useNotices(): Notices {
  return useServices().notices;
}

/** Where a view that shows the session (the canvas) hears of a node the page has just added, to show it. */
export function useReveals(): Reveals {
  return useServices().reveals;
}

/** Where a view that adds or takes out a modulator or a cable asks for it. */
export function useEditor(): Editor {
  return useServices().editor;
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
