// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import { Instrumented } from "./Instrumented";

beforeEach(() => {
  delete window.tangCommits;
});

afterEach(() => {
  cleanup();
  vi.unstubAllEnvs();
});

function Counter() {
  const [count, setCount] = useState(0);
  return (
    <>
      <button
        onClick={() => {
          setCount((value) => value + 1);
        }}
      >
        once
      </button>
      <button
        onClick={() => {
          setCount((value) => value + 1);
          setCount((value) => value + 1);
        }}
      >
        twice
      </button>
      <span>{count}</span>
    </>
  );
}

describe("counting the commits of the page", () => {
  test("counts the first and every later one, in a development build", () => {
    render(
      <Instrumented>
        <Counter />
      </Instrumented>,
    );
    expect(window.tangCommits).toBe(1);
    fireEvent.click(screen.getByText("once"));
    expect(window.tangCommits).toBe(2);
    fireEvent.click(screen.getByText("twice"));
    expect(window.tangCommits).toBe(3); // one commit for two updates
  });

  test("counts nothing while nothing renders again", () => {
    render(
      <Instrumented>
        <Counter />
      </Instrumented>,
    );
    const before = window.tangCommits;
    expect(before).toBe(1);
    expect(window.tangCommits).toBe(before);
  });

  test("shows what is in it, as it is", () => {
    const { container } = render(
      <Instrumented>
        <Counter />
      </Instrumented>,
    );
    expect(container.querySelector("span")?.textContent).toBe("0");
    fireEvent.click(screen.getByText("twice"));
    expect(container.querySelector("span")?.textContent).toBe("2");
  });

  test("does nothing in a production build, which React does not profile", () => {
    vi.stubEnv("DEV", false);
    const { container } = render(
      <Instrumented>
        <Counter />
      </Instrumented>,
    );
    fireEvent.click(screen.getByText("once"));
    expect(container.querySelector("span")?.textContent).toBe("1");
    expect(window.tangCommits).toBeUndefined();
  });
});
