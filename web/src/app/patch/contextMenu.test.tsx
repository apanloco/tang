// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { ContextMenuHost, useContextMenu } from "./contextMenu";
import type { MenuSpec } from "./contextMenu";

afterEach(cleanup);

// The host of the menus of the canvas (step 107): what a right click on whatever is in it asks for is a menu at the pointer, one at a time, and nothing of the menu before is kept for the next.

/** A button that opens the menu that `spec` says at a right click, in a box that would hear the click if the button did not take it. */
function Opener({ spec, outer }: { spec: () => MenuSpec; outer: () => void }) {
  const open = useContextMenu();
  return (
    <div onContextMenu={outer}>
      <button
        type="button"
        onContextMenu={(event) => {
          open(event, spec());
        }}
      >
        press
      </button>
    </div>
  );
}

function host(spec: () => MenuSpec) {
  const outer = vi.fn<() => void>();
  render(
    <ContextMenuHost>
      <Opener spec={spec} outer={outer} />
    </ContextMenuHost>,
  );
  return { outer, press: (init: MouseEventInit = {}) => fireEvent.contextMenu(screen.getByRole("button", { name: "press" }), init) };
}

const plain = (label: string, items = ["One", "Two"]): MenuSpec => ({ label, items: items.map((name) => ({ id: name, label: name, run: () => undefined })) });

describe("the host of the menus", () => {
  test("opens a menu at the pointer with the items and the name that it is given, and keeps the browser's own menu away, and the click from every box that the button is in", () => {
    const { press, outer } = host(() => plain("Oscillator"));
    // (`fireEvent` says whether the event was left alone: it was taken.)
    expect(press({ clientX: 30, clientY: 220 })).toBe(false);
    const opened = screen.getByRole("menu", { name: "Oscillator" });
    // (The corner of the menu is the pointer: this window has no size to keep it in, and the sides are what the page puts it at: 30 across and 220 down, and not the other way round.)
    expect([opened.style.left, opened.style.top]).toEqual(["30px", "220px"]);
    expect(screen.getAllByRole("menuitem").map((entry) => entry.textContent)).toEqual(["One", "Two"]);
    expect(outer).not.toHaveBeenCalled();
  });

  test("opens no menu for a menu that has no item, takes away the one that was open, and keeps the browser's menu away all the same", () => {
    let spec = plain("Oscillator");
    const { press } = host(() => spec);
    press();
    expect(screen.getByRole("menu", { name: "Oscillator" })).toBeTruthy();
    spec = { label: "Nothing", items: [] };
    expect(press()).toBe(false);
    expect(screen.queryByRole("menu")).toBeNull();
    // Still none, and none is not a menu of no item.
    expect(press()).toBe(false);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("is one menu at a time, and a menu that is opened while another is open is a new one: nothing of the first, the menus that were open in it and the item that had the focus, is kept", () => {
    let spec: MenuSpec = {
      label: "First",
      items: [
        { id: "menu", label: "Choices", items: [{ id: "a", label: "A", run: () => undefined }] },
        { id: "b", label: "B", run: () => undefined },
      ],
    };
    const { press } = host(() => spec);
    press({ clientX: 10, clientY: 10 });
    fireEvent.click(screen.getByRole("menuitem", { name: "Choices" }));
    expect(screen.getAllByRole("menu")).toHaveLength(2);
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "A" }));
    spec = plain("Second", ["Menu", "Other"]);
    press({ clientX: 200, clientY: 40 });
    expect(screen.getAllByRole("menu").map((each) => each.getAttribute("aria-label"))).toEqual(["Second"]);
    const second = screen.getByRole("menu", { name: "Second" });
    expect([second.style.left, second.style.top]).toEqual(["200px", "40px"]);
    // The menu itself has the focus, with no item chosen, as a menu has when it opens.
    expect(document.activeElement).toBe(second);
  });

  test("closes the menu when an item is chosen, or the menu is left", () => {
    const { press } = host(() => plain("Oscillator"));
    press();
    fireEvent.click(screen.getByRole("menuitem", { name: "One" }));
    expect(screen.queryByRole("menu")).toBeNull();
    press();
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("has no menu to open for whatever is outside the host: the function that opens one does nothing", () => {
    function Outside() {
      const open = useContextMenu();
      return (
        <button
          type="button"
          onContextMenu={(event) => {
            open(event, plain("Nowhere"));
          }}
        >
          outside
        </button>
      );
    }
    render(<Outside />);
    fireEvent.contextMenu(screen.getByRole("button", { name: "outside" }));
    expect(screen.queryByRole("menu")).toBeNull();
  });
});
