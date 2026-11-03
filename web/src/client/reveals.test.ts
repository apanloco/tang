import { describe, expect, test } from "vitest";

import { Reveals } from "./reveals";

describe("what the page wants shown", () => {
  test("is told to whoever listens, with the node, as it is asked for", () => {
    const reveals = new Reveals();
    const heard: number[] = [];
    reveals.subscribe((node) => heard.push(node));
    reveals.show(3);
    reveals.show(7);
    expect(heard).toEqual([3, 7]);
  });

  test("is told to every listener, and not to one that has stopped listening", () => {
    const reveals = new Reveals();
    const first: number[] = [];
    const second: number[] = [];
    const stop = reveals.subscribe((node) => first.push(node));
    reveals.subscribe((node) => second.push(node));
    reveals.show(1);
    stop();
    reveals.show(2);
    expect(first).toEqual([1]);
    expect(second).toEqual([1, 2]);
  });

  test("is nothing to anyone when nobody listens (a page with no canvas has nothing to show it on)", () => {
    const reveals = new Reveals();
    expect(() => {
      reveals.show(1);
    }).not.toThrow();
  });

  test("is not told to a listener that starts while it is being told: the next one is", () => {
    const reveals = new Reveals();
    const late: number[] = [];
    let started = false;
    reveals.subscribe(() => {
      if (started) return;
      started = true;
      reveals.subscribe((node) => late.push(node));
    });
    reveals.show(1);
    expect(late).toEqual([]);
    reveals.show(2);
    expect(late).toEqual([2]);
  });

  test("keeps a listener that stops while it is being told from missing the others", () => {
    const reveals = new Reveals();
    const heard: string[] = [];
    const stop = reveals.subscribe(() => {
      heard.push("first");
      stop();
    });
    reveals.subscribe(() => heard.push("second"));
    reveals.show(1);
    reveals.show(2);
    expect(heard).toEqual(["first", "second", "second"]);
  });
});
