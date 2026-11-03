// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { Mock } from "vitest";

import { ContextMenu } from "./ContextMenu";
import type { ContextItem } from "./ContextMenu";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
  // (What a test that measured the window said it was.)
  Object.defineProperty(window, "innerWidth", { configurable: true, value: 1024 });
  Object.defineProperty(window, "innerHeight", { configurable: true, value: 768 });
});

interface Mounted {
  onClose: Mock<() => void>;
  ran: string[];
}

/** A menu with a few items of every kind: a plain one, a switch, a choice, one that takes something away, one that cannot be chosen, and two menus. */
function menu(ran: string[]): ContextItem[] {
  const run = (name: string) => () => {
    ran.push(name);
  };
  return [
    { id: "params", label: "Parameters", run: run("params") },
    { id: "mute", label: "Mute", checked: false, run: run("mute") },
    { id: "solo", label: "Solo", checked: true, run: run("solo") },
    { id: "preset", label: "Presets", items: [{ id: "a", label: "Small Room", run: run("small") }, { id: "b", label: "Big Hall", current: true, run: run("big") }] },
    { id: "send", label: "Send audio to", items: [{ id: "a", label: "Reverb", group: "Effects", run: run("reverb") }, { id: "master", label: "Master", group: "Mix", run: run("master") }] },
    { id: "locked", label: "Save as module…", disabled: true, title: "A module needs an effect or an instrument", run: run("save") },
    { id: "remove", label: "Remove", danger: true, rule: true, run: run("remove") },
  ];
}

function mount(items?: (ran: string[]) => ContextItem[], at = { x: 100, y: 120 }): Mounted {
  const ran: string[] = [];
  const onClose = vi.fn<() => void>();
  render(
    <div>
      <button type="button">before</button>
      <ContextMenu label="Oscillator" at={at} items={(items ?? menu)(ran)} onClose={onClose} />
      <button type="button">after</button>
    </div>,
  );
  return { onClose, ran };
}

const first = (): HTMLElement => screen.getByRole("menu", { name: "Oscillator" });
/** The items of one menu, in the order they are drawn in (a switch is an item of its own role). */
const itemsOf = (container: ParentNode): HTMLElement[] => Array.from(container.querySelectorAll<HTMLElement>('[role="menuitem"], [role="menuitemcheckbox"]'));
const labels = (container: HTMLElement): (string | null)[] => itemsOf(container).map((entry) => entry.textContent);
/** The item with this label, of whichever menu is open (the labels of the menus in a test are all different). */
function item(name: string): HTMLElement {
  const found = itemsOf(document.body).filter((entry) => entry.textContent === name);
  const [only] = found;
  if (found.length !== 1 || !only) throw new Error(`${String(found.length)} items are called ${name}`);
  return only;
}

/** What has the focus. */
function active(): Element {
  const found = document.activeElement;
  if (!found) throw new Error("nothing has the focus");
  return found;
}
const key = (target: Element, name: string, init: KeyboardEventInit = {}): boolean => fireEvent.keyDown(target, { key: name, ...init });

describe("a context menu", () => {
  test("is a menu named for what it is of, in the page and not where it was opened, with its items in order, and the focus on the menu itself with no item chosen", () => {
    mount();
    const found = first();
    expect(found.parentElement).toBe(document.body);
    expect(labels(found)).toEqual(["Parameters", "Mute", "Solo", "Presets", "Send audio to", "Save as module…", "Remove"]);
    expect(document.activeElement).toBe(found);
    expect(itemsOf(found).filter((entry) => entry.tabIndex === 0)).toHaveLength(0);
  });

  test("says what each item is: a switch and whether it is on, the choice that is the one now, a menu that opens, an item that cannot be chosen and why", () => {
    mount();
    expect(item("Parameters").getAttribute("role")).toBe("menuitem");
    expect(item("Mute").getAttribute("role")).toBe("menuitemcheckbox");
    expect(item("Mute").getAttribute("aria-checked")).toBe("false");
    expect(item("Solo").getAttribute("aria-checked")).toBe("true");
    expect(item("Presets").getAttribute("aria-haspopup")).toBe("menu");
    expect(item("Presets").getAttribute("aria-expanded")).toBe("false");
    expect(item("Parameters").hasAttribute("aria-haspopup")).toBe(false);
    expect(item("Save as module…").getAttribute("aria-disabled")).toBe("true");
    expect(item("Save as module…").getAttribute("title")).toBe("A module needs an effect or an instrument");
    expect(item("Parameters").hasAttribute("aria-disabled")).toBe(false);
    expect(item("Remove").className).toContain("tang-menu__item--danger");
    expect(item("Parameters").className).not.toContain("tang-menu__item--danger");
    // (A menu that opens, and a switch that is on, are drawn so; nothing else is a menu or is expanded or not.)
    expect(item("Presets").className).toContain("tang-context__item--submenu");
    expect(item("Parameters").className).not.toContain("tang-context__item--submenu");
    expect(item("Solo").className).toContain("tang-context__item--checked");
    expect(item("Mute").className).not.toContain("tang-context__item--checked");
    expect(item("Parameters").hasAttribute("aria-expanded")).toBe(false);
  });

  test("puts the items in sections: a heading where the group changes, and a rule where an item asks for one", () => {
    mount();
    // (The three plain items, the two menus and the one that cannot be chosen are one section, and `Remove`, which asked for a rule, is another.)
    const sections = within(first()).getAllByRole("group");
    expect(sections.map((section) => labels(section))).toEqual([
      ["Parameters", "Mute", "Solo", "Presets", "Send audio to", "Save as module…"],
      ["Remove"],
    ]);
    fireEvent.click(item("Send audio to"));
    const submenu = screen.getByRole("menu", { name: "Send audio to" });
    expect(within(submenu).getAllByRole("group").map((section) => [section.getAttribute("aria-label"), section.querySelector(".tang-menu__heading")?.textContent])).toEqual([
      ["Effects", "Effects"],
      ["Mix", "Mix"],
    ]);
  });

  test("runs the item that is clicked, and the menu is closed before it runs, so that what an item opens is not closed by it", () => {
    const order: string[] = [];
    render(
      <ContextMenu
        label="Oscillator"
        at={{ x: 0, y: 0 }}
        items={[{ id: "x", label: "X", run: () => order.push("ran") }]}
        onClose={() => order.push("closed")}
      />,
    );
    fireEvent.click(screen.getByRole("menuitem", { name: "X" }));
    expect(order).toEqual(["closed", "ran"]);
  });

  test("runs one item once and closes once", () => {
    const { onClose, ran } = mount();
    fireEvent.click(item("Parameters"));
    expect(ran).toEqual(["params"]);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test("does nothing for an item that cannot be chosen: it does not run, and the menu stays", () => {
    const { onClose, ran } = mount();
    fireEvent.click(item("Save as module…"));
    key(item("Save as module…"), "Enter");
    expect(ran).toEqual([]);
    expect(onClose).not.toHaveBeenCalled();
    expect(first()).toBeTruthy();
  });

  test("moves between the items with the arrow keys, wrapping at the ends, and to the first and the last with Home and End; the first arrow from the menu itself goes to the first item or the last", () => {
    mount();
    const menuElement = first();
    key(menuElement, "ArrowDown");
    expect(document.activeElement).toBe(item("Parameters"));
    key(active(), "ArrowDown");
    expect(document.activeElement).toBe(item("Mute"));
    key(active(), "End");
    expect(document.activeElement).toBe(item("Remove"));
    key(active(), "ArrowDown");
    expect(document.activeElement).toBe(item("Parameters"));
    key(active(), "ArrowUp");
    expect(document.activeElement).toBe(item("Remove"));
    key(active(), "Home");
    expect(document.activeElement).toBe(item("Parameters"));
    cleanup();
    mount();
    key(first(), "ArrowUp");
    expect(document.activeElement).toBe(item("Remove"));
  });

  test("chooses with Enter or Space, and the page's keys do not hear the keys of the menu, which are letters that play notes", () => {
    const heard: string[] = [];
    const listen = (event: KeyboardEvent): void => {
      heard.push(event.key);
    };
    window.addEventListener("keydown", listen);
    const { ran } = mount();
    key(first(), "ArrowDown");
    key(active(), "Enter");
    expect(ran).toEqual(["params"]);
    cleanup();
    const second = mount();
    key(first(), "ArrowDown");
    key(active(), "ArrowDown");
    key(active(), " ");
    expect(second.ran).toEqual(["mute"]);
    cleanup();
    mount();
    key(first(), "z");
    key(first(), "ArrowDown");
    key(first(), "Home");
    key(first(), "Escape");
    window.removeEventListener("keydown", listen);
    expect(heard).toEqual([]);
  });

  test("goes to the item that starts with the letters typed: one letter goes to the next that starts with it, more go to the first that starts with all of them", () => {
    vi.useFakeTimers();
    mount();
    key(first(), "s");
    expect(document.activeElement).toBe(item("Solo"));
    key(first(), "s");
    expect(document.activeElement).toBe(item("Send audio to"));
    key(first(), "s");
    expect(document.activeElement).toBe(item("Save as module…"));
    key(first(), "s");
    expect(document.activeElement).toBe(item("Solo"));
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    key(first(), "p");
    key(first(), "r");
    expect(document.activeElement).toBe(item("Presets"));
    // (A letter with Control or Command held is not a letter.)
    key(first(), "m", { ctrlKey: true });
    expect(document.activeElement).toBe(item("Presets"));
  });

  test("goes to the first item of the menu for a letter typed when no item has the focus, and more letters of the name stay on the item that has it while it starts with them", () => {
    vi.useFakeTimers();
    mount((ran) => [
      { id: "solo", label: "Solo", run: () => ran.push("solo") },
      { id: "mix", label: "Solo mix", run: () => ran.push("mix") },
      { id: "pan", label: "Pan", run: () => ran.push("pan") },
    ]);
    key(first(), "s");
    expect(document.activeElement).toBe(item("Solo"));
    key(first(), "o");
    expect(document.activeElement).toBe(item("Solo"));
    // (A letter, a moment later, goes to the next item that starts with it.)
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    key(first(), "s");
    expect(document.activeElement).toBe(item("Solo mix"));
    // A capital letter is the letter.
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    key(first(), "P", { shiftKey: true });
    expect(document.activeElement).toBe(item("Pan"));
  });

  test("ends the letters that were typed after a pause of 700 ms, and not before: a letter at 699 ms goes on with the name, one at 700 ms begins another", () => {
    vi.useFakeTimers();
    mount((ran) => [
      { id: "alpha", label: "Alpha", run: () => ran.push("alpha") },
      { id: "beta", label: "Beta", run: () => ran.push("beta") },
      { id: "lambda", label: "Lambda", run: () => ran.push("lambda") },
    ]);
    key(first(), "a");
    expect(document.activeElement).toBe(item("Alpha"));
    act(() => {
      vi.advanceTimersByTime(699);
    });
    // "al": Alpha is still the first item that starts with it.
    key(first(), "l");
    expect(document.activeElement).toBe(item("Alpha"));
    act(() => {
      vi.advanceTimersByTime(700);
    });
    // "l": the next item that starts with it.
    key(first(), "l");
    expect(document.activeElement).toBe(item("Lambda"));
  });

  test("leaves to the browser the keys that are not letters, and the letters that have Control, Command or Alt held, which are its shortcuts: they are not taken, and go to no item", () => {
    vi.useFakeTimers();
    mount();
    for (const held of [{ ctrlKey: true }, { metaKey: true }, { altKey: true }]) {
      expect(key(first(), "m", held)).toBe(true);
      expect(document.activeElement).toBe(first());
    }
    expect(key(first(), "F5")).toBe(true);
    expect(key(first(), "Shift")).toBe(true);
    expect(document.activeElement).toBe(first());
    // (A letter by itself is the menu's.)
    expect(key(first(), "m")).toBe(false);
    expect(document.activeElement).toBe(item("Mute"));
  });

  test("opens the menu of an item that has one when it is clicked, beside it, with the first item focused, and closes everything when one of its items is chosen", () => {
    const { onClose, ran } = mount();
    fireEvent.click(item("Presets"));
    const submenu = screen.getByRole("menu", { name: "Presets" });
    expect(labels(submenu)).toEqual(["Small Room", "Big Hall"]);
    expect(item("Presets").getAttribute("aria-expanded")).toBe("true");
    expect(document.activeElement).toBe(within(submenu).getByRole("menuitem", { name: "Small Room" }));
    // (The choice now is marked.)
    expect(within(submenu).getByRole("menuitem", { name: "Big Hall" }).getAttribute("aria-current")).toBe("true");
    fireEvent.click(within(submenu).getByRole("menuitem", { name: "Big Hall" }));
    expect(ran).toEqual(["big"]);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test("opens a menu with the right arrow and with Enter, and takes it away with the left arrow, which gives the focus back to the item that opened it; Escape takes away the menu that has the focus and leaves the first alone", () => {
    const { onClose } = mount();
    key(first(), "ArrowDown");
    key(active(), "End");
    key(active(), "ArrowUp");
    key(active(), "ArrowUp");
    expect(document.activeElement).toBe(item("Send audio to"));
    key(active(), "ArrowRight");
    expect(screen.getByRole("menu", { name: "Send audio to" })).toBeTruthy();
    expect(document.activeElement).toBe(item("Reverb"));
    key(active(), "ArrowLeft");
    expect(screen.queryByRole("menu", { name: "Send audio to" })).toBeNull();
    expect(document.activeElement).toBe(item("Send audio to"));
    key(active(), "Enter");
    expect(document.activeElement).toBe(item("Reverb"));
    key(active(), "Escape");
    expect(screen.queryByRole("menu", { name: "Send audio to" })).toBeNull();
    expect(document.activeElement).toBe(item("Send audio to"));
    expect(onClose).not.toHaveBeenCalled();
    // (The left arrow has nothing to close on the first menu, and the right arrow nothing to open on an item that has no menu.)
    key(active(), "ArrowLeft");
    expect(document.activeElement).toBe(item("Send audio to"));
    key(active(), "Home");
    key(active(), "ArrowRight");
    expect(screen.queryAllByRole("menu")).toHaveLength(1);
    expect(onClose).not.toHaveBeenCalled();
  });

  test("does not take away a menu that the pointer opened when the left arrow is pressed in the first menu: there is no menu to go back from", () => {
    vi.useFakeTimers();
    mount();
    fireEvent.pointerEnter(item("Presets"));
    act(() => {
      vi.advanceTimersByTime(200);
    });
    expect(screen.getByRole("menu", { name: "Presets" })).toBeTruthy();
    key(item("Presets"), "ArrowLeft");
    expect(screen.getByRole("menu", { name: "Presets" })).toBeTruthy();
    expect(document.activeElement).toBe(item("Presets"));
  });

  test("is left with Escape on the first menu, which gives the focus back to where it was", () => {
    const outside = document.createElement("button");
    document.body.append(outside);
    outside.focus();
    const { onClose } = mount();
    expect(document.activeElement).toBe(first());
    key(first(), "Escape");
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(document.activeElement).toBe(outside);
    outside.remove();
  });

  test("is closed by Escape wherever the focus is, and the key is taken: nothing else hears it, so that what is chosen on the canvas stays", () => {
    const heard: string[] = [];
    const listen = (event: KeyboardEvent): void => {
      heard.push(event.key);
    };
    window.addEventListener("keydown", listen);
    const { onClose } = mount();
    const outside = screen.getByRole("button", { name: "before" });
    outside.focus();
    // (`fireEvent` says whether the event was left alone: it was not.)
    expect(fireEvent.keyDown(outside, { key: "Escape" })).toBe(false);
    window.removeEventListener("keydown", listen);
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(heard).toEqual([]);
    // Another key outside the menu is none of its business.
    fireEvent.keyDown(outside, { key: "a" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test("is left with Tab, which gives the focus back, so that the Tab goes on from there, and is not taken", () => {
    const outside = document.createElement("button");
    document.body.append(outside);
    outside.focus();
    const { onClose } = mount();
    // (`fireEvent` says whether the event was left alone.)
    expect(key(first(), "Tab")).toBe(true);
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(document.activeElement).toBe(outside);
    outside.remove();
  });

  test("opens the menu of an item that the pointer rests on after a moment, and another item that it rests on takes it away, and a pointer that goes into the menu keeps it", () => {
    vi.useFakeTimers();
    mount();
    fireEvent.pointerEnter(item("Presets"));
    expect(screen.queryByRole("menu", { name: "Presets" })).toBeNull();
    // (It rests a moment: not at once, and not for long.)
    act(() => {
      vi.advanceTimersByTime(100);
    });
    expect(screen.queryByRole("menu", { name: "Presets" })).toBeNull();
    act(() => {
      vi.advanceTimersByTime(100);
    });
    const submenu = screen.getByRole("menu", { name: "Presets" });
    // (The pointer has the focus, as the keys do.)
    expect(document.activeElement).toBe(item("Presets"));
    // The pointer crosses an item on its way to the menu: nothing is closed when it gets there.
    fireEvent.pointerEnter(item("Send audio to"));
    fireEvent.pointerEnter(within(submenu).getByRole("menuitem", { name: "Small Room" }));
    act(() => {
      vi.advanceTimersByTime(500);
    });
    expect(screen.getByRole("menu", { name: "Presets" })).toBeTruthy();
    expect(screen.queryByRole("menu", { name: "Send audio to" })).toBeNull();
    // It rests on an item that has no menu: the one that was open goes.
    fireEvent.pointerEnter(item("Mute"));
    act(() => {
      vi.advanceTimersByTime(200);
    });
    expect(screen.queryByRole("menu", { name: "Presets" })).toBeNull();
    // It rests on one that has a menu while another is open: that one goes and this one opens.
    fireEvent.pointerEnter(item("Presets"));
    act(() => {
      vi.advanceTimersByTime(200);
    });
    fireEvent.pointerEnter(item("Send audio to"));
    act(() => {
      vi.advanceTimersByTime(200);
    });
    expect(screen.queryByRole("menu", { name: "Presets" })).toBeNull();
    expect(screen.getByRole("menu", { name: "Send audio to" })).toBeTruthy();
  });

  test("is closed by a press anywhere else, and by the wheel anywhere else, the page being sized and the window being left, and not by a press or a wheel in it: a long list is scrolled by the wheel", () => {
    const { onClose } = mount();
    fireEvent.pointerDown(first());
    fireEvent.wheel(first());
    fireEvent.pointerDown(item("Parameters"));
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(item("Presets"));
    fireEvent.pointerDown(screen.getByRole("menu", { name: "Presets" }));
    fireEvent.wheel(screen.getByRole("menu", { name: "Presets" }));
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.pointerDown(screen.getByRole("button", { name: "before" }));
    expect(onClose).toHaveBeenCalledTimes(1);
    fireEvent.wheel(screen.getByRole("button", { name: "after" }));
    expect(onClose).toHaveBeenCalledTimes(2);
    fireEvent(window, new Event("resize"));
    expect(onClose).toHaveBeenCalledTimes(3);
    fireEvent(window, new Event("blur"));
    expect(onClose).toHaveBeenCalledTimes(4);
  });

  test("lets go of what it listens to when it goes, and of a pause that was waiting", () => {
    vi.useFakeTimers();
    const set = vi.spyOn(window, "setTimeout");
    const cleared = vi.spyOn(window, "clearTimeout");
    const { onClose } = mount();
    fireEvent.pointerEnter(item("Presets"));
    // (The pause is the timer of 140 ms: the menu that the pointer has come to opens when it ends.)
    const resting = set.mock.calls.findIndex(([, delay]) => delay === 140);
    expect(resting).toBeGreaterThanOrEqual(0);
    const pause: unknown = set.mock.results[resting]?.value;
    cleanup();
    expect(cleared.mock.calls.map(([timer]) => timer as unknown)).toContain(pause);
    act(() => {
      vi.advanceTimersByTime(500);
    });
    fireEvent.pointerDown(document.body);
    fireEvent(window, new Event("resize"));
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("keeps the browser's own menu from opening over it", () => {
    mount();
    expect(fireEvent.contextMenu(first())).toBe(false);
    expect(fireEvent.contextMenu(item("Parameters"))).toBe(false);
  });
});

describe("where a context menu opens", () => {
  /** What every menu and every item measures in a window of 1000 by 800: a menu is 200 by 100, and an item 190 by 20, 5 across and 40 down in the menu it is in. */
  function measure(): void {
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1000 });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 800 });
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
      const rect = (left: number, top: number, width: number, height: number): DOMRect => ({
        x: left,
        y: top,
        left,
        top,
        width,
        height,
        right: left + width,
        bottom: top + height,
        toJSON: () => ({}),
      });
      if (this.getAttribute("role")?.startsWith("menuitem") === true) {
        const left = Number.parseFloat(this.closest<HTMLElement>('[role="menu"]')?.style.left ?? "0") + 5;
        const top = Number.parseFloat(this.closest<HTMLElement>('[role="menu"]')?.style.top ?? "0") + 40;
        return rect(left, top, 190, 20);
      }
      return rect(0, 0, 200, 100);
    });
  }

  const place = (element: HTMLElement): { left: number; top: number } => ({ left: Number.parseFloat(element.style.left), top: Number.parseFloat(element.style.top) });

  test("is at the pointer, and shown once it is there", () => {
    measure();
    mount(undefined, { x: 300, y: 250 });
    expect(place(first())).toEqual({ left: 300, top: 250 });
    expect(first().style.visibility).toBe("visible");
  });

  test("moves to stay in the window: left of the pointer at the right edge and above it at the bottom, and not past the top or the left edge", () => {
    measure();
    mount(undefined, { x: 950, y: 790 });
    expect(place(first())).toEqual({ left: 1000 - 200 - 4, top: 800 - 100 - 4 });
    cleanup();
    mount(undefined, { x: -30, y: -10 });
    expect(place(first())).toEqual({ left: 4, top: 4 });
  });

  test("is no taller than the window, and scrolls when it is longer", () => {
    measure();
    mount(undefined, { x: 10, y: 10 });
    expect(first().style.maxHeight).toBe("792px");
    expect(first().style.overflowY).toBe("auto");
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 100 });
    cleanup();
    mount(undefined, { x: 10, y: 10 });
    expect(first().style.maxHeight).toBe("120px");
  });

  test("puts the menu of an item at its left from the first place that has no room for it at its right: the room is what is left of the window, less the edge, and it is enough when it is as wide as the menu", () => {
    measure();
    // (The item is 195 across the first menu, whose place is the pointer's: its right edge is at x + 195, and the room is 1000 - 4 - x - 195.)
    mount(undefined, { x: 601, y: 100 });
    fireEvent.click(item("Presets"));
    // 200 is room enough for a menu that is 200 wide: it opens at the item's right, and the window holds it.
    expect(place(screen.getByRole("menu", { name: "Presets" })).left).toBe(1000 - 200 - 4);
    cleanup();
    mount(undefined, { x: 602, y: 100 });
    fireEvent.click(item("Presets"));
    expect(place(screen.getByRole("menu", { name: "Presets" })).left).toBe(602 + 5 - 200 - 2);
    cleanup();
    mount(undefined, { x: 603, y: 100 });
    fireEvent.click(item("Presets"));
    expect(place(screen.getByRole("menu", { name: "Presets" })).left).toBe(603 + 5 - 200 - 2);
  });

  test("puts the menu of an item at its right, level with the item, and at its left when there is no room at the right", () => {
    measure();
    mount(undefined, { x: 100, y: 100 });
    fireEvent.click(item("Presets"));
    const open = place(screen.getByRole("menu", { name: "Presets" }));
    // The item is at 105 to 295 across and 140 down: the menu opens at 297, a little above the item.
    expect(open).toEqual({ left: 297, top: 136 });
    cleanup();
    mount(undefined, { x: 760, y: 100 });
    fireEvent.click(item("Presets"));
    // The first menu fits at 760 (it is 200 wide, the window 1000), so the item is at 765 to 955, and there is no room for 200 more at its right: the menu opens at its left.
    const flipped = place(screen.getByRole("menu", { name: "Presets" }));
    expect(flipped.left).toBe(765 - 200 - 2);
  });
});

describe("menus in menus", () => {
  const deep = (ran: string[]): ContextItem[] => [
    {
      id: "add",
      label: "Add",
      items: [
        { id: "more", label: "More", items: [{ id: "deep", label: "Deep", run: () => ran.push("deep") }, { id: "deeper", label: "Deeper", run: () => ran.push("deeper") }] },
        { id: "other", label: "Other", run: () => ran.push("other") },
      ],
    },
    { id: "zed", label: "Zed", run: () => ran.push("zed") },
  ];
  const open = (): (string | null)[] => screen.getAllByRole("menu").map((each) => each.getAttribute("aria-label"));

  test("open one inside the other to any depth, each named for the item that opened it, and the left arrow takes away the deepest and gives the focus back to the item that opened it", () => {
    const { onClose, ran } = mount(deep);
    key(first(), "ArrowDown");
    key(active(), "ArrowRight");
    key(active(), "ArrowRight");
    expect(open()).toEqual(["Oscillator", "Add", "More"]);
    expect(document.activeElement).toBe(item("Deep"));
    expect(item("Add").getAttribute("aria-expanded")).toBe("true");
    expect(item("More").getAttribute("aria-expanded")).toBe("true");
    key(active(), "ArrowLeft");
    expect(open()).toEqual(["Oscillator", "Add"]);
    expect(document.activeElement).toBe(item("More"));
    key(active(), "ArrowRight");
    key(active(), "Enter");
    expect(ran).toEqual(["deep"]);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test("a pointer that rests on the item of a menu that is open, with another open inside it, takes the inner one away, and leaves the one that it opened", () => {
    vi.useFakeTimers();
    mount(deep);
    key(first(), "ArrowDown");
    key(active(), "ArrowRight");
    key(active(), "ArrowRight");
    expect(open()).toEqual(["Oscillator", "Add", "More"]);
    fireEvent.pointerEnter(item("Add"));
    act(() => {
      vi.advanceTimersByTime(200);
    });
    expect(open()).toEqual(["Oscillator", "Add"]);
    // On the item whose menu is the last that is open, nothing changes.
    fireEvent.pointerEnter(item("More"));
    act(() => {
      vi.advanceTimersByTime(200);
    });
    expect(open()).toEqual(["Oscillator", "Add", "More"]);
    fireEvent.pointerEnter(item("Other"));
    act(() => {
      vi.advanceTimersByTime(200);
    });
    expect(open()).toEqual(["Oscillator", "Add"]);
  });
});

describe("a context menu that is given other items", () => {
  test("shows the items it has now, and keeps the menu that is open while it does", () => {
    const ran: string[] = [];
    const onClose = vi.fn<() => void>();
    const view = render(<ContextMenu label="Oscillator" at={{ x: 0, y: 0 }} items={menu(ran)} onClose={onClose} />);
    fireEvent.click(item("Presets"));
    view.rerender(<ContextMenu label="Oscillator" at={{ x: 0, y: 0 }} items={[...menu(ran).slice(0, 4), { id: "late", label: "Late", run: () => undefined }]} onClose={onClose} />);
    expect(screen.getByRole("menuitem", { name: "Late" })).toBeTruthy();
    expect(screen.getByRole("menu", { name: "Presets" })).toBeTruthy();
    // An item whose menu is gone from the list takes its menu with it.
    view.rerender(<ContextMenu label="Oscillator" at={{ x: 0, y: 0 }} items={[{ id: "only", label: "Only", run: () => undefined }]} onClose={onClose} />);
    expect(screen.queryByRole("menu", { name: "Presets" })).toBeNull();
    expect(labels(first())).toEqual(["Only"]);
  });
});
