import { createContext, useContext, useEffect } from "react";
import type { ReactNode } from "react";
import { useStore } from "zustand";

import type { Client, ClientState } from "../client/connection";

const ClientContext = createContext<Client | null>(null);

/** Gives the views below it a client, and keeps it connected for as long as it is on the page. */
export function ClientProvider({ client, children }: { client: Client; children: ReactNode }) {
  useEffect(() => {
    client.start();
    return () => {
      client.stop();
    };
  }, [client]);
  return <ClientContext.Provider value={client}>{children}</ClientContext.Provider>;
}

export function useClient(): Client {
  const client = useContext(ClientContext);
  if (!client) throw new Error("useClient is used below a ClientProvider");
  return client;
}

/** A part of the client's state; the view renders again when that part changes. */
export function useClientState<T>(select: (state: ClientState) => T): T {
  return useStore(useClient().store, select);
}
