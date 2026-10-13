// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
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

  test("is left by Tab, which goes on from the button: the menu is not where the button is, so the focus is put back first", () => {
    for (const shiftKey of [false, true]) {
      const { trigger } = mount();
      fireEvent.click(trigger);
      fireEvent.keyDown(item(0), { key: "Tab", shiftKey });
      expect(screen.queryByRole("menu")).toBeNull();
      // (The browser's own Tab, which comes after this, goes to the next or the previous control from where the focus is.)
      expect(document.activeElement).toBe(trigger);
      cleanup();
    }
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

/** A menu of a plugin's parameters, as the list of a cable has them: by the plugin they are of. */
const PARTS = [
  { id: "1:1", label: "detune", group: "Oscillator" },
  { id: "1:2", label: "volume", group: "Oscillator" },
  { id: "2:0", label: "cutoff", group: "Filter" },
  { id: "2:1", label: "resonance", group: "Filter" },
];

function mountParts(parts: readonly { id: string; label: string; group?: string }[] = PARTS, onChoose = vi.fn()) {
  const element = (list: typeof parts) => (
    <MenuButton label="Add a cable" id="drives" items={list} onChoose={onChoose}>
      Add a cable…
    </MenuButton>
  );
  const view = render(element(parts));
  const again = (list: typeof parts): void => {
    view.rerender(element(list));
  };
  return { view, onChoose, again, trigger: screen.getByRole("button", { name: "Add a cable" }) };
}

/** What the focused item says. */
const focusedLabel = (): string | null | undefined => document.activeElement?.textContent;

describe("a menu with groups", () => {
  test("draws the items that have a group under a heading: a run of one group is a group of the menu, named for it", () => {
    const { trigger } = mountParts();
    fireEvent.click(trigger);
    const groups = screen.getAllByRole("group");
    expect(groups.map((group) => group.getAttribute("aria-label"))).toEqual(["Oscillator", "Filter"]);
    expect(groups.map((group) => within(group).getAllByRole("menuitem").map((entry) => entry.textContent))).toEqual([
      ["detune", "volume"],
      ["cutoff", "resonance"],
    ]);
    // (The heading is for the eye, and the group says its name to a screen reader: the heading is not a thing to choose.)
    expect(groups.map((group) => group.firstElementChild?.textContent)).toEqual(["Oscillator", "Filter"]);
    expect(groups.map((group) => group.firstElementChild?.getAttribute("aria-hidden"))).toEqual(["true", "true"]);
    expect(screen.getAllByRole("menuitem")).toHaveLength(4);
  });

  test("goes through the items of all the groups as one list, and tells what was chosen by its id, not by its label", () => {
    const { trigger, onChoose } = mountParts();
    fireEvent.click(trigger);
    fireEvent.keyDown(item(0), { key: "ArrowDown" });
    fireEvent.keyDown(item(1), { key: "ArrowDown" });
    expect(focusedLabel()).toBe("cutoff");
    fireEvent.click(item(2));
    expect(onChoose).toHaveBeenCalledExactlyOnceWith("2:0");
  });

  test("makes a group of each run: one that comes up again after another is another group, and an item with none is loose", () => {
    const { trigger } = mountParts([
      { id: "a", label: "a", group: "One" },
      { id: "b", label: "b", group: "Two" },
      { id: "c", label: "c", group: "One" },
      { id: "d", label: "d" },
    ]);
    fireEvent.click(trigger);
    expect(screen.getAllByRole("group").map((group) => group.getAttribute("aria-label"))).toEqual(["One", "Two", "One"]);
    expect(screen.getAllByRole("menuitem").map((entry) => entry.closest('[role="group"]')?.getAttribute("aria-label") ?? null)).toEqual(["One", "Two", "One", null]);
  });

  test("has no groups in a menu whose items have none", () => {
    const { trigger } = mount();
    fireEvent.click(trigger);
    expect(screen.queryAllByRole("group")).toHaveLength(0);
  });

  test("is as the page gives it: the class of its button, which the page styles", () => {
    render(
      <MenuButton label="Add" className="wide" items={ITEMS} onChoose={vi.fn()}>
        +
      </MenuButton>,
    );
    const button = screen.getByRole("button", { name: "Add" });
    expect(button.classList.contains("wide")).toBe(true);
    expect(button.classList.contains("tang-menu-button")).toBe(true);
  });
});

describe("a long menu", () => {
  const MANY = Array.from({ length: 100 }, (_, index) => ({ id: `p${String(index)}`, label: `parameter ${String(index)}` }));

  test("has a height it does not go over, and scrolls", () => {
    const { trigger } = mountParts(MANY);
    fireEvent.click(trigger);
    const menu = screen.getByRole("menu");
    expect(menu.style.maxHeight).toBe("360px");
    expect(menu.style.overflowY).toBe("auto");
  });

  test("goes over the button when the most it grows to does not fit under it and fits above (not when all of its items would not)", () => {
    const { trigger } = mountParts(MANY);
    const bottom = window.innerHeight - 4;
    trigger.getBoundingClientRect = () => ({ left: 100, right: 130, top: bottom - 24, bottom, width: 30, height: 24, x: 100, y: bottom - 24, toJSON: () => ({}) });
    fireEvent.click(trigger);
    expect(screen.getByRole("menu").style.transform).toBe("translate(-100%, -100%)");
  });

  test("counts the headings in the height it needs: under the button where the items fit, over it where they and the headings do not", () => {
    const four = (grouped: boolean) => Array.from({ length: 4 }, (_, index) => ({ id: `p${String(index)}`, label: `parameter ${String(index)}`, ...(grouped ? { group: `plugin ${String(index)}` } : {}) }));
    const height = window.innerHeight;
    Object.defineProperty(window, "innerHeight", { value: 1000, configurable: true });
    try {
      // The button's foot is 150 px from the bottom of the window: four items need 128 px, and with a heading each, 224.
      for (const [grouped, transform] of [
        [false, "translateX(-100%)"],
        [true, "translate(-100%, -100%)"],
      ] as const) {
        const { trigger } = mountParts(four(grouped));
        trigger.getBoundingClientRect = () => ({ left: 100, right: 130, top: 826, bottom: 850, width: 30, height: 24, x: 100, y: 826, toJSON: () => ({}) });
        fireEvent.click(trigger);
        expect(screen.getByRole("menu").style.transform, grouped ? "with headings" : "without").toBe(transform);
        cleanup();
      }
    } finally {
      Object.defineProperty(window, "innerHeight", { value: height, configurable: true });
    }
  });
});

describe("finding an item by typing", () => {
  const PARAMS = ["Attack", "Decay", "Detune", "Drive", "Pan"].map((label) => ({ id: label.toLowerCase(), label }));

  afterEach(() => {
    vi.useRealTimers();
  });

  const type = (...keys: string[]): void => {
    for (const key of keys) fireEvent.keyDown(document.activeElement ?? document.body, { key });
  };

  test("goes to the next item that starts with the letter, whatever its case, and round the end", () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const { trigger } = mountParts(PARAMS);
    fireEvent.click(trigger);
    expect(focusedLabel()).toBe("Attack");
    type("d");
    expect(focusedLabel()).toBe("Decay");
    type("d"); // (the same letter again, at once: the next one, though the text typed so far is "dd")
    expect(focusedLabel()).toBe("Detune");
    type("D", "d");
    expect(focusedLabel()).toBe("Decay"); // Drive, then round to Decay
    vi.advanceTimersByTime(1000);
    type("P");
    expect(focusedLabel()).toBe("Pan");
    vi.advanceTimersByTime(1000);
    type("a"); // the next that starts with "a" is round the end
    expect(focusedLabel()).toBe("Attack");
  });

  test("goes to the first item that starts with the letters typed together, from the one it is on", () => {
    const { trigger } = mountParts(PARAMS);
    fireEvent.click(trigger);
    type("d", "e");
    expect(focusedLabel()).toBe("Decay");
    type("t");
    expect(focusedLabel()).toBe("Detune");
    type("x"); // nothing starts with "detx": the focus stays
    expect(focusedLabel()).toBe("Detune");
  });

  test("starts the text over after a pause", () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const { trigger } = mountParts(PARAMS);
    fireEvent.click(trigger);
    type("d", "e");
    expect(focusedLabel()).toBe("Decay");
    vi.advanceTimersByTime(1000);
    type("t"); // "t" and not "det": nothing starts with it
    expect(focusedLabel()).toBe("Decay");
    vi.advanceTimersByTime(1000);
    type("p");
    expect(focusedLabel()).toBe("Pan");
  });

  test("goes on within the pause: a letter after 600 ms is still part of the text", () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const { trigger } = mountParts(PARAMS);
    fireEvent.click(trigger);
    type("d");
    vi.advanceTimersByTime(600);
    type("r");
    expect(focusedLabel()).toBe("Drive");
  });

  test("leaves alone a key that is held with Control, Meta or Alt, a space (which chooses), and a key that has a name: they are the browser's", () => {
    const heard = vi.fn();
    document.addEventListener("keydown", heard);
    try {
      const { trigger } = mountParts(PARAMS);
      fireEvent.click(trigger);
      const keys = [
        { key: "d", ctrlKey: true },
        { key: "d", metaKey: true },
        { key: "d", altKey: true },
        { key: " " },
        { key: "Shift" },
        { key: "Dead" },
      ];
      for (const key of keys) {
        // (`fireEvent` says whether the browser may still do what the key does: not if it was prevented.)
        expect(fireEvent.keyDown(item(0), key), JSON.stringify(key)).toBe(true);
      }
      expect(heard).toHaveBeenCalledTimes(keys.length);
      expect(focusedLabel()).toBe("Attack");
    } finally {
      document.removeEventListener("keydown", heard);
    }
  });

  test("is not heard by the page, whose keys play notes: the letters are the menu's while it is open", () => {
    const heard = vi.fn();
    document.addEventListener("keydown", heard);
    try {
      const { trigger } = mountParts(PARAMS);
      fireEvent.click(trigger);
      for (const key of ["d", "z", "["]) {
        // (And what the key would do in the page, such as moving it, is not done: it was for the menu.)
        expect(fireEvent.keyDown(item(0), { key }), key).toBe(false);
      }
      expect(heard).not.toHaveBeenCalled();
    } finally {
      document.removeEventListener("keydown", heard);
    }
  });
});

describe("a menu whose items change while it is open", () => {
  test("closes when it has no items left", () => {
    const { trigger, again } = mountParts();
    fireEvent.click(trigger);
    expect(screen.queryByRole("menu")).not.toBeNull();
    again([]);
    expect(screen.queryByRole("menu")).toBeNull();
    // ...and does not come back by itself when there are items again.
    again(PARTS);
    expect(screen.queryByRole("menu")).toBeNull();
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
  });

  test("keeps the focus on the item that has it when the ones before it go", () => {
    const { trigger, again } = mountParts();
    fireEvent.click(trigger);
    fireEvent.keyDown(item(0), { key: "ArrowDown" });
    expect(focusedLabel()).toBe("volume");
    again(PARTS.slice(1));
    expect(focusedLabel()).toBe("volume");
    expect(items().map((entry) => entry.getAttribute("tabindex"))).toEqual(["0", "-1", "-1"]);
  });

  test("puts the focus on the first item when the one that had it goes", () => {
    const { trigger, again } = mountParts();
    fireEvent.click(trigger);
    fireEvent.keyDown(item(0), { key: "ArrowDown" });
    again(PARTS.filter((part) => part.label !== "volume"));
    expect(focusedLabel()).toBe("detune");
    // (Also when it was the first that went: the focus is not lost to the page.)
    cleanup();
    const second = mountParts();
    fireEvent.click(second.trigger);
    expect(focusedLabel()).toBe("detune");
    second.again(PARTS.slice(1));
    expect(focusedLabel()).toBe("volume");
  });
});
