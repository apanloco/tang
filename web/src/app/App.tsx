import { useMemo } from "react";

import { ConnectionStatus } from "../ui/ConnectionStatus";
import { useClientState } from "./ClientProvider";
import type { EngineKind } from "./engine";
import "./App.css";

/** The whole page, for now: the state of the connection and the session as the server sent it. */
export function App({ engine }: { engine: EngineKind }) {
  const status = useClientState((state) => state.status);
  const problem = useClientState((state) => state.problem);
  const snapshot = useClientState((state) => state.snapshot);
  const raw = useMemo(() => (snapshot ? JSON.stringify(snapshot, null, 2) : null), [snapshot]);

  return (
    <div className="tang-app">
      <header className="tang-app__bar">
        <h1 className="tang-app__title">Tang</h1>
        <ConnectionStatus status={status} />
        <span className="tang-app__engine">{engine === "mock" ? "mock engine" : "server"}</span>
      </header>
      <main className="tang-app__main">
        {problem ? (
          <p className="tang-app__problem" role="alert">
            {problem}
          </p>
        ) : null}
        {raw ? <pre className="tang-session">{raw}</pre> : null}
      </main>
    </div>
  );
}
