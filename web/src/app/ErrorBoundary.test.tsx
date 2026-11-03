// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import { ErrorBoundary } from "./ErrorBoundary";

afterEach(cleanup);

/** React says so, loudly, on the console, for every error a boundary takes: that is not what is tested. */
beforeEach(() => {
  vi.spyOn(console, "error").mockImplementation(() => undefined);
});
afterEach(() => {
  vi.restoreAllMocks();
});

let broken: unknown = null;

function Part() {
  // eslint-disable-next-line @typescript-eslint/only-throw-error -- a part can throw whatever it likes
  if (broken !== null) throw broken;
  return <p>the part</p>;
}

function page() {
  return (
    <div>
      <p>around it</p>
      <ErrorBoundary what="the patch">
        <Part />
      </ErrorBoundary>
    </div>
  );
}

describe("the error boundary", () => {
  test("draws what is in it, and says nothing, while it can be drawn", () => {
    broken = null;
    render(page());
    expect(screen.getByText("the part")).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  test("says what could not be drawn and why, and leaves what is around it", () => {
    broken = new Error("Cannot read properties of null");
    render(page());
    expect(screen.getByRole("alert").textContent).toContain("The page could not draw the patch: Cannot read properties of null");
    expect(screen.queryByText("the part")).toBeNull();
    expect(screen.getByText("around it")).toBeTruthy();
  });

  test("tells what is thrown that is not an error as its text", () => {
    broken = "no good";
    render(page());
    expect(screen.getByRole("alert").textContent).toContain("could not draw the patch: no good");
  });

  test("draws it again when asked to try again, if what was wrong is gone, and says so again if it is not", () => {
    broken = new Error("first");
    render(page());
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(screen.getByRole("alert").textContent).toContain("first");
    broken = null;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(screen.getByText("the part")).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  test("tells the console too, with where it was", () => {
    broken = new Error("seen");
    render(page());
    expect(console.error).toHaveBeenCalledWith("The page could not draw the patch", expect.objectContaining({ message: "seen" }), expect.stringContaining("Part"));
  });
});
