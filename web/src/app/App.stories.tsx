import type { Meta, StoryObj } from "@storybook/react-vite";
import { useMemo } from "react";

import { Client } from "../client/connection";
import { MockServer } from "../client/mock";
import type { MockOptions } from "../client/mock";
import { stressSnapshot } from "../client/stress";
import { App } from "./App";
import { ClientProvider } from "./ClientProvider";

/** The whole page over a mock engine with these options, which is what most of the interface will be built on. */
function OverMock({ server }: { server?: MockOptions }) {
  const client = useMemo(() => new Client({ transport: new MockServer(server).transport }), [server]);
  return (
    <ClientProvider client={client}>
      <App engine="mock" />
    </ClientProvider>
  );
}

const meta = {
  title: "App/Page",
  component: OverMock,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof OverMock>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The session of the mock, kept up to date. */
export const WithTheTracerSession: Story = {};

/** Eight lanes, 32 LFOs and 64 knobs that move: what the performance test runs on. */
export const TheStressScene: Story = { args: { server: { snapshot: stressSnapshot() } } };

/** A server that speaks another protocol, which the page refuses with a reason. */
export const ServerOfAnotherProtocol: Story = { args: { server: { protocol: 2 } } };

/** A server that wants a token the page was not given. */
export const ServerThatWantsAToken: Story = { args: { server: { token: "secret" } } };
