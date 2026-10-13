// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { MenuButton } from "./MenuButton";

afterEach(cleanup);

const ITEMS = [
  { id: "lfo", label: "LFO" },
  { id: "envelope", label: "Envelope" },
  { id: "midi", label: "MIDI control" },
];

function mount(onChoose = vi.fn()) {
  const view = render(
    <div>
      <button type="button">before</button>
      <MenuButton label="Add a modulator to Oscillator" items={ITEMS} onChoose={onChoose}>
        +
      </MenuButton>
      <button type="button">after</button>
    </div>,
  );
  return { view, onChoose, trigger: screen.getByRole("button", { name: "Add a modulator to Oscillator" }) };
}

const items = (): HTMLElement[] => screen.queryAllByRole("menuitem");

/** The item at `index` of the menu that is open. */
function item(index: number): HTMLElement {
  const found = items()[index];
  if (!found) throw new Error(`the menu has no item ${String(index)}`);
  return found;
}

describe("a menu button", () => {
  test("is a button named for what it does, with a menu that is not there until it is opened", () => {
    const { trigger } = mount();
    expect(trigger.getAttribute("title")).toBe("Add a modulator to Oscillator");
    expect(trigger.getAttribute("aria-haspopup")).toBe("menu");
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(trigger.hasAttribute("aria-controls")).toBe(false); // (there is nothing for it to control)
    expect(trigger.textContent).toBe("+");
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("has the id it is given, which something that gives the focus back can find it by", () => {
    render(
      <MenuButton label="Add" id="the-button" items={ITEMS} onChoose={vi.fn()}>
        +
      </MenuButton>,
    );
    expect(document.getElementById("the-button")).toBe(screen.getByRole("button", { name: "Add" }));
  });

  test("opens a menu of the items when pressed, in the page and not in the node it is in, with the first item focused", () => {
    const { trigger } = mount();
    fireEvent.click(trigger);
    const menu = screen.getByRole("menu", { name: "Add a modulator to Oscillator" });
    expect(menu.parentElement).toBe(document.body);
    expect(items().map((item) => item.textContent)).toEqual(["LFO", "Envelope", "MIDI control"]);
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    expect(trigger.getAttribute("aria-controls")).toBe(menu.id);
    expect(document.activeElement).toBe(items()[0]);
  });

  test("is closed by pressing the button again", () => {
    const { trigger } = mount();
    fireEvent.click(trigger);
    fireEvent.click(trigger);
    expect(screen.queryByRole("menu")).toBeNull();
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
  });

  test("is opened from the keyboard by the down arrow, which focuses the first item, as the up arrow does the last", () => {
    const { trigger } = mount();
    trigger.focus();
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    expect(document.activeElement).toBe(items()[0]);
    cleanup();
    const second = mount();
    second.trigger.focus();
    fireEvent.keyDown(second.trigger, { key: "ArrowUp" });
    expect(document.activeElement).toBe(items()[2]);
  });

  test("moves between the items with the arrow keys, round the ends, and with Home and End", () => {
    const { trigger } = mount();
    fireEvent.click(trigger);
    const [first, second, third] = [item(0), item(1), item(2)];
    fireEvent.keyDown(first, { key: "ArrowDown" });
    expect(document.activeElement).toBe(second);
    fireEvent.keyDown(second, { key: "ArrowDown" });
    expect(document.activeElement).toBe(third);
    fireEvent.keyDown(third, { key: "ArrowDown" });
    expect(document.activeElement).toBe(first);
    fireEvent.keyDown(first, { key: "ArrowUp" });
    expect(document.activeElement).toBe(third);
    fireEvent.keyDown(third, { key: "Home" });
    expect(document.activeElement).toBe(first);
    fireEvent.keyDown(first, { key: "End" });
    expect(document.activeElement).toBe(third);
  });

  test("lets only the focused item be reached by Tab, as a menu does", () => {
    const { trigger } = mount();
    fireEvent.click(trigger);
    const [first, second, third] = [item(0), item(1), item(2)];
    expect([first, second, third].map((element) => element.getAttribute("tabindex"))).toEqual(["0", "-1", "-1"]);
    fireEvent.keyDown(first, { key: "ArrowDown" });
    expect([first, second, third].map((element) => element.getAttribute("tabindex"))).toEqual(["-1", "0", "-1"]);
  });

  test("tells what was chosen, closes, and gives the focus back to the button", () => {
    const onChoose = vi.fn();
    const { trigger } = mount(onChoose);
    fireEvent.click(trigger);
    fireEvent.click(item(1));
    expect(onChoose).toHaveBeenCalledExactlyOnceWith("envelope");
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  test("is left by Escape, with nothing chosen and the focus back on the button", () => {
    const onChoose = vi.fn();
    const { trigger } = mount(onChoose);
    fireEvent.click(trigger);
    fireEvent.keyDown(item(0), { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    expect(onChoose).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(trigger);
  });

  test("is left by Tab, which goes on to what comes next", () => {
    const { trigger } = mount();
    fireEvent.click(trigger);
    fireEvent.keyDown(item(0), { key: "Tab" });
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("is closed by a press anywhere else, and by the page being scrolled or sized, without choosing", () => {
    const onChoose = vi.fn();
    const { trigger } = mount(onChoose);
    for (const dismiss of [
      () => fireEvent.pointerDown(screen.getByRole("button", { name: "after" })),
      () => fireEvent.pointerDown(document.body),
      () => fireEvent.wheel(document.body),
      () => fireEvent(window, new Event("resize")),
    ]) {
      fireEvent.click(trigger);
      expect(screen.queryByRole("menu")).not.toBeNull();
      dismiss();
      expect(screen.queryByRole("menu")).toBeNull();
    }
    expect(onChoose).not.toHaveBeenCalled();
  });

  test("is not closed by a press on itself, or on its button (which closes it by its own click)", () => {
    const { trigger } = mount();
    fireEvent.click(trigger);
    fireEvent.pointerDown(screen.getByRole("menu"));
    expect(screen.queryByRole("menu")).not.toBeNull();
    fireEvent.pointerDown(trigger);
    expect(screen.queryByRole("menu")).not.toBeNull();
  });

  test("takes its menu away with it", () => {
    const { trigger, view } = mount();
    fireEvent.click(trigger);
    view.unmount();
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("with no items has no menu to open", () => {
    render(
      <MenuButton label="Nothing" items={[]} onChoose={vi.fn()}>
        +
      </MenuButton>,
    );
    const button = screen.getByRole("button", { name: "Nothing" });
    expect(button.hasAttribute("disabled")).toBe(true);
    act(() => {
      button.click();
    });
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("puts its menu over the button, and not under it, when there is no room below and there is above", () => {
    const { trigger } = mount();
    const bottom = window.innerHeight - 4;
    trigger.getBoundingClientRect = () => ({ left: 100, right: 130, top: bottom - 24, bottom, width: 30, height: 24, x: 100, y: bottom - 24, toJSON: () => ({}) });
    fireEvent.click(trigger);
    const menu = screen.getByRole("menu");
    expect(menu.style.top).toBe(`${String(bottom - 24 - 2)}px`);
    expect(menu.style.transform).toBe("translate(-100%, -100%)");
    // And under it, where there is room: at the top of the page, and in the middle of it, where there is room above as well.
    fireEvent.click(trigger);
    for (const top of [20, 300]) {
      trigger.getBoundingClientRect = () => ({ left: 100, right: 130, top, bottom: top + 24, width: 30, height: 24, x: 100, y: top, toJSON: () => ({}) });
      fireEvent.click(trigger);
      expect(screen.getByRole("menu").style.transform, `at ${String(top)}`).toBe("translateX(-100%)");
      fireEvent.click(trigger);
    }
  });

  test("stays under the button when there is no room above either (a window that is not tall enough)", () => {
    const { trigger } = mount();
    const height = window.innerHeight;
    Object.defineProperty(window, "innerHeight", { value: 100, configurable: true });
    try {
      trigger.getBoundingClientRect = () => ({ left: 100, right: 130, top: 20, bottom: 44, width: 30, height: 24, x: 100, y: 20, toJSON: () => ({}) });
      fireEvent.click(trigger);
      expect(screen.getByRole("menu").style.transform).toBe("translateX(-100%)");
    } finally {
      Object.defineProperty(window, "innerHeight", { value: height, configurable: true });
    }
  });

  test("stops listening to the page when it is closed", () => {
    const added = vi.spyOn(document, "addEventListener");
    const removed = vi.spyOn(document, "removeEventListener");
    const { trigger } = mount();
    const count = (spy: typeof added) => spy.mock.calls.filter(([type]) => type === "pointerdown").length;
    const before = [count(added), count(removed)];
    fireEvent.click(trigger);
    expect(count(added)).toBe((before[0] ?? 0) + 1);
    fireEvent.click(trigger);
    expect(count(removed)).toBe((before[1] ?? 0) + 1);
    added.mockRestore();
    removed.mockRestore();
  });

  test("puts its menu under the button, at the right edge of it", () => {
    const { trigger } = mount();
    trigger.getBoundingClientRect = () => ({ left: 100, right: 130, top: 20, bottom: 44, width: 30, height: 24, x: 100, y: 20, toJSON: () => ({}) });
    fireEvent.click(trigger);
    const menu = screen.getByRole("menu");
    expect(menu.style.top).toBe("46px");
    // (jsdom lays nothing out: the menu is as wide as nothing, so its left edge is the button's right edge.)
    expect(menu.style.left).toBe("130px");
    expect(menu.style.position).toBe("fixed");
  });
});
