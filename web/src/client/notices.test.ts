import { describe, expect, test, vi } from "vitest";

import { MAX_NOTICES, Notices } from "./notices";

describe("the notices", () => {
  test("start empty, and keep what is said, in order, each with an id of its own", () => {
    const notices = new Notices();
    expect(notices.getItems()).toEqual([]);
    const a = notices.say("one");
    const b = notices.say("two");
    expect(a).not.toBe(b);
    expect(notices.getItems()).toEqual([
      { id: a, text: "one", count: 1 },
      { id: b, text: "two", count: 1 },
    ]);
  });

  test("count the same words said again and again, as one notice, while nothing else was said between", () => {
    const notices = new Notices();
    const first = notices.say("no");
    expect(notices.say("no")).toBe(first);
    expect(notices.say("no")).toBe(first);
    expect(notices.getItems()).toEqual([{ id: first, text: "no", count: 3 }]);
    notices.say("other");
    const again = notices.say("no");
    expect(again).not.toBe(first);
    expect(notices.getItems().map((n) => [n.text, n.count])).toEqual([["no", 3], ["other", 1], ["no", 1]]);
  });

  test("keep the last few and let the older ones go", () => {
    const notices = new Notices();
    for (let i = 1; i <= MAX_NOTICES + 2; i += 1) notices.say(`notice ${String(i)}`);
    expect(notices.getItems().map((n) => n.text)).toEqual(["notice 3", "notice 4", "notice 5"]);
  });

  test("can be dismissed one by one, and a notice that is gone is nothing to dismiss", () => {
    const notices = new Notices();
    const a = notices.say("a");
    const b = notices.say("b");
    notices.dismiss(a);
    expect(notices.getItems().map((n) => n.text)).toEqual(["b"]);
    const before = notices.getItems();
    notices.dismiss(a);
    notices.dismiss(999);
    expect(notices.getItems()).toBe(before);
    notices.dismiss(b);
    expect(notices.getItems()).toEqual([]);
  });

  test("tell the listeners when they change, and not otherwise, until they stop listening", () => {
    const notices = new Notices();
    const listener = vi.fn();
    const stop = notices.subscribe(listener);
    const before = notices.getItems();
    expect(notices.getItems()).toBe(before);
    expect(listener).not.toHaveBeenCalled();
    const a = notices.say("a");
    expect(listener).toHaveBeenCalledTimes(1);
    notices.say("a");
    expect(listener).toHaveBeenCalledTimes(2);
    notices.dismiss(999);
    expect(listener).toHaveBeenCalledTimes(2);
    notices.dismiss(a);
    expect(listener).toHaveBeenCalledTimes(3);
    stop();
    notices.say("b");
    expect(listener).toHaveBeenCalledTimes(3);
    expect(notices.getItems()).not.toBe(before);
  });
});
