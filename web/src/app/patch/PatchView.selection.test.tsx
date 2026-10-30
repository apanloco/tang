// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { nodeNamed, tracerSnapshot } from "../../client/fixtures";
import type { Intent } from "../../protocol";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { useNotices } from "../ClientProvider";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// Choosing nodes to make a module of (step 105): the button that turns choosing on, a click that chooses a node, what the bar says of what is chosen, the button that saves it and what it asks of the server.

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");

function Board() {
  return <NoticeBoard notices={useNotices()} />;
}

async function rigged() {
  return rig(
    <>
      <PatchView />
      <Board />
    </>,
    { snapshot: tracerSnapshot },
  );
}

const asked = (requests: () => { intent: unknown }[]): Intent[] => requests().map((request) => request.intent as Intent);
const noticeTexts = (): string[] => Array.from(document.querySelectorAll(".tang-notice__text")).map((text) => text.textContent);
const chooseButton = (): Promise<HTMLElement> => screen.findByRole("button", { name: "Choose nodes to save as a module" });

/** The form that an element of the field for a name is in (the field is drawn in the page, in a form of its own). */
function formOf(element: HTMLElement): HTMLFormElement {
  const form = element.closest("form");
  if (!form) throw new Error("the field is in no form");
  return form;
}

/** The node on the canvas that has this name in its header, as the element React Flow puts it in. */
async function nodeOnCanvas(name: string): Promise<HTMLElement> {
  const names = await screen.findAllByText(name, { selector: ".tang-node__name" });
  const found = names[0]?.closest<HTMLElement>(".react-flow__node");
  if (!found) throw new Error(`no node called ${name}`);
  return found;
}

describe("choosing nodes on the canvas", () => {
  test("is off until the button turns it on: a click on a node chooses nothing, and the bar has no field for what is chosen", async () => {
    await rigged();
    const button = await chooseButton();
    expect(button.getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(await nodeOnCanvas("Oscillator"));
    expect(document.querySelectorAll(".react-flow__node.selected")).toHaveLength(0);
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.queryByRole("button", { name: "Save the chosen nodes as a module" })).toBeNull();
  });

  test("is on while the button is pressed: the bar says what to do, and the button that saves cannot be pressed with nothing chosen", async () => {
    await rigged();
    const button = await chooseButton();
    fireEvent.click(button);
    expect(button.getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("status").textContent).toBe("Drag a box round the nodes to keep");
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Save the chosen nodes as a module" }).disabled).toBe(true);
    // (The button is its own off switch.)
    fireEvent.click(button);
    expect(button.getAttribute("aria-pressed")).toBe("false");
    expect(screen.queryByRole("status")).toBeNull();
  });

  test("chooses a node by a click, says how many are chosen, and saves them as a module under the name that was typed, asking for the plugins and for nothing else", async () => {
    const { requests } = await rigged();
    fireEvent.click(await chooseButton());
    fireEvent.click(await nodeOnCanvas("Oscillator"));
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).toBe("1 chosen");
    });
    expect(document.querySelectorAll(".react-flow__node.selected")).toHaveLength(1);
    const save = screen.getByRole<HTMLButtonElement>("button", { name: "Save the chosen nodes as a module" });
    expect(save.disabled).toBe(false);
    fireEvent.click(save);
    const dialog = await screen.findByRole("dialog", { name: "Save the chosen nodes as a module" });
    const field = within(dialog).getByRole("textbox", { name: "Module name" });
    fireEvent.change(field, { target: { value: " Voice " } });
    fireEvent.submit(formOf(field));
    await waitFor(() => {
      expect(noticeTexts()).toEqual(['Saved the module "Voice": + Effect adds it']);
    });
    expect(asked(requests)).toEqual([{ type: "saveSelectionAsModule", nodes: [oscillator.id], name: " Voice " }]);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    // (What is chosen stays so: another name is a click away.)
    expect(screen.getByRole("status").textContent).toBe("1 chosen");
  });

  /** The nodes called these, added one after the other to what is chosen (a second node is added to the ones that are chosen while the key for it is held: React Flow reads that from the keys, not from the click). */
  async function choose(...names: string[]): Promise<void> {
    fireEvent.click(await nodeOnCanvas(names[0] ?? ""));
    for (const key of ["Control", "Meta"]) fireEvent.keyDown(document.body, { key, code: `${key}Left` });
    for (const name of names.slice(1)) fireEvent.click(await nodeOnCanvas(name), { ctrlKey: true, metaKey: true });
    for (const key of ["Control", "Meta"]) fireEvent.keyUp(document.body, { key, code: `${key}Left` });
  }

  const saveAs = async (name: string): Promise<void> => {
    fireEvent.click(screen.getByRole("button", { name: "Save the chosen nodes as a module" }));
    const field = await screen.findByRole("textbox", { name: "Module name" });
    fireEvent.change(field, { target: { value: name } });
    fireEvent.submit(formOf(field));
  };

  test("asks for the modulators that are chosen with the plugins, in the order of the canvas's nodes (the server has the lanes of a module in the session's order whatever it is given), and counts them among what is chosen", async () => {
    const { requests } = await rigged();
    fireEvent.click(await chooseButton());
    const modulator = tracerSnapshot.graph.nodes.find((node) => node.kind === "modulator");
    if (!modulator) throw new Error("the tracer has a modulator");
    await choose(modulator.name, "Oscillator");
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).toBe("2 chosen");
    });
    await saveAs("Voice");
    await waitFor(() => {
      expect(asked(requests)).toEqual([{ type: "saveSelectionAsModule", nodes: [oscillator.id, modulator.id], name: "Voice" }]);
    });
  });

  test("leaves out of what it asks the nodes that a module cannot hold, and says how many: the master is none of them", async () => {
    const { requests } = await rigged();
    fireEvent.click(await chooseButton());
    await choose("Oscillator", "Master");
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).toBe("1 chosen, 1 that a module cannot hold left out");
    });
    await saveAs("Voice");
    await waitFor(() => {
      expect(asked(requests)).toEqual([{ type: "saveSelectionAsModule", nodes: [oscillator.id], name: "Voice" }]);
    });
  });

  test("has nothing to hold when the master alone is chosen, says that and no more of what a module needs, and cannot be saved", async () => {
    const { requests } = await rigged();
    fireEvent.click(await chooseButton());
    fireEvent.click(await nodeOnCanvas("Master"));
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).toBe("0 chosen, 1 that a module cannot hold left out");
    });
    const save = screen.getByRole("button", { name: "Save the chosen nodes as a module" });
    expect(save).toHaveProperty("disabled", true);
    fireEvent.click(save);
    expect(screen.queryByRole("textbox", { name: "Module name" })).toBeNull();
    expect(asked(requests)).toEqual([]);
  });

  test("cannot be saved with a modulator and no plugin, and says what a module needs: it is made of effects and instruments, which a modulator goes with", async () => {
    const { requests } = await rigged();
    fireEvent.click(await chooseButton());
    const modulator = tracerSnapshot.graph.nodes.find((node) => node.kind === "modulator");
    if (!modulator) throw new Error("the tracer has a modulator");
    fireEvent.click(await nodeOnCanvas(modulator.name));
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).toBe("1 chosen, and a module needs an effect or an instrument");
    });
    const save = screen.getByRole("button", { name: "Save the chosen nodes as a module" });
    expect(save).toHaveProperty("disabled", true);
    fireEvent.click(save);
    expect(screen.queryByRole("textbox", { name: "Module name" })).toBeNull();
    expect(asked(requests)).toEqual([]);
    // With the filter chosen as well it can.
    await choose(modulator.name, "Filter");
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).toBe("2 chosen");
    });
    expect(screen.getByRole("button", { name: "Save the chosen nodes as a module" })).toHaveProperty("disabled", false);
  });

  test("forgets what is chosen when it is turned off, and Escape turns it off", async () => {
    await rigged();
    const button = await chooseButton();
    fireEvent.click(button);
    fireEvent.click(await nodeOnCanvas("Oscillator"));
    await waitFor(() => {
      expect(document.querySelectorAll(".react-flow__node.selected")).toHaveLength(1);
    });
    fireEvent.click(button);
    await waitFor(() => {
      expect(document.querySelectorAll(".react-flow__node.selected")).toHaveLength(0);
    });
    fireEvent.click(button);
    fireEvent.click(await nodeOnCanvas("Oscillator"));
    await waitFor(() => {
      expect(document.querySelectorAll(".react-flow__node.selected")).toHaveLength(1);
    });
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => {
      expect(button.getAttribute("aria-pressed")).toBe("false");
    });
    expect(document.querySelectorAll(".react-flow__node.selected")).toHaveLength(0);
  });

  test("is not left by an Escape that something else took: what is open closes with the key and what is chosen stays, and the key that nothing took leaves it", async () => {
    await rigged();
    const button = await chooseButton();
    fireEvent.click(button);
    fireEvent.click(await nodeOnCanvas("Oscillator"));
    await waitFor(() => {
      expect(document.querySelectorAll(".react-flow__node.selected")).toHaveLength(1);
    });
    // (A menu or a field that is open takes the key and says so; the canvas's own listener, on the window, comes after it.)
    const take = (event: KeyboardEvent): void => {
      if (event.key === "Escape") event.preventDefault();
    };
    window.addEventListener("keydown", take, true);
    fireEvent.keyDown(document.body, { key: "Escape" });
    window.removeEventListener("keydown", take, true);
    expect(button.getAttribute("aria-pressed")).toBe("true");
    expect(document.querySelectorAll(".react-flow__node.selected")).toHaveLength(1);
    fireEvent.keyDown(document.body, { key: "Escape" });
    await waitFor(() => {
      expect(button.getAttribute("aria-pressed")).toBe("false");
    });
    expect(document.querySelectorAll(".react-flow__node.selected")).toHaveLength(0);
  });

  test("chooses no cable: a click on one while choosing is on leaves what is chosen as it was, and a cable is none of what a module is made of", async () => {
    await rigged();
    fireEvent.click(await chooseButton());
    await nodeOnCanvas("Oscillator");
    const cables = Array.from(document.querySelectorAll<HTMLElement>(".react-flow__edge"));
    expect(cables.length).toBeGreaterThan(0);
    for (const cable of cables) fireEvent.click(cable);
    expect(document.querySelectorAll(".react-flow__edge.selected")).toHaveLength(0);
    expect(document.querySelectorAll(".react-flow__edge.selectable")).toHaveLength(0);
    expect(screen.getByRole("status").textContent).toBe("Drag a box round the nodes to keep");
  });
});
