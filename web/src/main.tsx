import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./app/App";
import { ClientProvider } from "./app/ClientProvider";
import { createConnection } from "./app/engine";
import { Instrumented } from "./app/Instrumented";
import "./ui/global.css";
import "./ui/tokens.css";

const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root");
const { client, engine } = createConnection(window.location);
createRoot(root).render(
  <StrictMode>
    <Instrumented>
      <ClientProvider client={client}>
        <App engine={engine} />
      </ClientProvider>
    </Instrumented>
  </StrictMode>,
);
