import { useMemo, useState } from "react";

import { ConnectionStatus } from "../ui/ConnectionStatus";
import { NoticeBoard } from "../ui/NoticeBoard";
import { useClientState, useNotices } from "./ClientProvider";
import type { EngineKind } from "./engine";
import { ErrorBoundary } from "./ErrorBoundary";
import { PatchView } from "./patch/PatchView";
import "./App.css";

/** The session as JSON, for whoever wants to see what the server sent. Made only while it is shown: it is a lot to write on every change. */
function SessionJson() {
  const snapshot = useClientState((state) => state.snapshot);
  const raw = useMemo(() => (snapshot ? JSON.stringify(snapshot, null, 2) : null), [snapshot]);
  return raw ? <pre className="tang-session">{raw}</pre> : null;
}

function RawSession() {
  const [open, setOpen] = useState(false);
  return (
    <details
      className="tang-raw"
      open={open}
      onToggle={(event) => {
        setOpen(event.currentTarget.open);
      }}
    >
      <summary>Session as JSON</summary>
      {open ? <SessionJson /> : null}
    </details>
  );
}

/** The whole page: the state of the connection, the session as a patch, and the session as the server sent it. */
export function App({ engine }: { engine: EngineKind }) {
  const status = useClientState((state) => state.status);
  const problem = useClientState((state) => state.problem);
  const notices = useNotices();

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
        <NoticeBoard notices={notices} />
        <div className="tang-app__patch">
          <ErrorBoundary what="the patch">
            <PatchView />
          </ErrorBoundary>
        </div>
        <ErrorBoundary what="the session as JSON">
          <RawSession />
        </ErrorBoundary>
      </main>
    </div>
  );
}
