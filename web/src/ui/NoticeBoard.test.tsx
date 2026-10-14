// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { Notices } from "../client/notices";
import { NoticeBoard } from "./NoticeBoard";

afterEach(cleanup);

describe("the notice board", () => {
  test("is a polite live region, and has nothing in it while there is nothing to say", () => {
    const { container } = render(<NoticeBoard notices={new Notices()} />);
    const board = container.querySelector(".tang-notices");
    expect(board?.getAttribute("aria-live")).toBe("polite");
    expect(board?.childElementCount).toBe(0);
    expect(screen.queryByRole("button")).toBeNull();
  });

  test("shows what is said as it is said, each with a button to dismiss it, which names it", () => {
    const notices = new Notices();
    render(<NoticeBoard notices={notices} />);
    act(() => {
      notices.say("Could not add an LFO to Filter: a modulator goes in a lane or in a group");
      notices.say("Could not remove Oscillator: a session keeps its last lane");
    });
    expect(screen.getAllByText(/^Could not/)).toHaveLength(2);
    const first = screen.getByRole("button", { name: "Dismiss: Could not add an LFO to Filter: a modulator goes in a lane or in a group" });
    expect(first.closest(".tang-notice")?.textContent).toContain("Could not add an LFO to Filter");
    expect(screen.getByRole("button", { name: /^Dismiss: Could not remove Oscillator/ })).toBeTruthy();
  });

  test("takes a notice away when its button is pressed, and only that one", () => {
    const notices = new Notices();
    render(<NoticeBoard notices={notices} />);
    act(() => {
      notices.say("one");
      notices.say("two");
    });
    fireEvent.click(screen.getByRole("button", { name: "Dismiss: one" }));
    expect(screen.queryByText("one")).toBeNull();
    expect(screen.getByText("two")).toBeTruthy();
    expect(notices.getItems().map((n) => n.text)).toEqual(["two"]);
  });

  test("says how many times the same thing was said, and not in the name of the button", () => {
    const notices = new Notices();
    render(<NoticeBoard notices={notices} />);
    act(() => {
      for (let i = 0; i < 3; i += 1) notices.say("no");
    });
    const notice = screen.getByText("no (3 times)").closest<HTMLElement>(".tang-notice");
    if (!notice) throw new Error("the notice is not in a notice");
    expect(within(notice).getByRole("button", { name: "Dismiss: no" })).toBeTruthy();
  });

  test("follows another set of notices when it is given one", () => {
    const a = new Notices();
    const b = new Notices();
    a.say("from a");
    b.say("from b");
    const { rerender } = render(<NoticeBoard notices={a} />);
    expect(screen.getByText("from a")).toBeTruthy();
    rerender(<NoticeBoard notices={b} />);
    expect(screen.queryByText("from a")).toBeNull();
    expect(screen.getByText("from b")).toBeTruthy();
  });
});
