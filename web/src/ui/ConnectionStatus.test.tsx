// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import type { Status } from "../client/connection";
import { ConnectionStatus } from "./ConnectionStatus";

afterEach(cleanup);

describe("the connection status", () => {
  const cases: [Status, string, string][] = [
    ["idle", "Not connected", "idle"],
    ["connecting", "Connecting…", "busy"],
    ["syncing", "Loading the session…", "busy"],
    ["open", "Connected", "ok"],
    ["reconnecting", "Connection lost, reconnecting…", "busy"],
    ["stopped", "Disconnected", "bad"],
  ];

  test.each(cases)("says %s in words, with a tone", (status, label, tone) => {
    render(<ConnectionStatus status={status} />);
    const shown = screen.getByRole("status");
    expect(shown.textContent).toBe(label);
    expect(shown.getAttribute("data-status")).toBe(status);
    expect(shown.getAttribute("data-tone")).toBe(tone);
  });

  test("adds why, when it is told", () => {
    render(<ConnectionStatus status="stopped" problem="The token is wrong." />);
    expect(screen.getByRole("status").textContent).toBe("Disconnected The token is wrong.");
  });
});
