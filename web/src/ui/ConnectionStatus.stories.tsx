import type { Meta, StoryObj } from "@storybook/react-vite";

import { ConnectionStatus } from "./ConnectionStatus";

const meta = {
  title: "Connection/ConnectionStatus",
  component: ConnectionStatus,
} satisfies Meta<typeof ConnectionStatus>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Idle: Story = { args: { status: "idle" } };
export const Connecting: Story = { args: { status: "connecting" } };
export const LoadingTheSession: Story = { args: { status: "syncing" } };
export const Connected: Story = { args: { status: "open" } };
export const Reconnecting: Story = { args: { status: "reconnecting" } };
export const Disconnected: Story = { args: { status: "stopped" } };
export const Refused: Story = {
  args: { status: "stopped", problem: "This server wants a token: open the address Tang printed when it started." },
};
