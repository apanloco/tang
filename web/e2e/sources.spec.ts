import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Page } from "@playwright/test";

import { SOURCE_HEIGHT } from "../src/app/patch/toFlow";
import { drag, expectNodesApart, hearsLinks, node, notesCables, notesIn, notesOut, open, showAll } from "./canvas";
import { test } from "./own";

// Sources of notes and cables of notes from the page (docs/patch-graph.md §11.6, P4c): the keys and a device added from the bar, a cable of notes dragged from a source's output to a lane's input, the
// links by which a lane that has no cable hears every source, the menus that do the same from the keyboard, and what the program keeps of it (its session, the saved file, the history). The first tests are
// against the mock engine in the page (the canvas and its rules; it says that "Mock Keyboard MIDI In" and "Mock Pad Controller" are open), the last against the real program (which has no MIDI input open).

/** The keys and then the device "Mock Keyboard MIDI In" added from the bar, and the view moved to show them (a cable dragged at once would start where its port was a moment before). */
async function addSources(page: Page): Promise<void> {
  const add = page.getByRole("button", { name: "Add a source of notes" });
  await add.click();
  await page.getByRole("menuitem", { name: "Keys", exact: true }).click();
  await expect(node(page, "Keys")).toBeVisible();
  await add.click();
  await page.getByRole("menuitem", { name: "Mock Keyboard MIDI In", exact: true }).click();
  await expect(node(page, "Mock Keyboard MIDI In")).toBeVisible();
  await showAll(page);
}

mock("the keys and a device are added from the bar: each is a node before the lanes, a device says that its input is plugged in, and the lane hears each of them by a link", async ({ page }) => {
  await open(page);
  const add = page.getByRole("button", { name: "Add a source of notes" });
  await add.click();
  // The keys, and under a heading each MIDI input that is open.
  const menu = page.getByRole("menu", { name: "Add a source of notes" });
  await expect(menu.getByRole("menuitem")).toHaveText(["Keys", "Mock Keyboard MIDI In", "Mock Pad Controller"]);
  await expect(menu.getByRole("group")).toHaveText(["MIDI inputsMock Keyboard MIDI InMock Pad Controller"]);
  await page.getByRole("menuitem", { name: "Keys", exact: true }).click();
  const keys = node(page, "Keys");
  await expect(keys).toBeVisible();
  await expect(keys.getByText("Computer keys and the piano")).toBeVisible();
  await expect(hearsLinks(page)).toHaveCount(1);
  // (The menu is closed and the focus is back on the button, which does not offer the keys again.)
  await expect(add).toBeFocused();
  await add.click();
  await expect(menu.getByRole("menuitem")).toHaveText(["Mock Keyboard MIDI In", "Mock Pad Controller"]);
  await page.getByRole("menuitem", { name: "Mock Keyboard MIDI In", exact: true }).click();
  const device = node(page, "Mock Keyboard MIDI In");
  await expect(device).toBeVisible();
  await expect(device.getByText("Plugged in: Mock Keyboard MIDI In")).toBeVisible();
  await expect(hearsLinks(page)).toHaveCount(2);
  await expect(notesCables(page)).toHaveCount(0);
  // The links are in the colour of notes, faint and dotted (a link is not a cable that somebody drew).
  await expect(hearsLinks(page).first()).toHaveCSS("stroke", "rgb(156, 140, 255)");
  await expect(hearsLinks(page).first()).toHaveCSS("stroke-opacity", "0.4");
  await expect(hearsLinks(page).first()).toHaveCSS("stroke-dasharray", "2px, 6px");

  // The sources are before the lane, in a column of their own, and no node lies over another.
  await showAll(page);
  const lane = await node(page, "Oscillator").boundingBox();
  for (const source of [keys, device]) {
    const box = await source.boundingBox();
    expect((box?.x ?? Infinity) + (box?.width ?? 0)).toBeLessThan(lane?.x ?? -Infinity);
  }
  await expectNodesApart(page);
});

mock("a source's node is as tall as the layout assumes", async ({ page }) => {
  await open(page);
  await addSources(page);
  for (const name of ["Keys", "Mock Keyboard MIDI In"]) {
    const box = await node(page, name).boundingBox();
    // (The canvas is zoomed to fit the window, so the scale is how much of the layout's width the node is.)
    const scale = (box?.width ?? 304) / 304;
    expect((box?.height ?? 0) / scale, name).toBeCloseTo(SOURCE_HEIGHT, 0);
  }
});

mock("a cable of notes is dragged from a source's output to a lane's input: the input says so while it is dragged, the line is the colour of notes, and the lane then hears that source and no other", async ({ page }) => {
  await open(page);
  await addSources(page);
  const [device, lane] = [node(page, "Mock Keyboard MIDI In"), node(page, "Oscillator")];
  await expect(hearsLinks(page)).toHaveCount(2);
  await drag(page, notesOut(device), notesIn(lane), false);

  // While it is dragged: it is notes, the input of the lane that takes it says so, and the line is the colour of notes, plain.
  await expect(page.getByTestId("patch")).toHaveAttribute("data-connecting", "notes");
  await expect(page.locator(".tang-port--notes.tang-port--takes")).toHaveCount(1);
  await expect(notesIn(lane)).toHaveClass(/tang-port--takes/);
  await expect(page.locator(".react-flow__connection-path")).toHaveCSS("stroke", "rgb(156, 140, 255)");
  await expect(page.locator(".react-flow__connection-path")).toHaveCSS("stroke-dasharray", "none");

  // Let go on the lane's input: the cable is drawn, the links by which the lane heard every source are gone (it hears the device only), and nothing says that a cable is dragged any more.
  await page.mouse.up();
  await expect(notesCables(page)).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Remove cable Mock Keyboard MIDI In → Oscillator" })).toBeVisible();
  // (Drawn, the cable is in the colour of notes, plain.)
  await expect(notesCables(page).first()).toHaveCSS("stroke", "rgb(156, 140, 255)");
  await expect(notesCables(page).first()).toHaveCSS("stroke-dasharray", "none");
  await expect(hearsLinks(page)).toHaveCount(0);
  await expect(page.getByTestId("patch")).not.toHaveAttribute("data-connecting");
  await expect(page.locator(".tang-port--takes")).toHaveCount(0);
});

mock("a cable of notes that is let go where nothing takes it is not drawn: on the canvas, or at a lane that has a cable from that source already", async ({ page }) => {
  await open(page);
  await addSources(page);
  const [keys, lane] = [node(page, "Keys"), node(page, "Oscillator")];
  // On the canvas, away from every node.
  await drag(page, notesOut(keys), { x: 700, y: 60 });
  await expect(notesCables(page)).toHaveCount(0);
  await expect(page.getByTestId("patch")).not.toHaveAttribute("data-connecting");
  await expect(page.locator(".tang-notice")).toHaveCount(0);
  // The keys have a cable to the lane now: the lane's input does not take a second one, and nothing is drawn when one is let go on it.
  await drag(page, notesOut(keys), notesIn(lane));
  await expect(notesCables(page)).toHaveCount(1);
  await drag(page, notesOut(keys), notesIn(lane), false);
  await expect(page.getByTestId("patch")).toHaveAttribute("data-connecting", "notes");
  await expect(page.locator(".tang-port--notes.tang-port--takes")).toHaveCount(0);
  await page.mouse.up();
  await expect(notesCables(page)).toHaveCount(1);
  // (Nothing is said of it: the canvas did not let the cable go, which the controller would have refused and the page said.)
  await expect(page.locator(".tang-notice")).toHaveCount(0);
});

mock("a cable of notes can be drawn from the keyboard, from either end, and taken out, with the focus on the menu of the source, where the next one is drawn from", async ({ page }) => {
  await open(page, "lanes");
  await addSources(page);
  const send = page.getByRole("button", { name: "Send notes from Keys" });
  await send.focus();
  await page.keyboard.press("Enter");
  // The lanes that take notes from the keys, in the names the canvas gives them, the first with the focus.
  await expect(page.getByRole("menu", { name: "Send notes from Keys" }).getByRole("menuitem")).toHaveText(["Oscillator 1", "Oscillator 2"]);
  await expect(page.getByRole("menuitem", { name: "Oscillator 1" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(notesCables(page)).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Remove cable Keys → Oscillator 1" })).toBeVisible();
  // (The first lane hears the keys by the cable now, and the second hears both sources by a link each.)
  await expect(hearsLinks(page)).toHaveCount(2);
  await expect(send).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("menu", { name: "Send notes from Keys" }).getByRole("menuitem")).toHaveText(["Oscillator 2"]);
  await page.keyboard.press("Enter");
  await expect(notesCables(page)).toHaveCount(2);
  await expect(hearsLinks(page)).toHaveCount(0);
  // The keys have a cable to every lane, and the menu of the source is shut.
  await expect(send).toBeDisabled();

  // From the lane's end: the sources that have no cable to it (the device), by the button on the lane.
  const take = page.getByRole("button", { name: "Take notes into Oscillator 1" });
  await take.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("menu", { name: "Take notes into Oscillator 1" }).getByRole("menuitem")).toHaveText(["Mock Keyboard MIDI In"]);
  await page.keyboard.press("Enter");
  await expect(notesCables(page)).toHaveCount(3);
  await expect(page.getByRole("button", { name: "Remove cable Mock Keyboard MIDI In → Oscillator 1" })).toBeVisible();
  await expect(take).toBeDisabled();

  // Taken out by its button, the cable is gone, and the focus is on the menu of the source it started at, which has a lane to send to again.
  await page.getByRole("button", { name: "Remove cable Keys → Oscillator 1" }).click();
  await expect(notesCables(page)).toHaveCount(2);
  await expect(send).toBeEnabled();
  await expect(send).toBeFocused();
});

mock("the history has what is done to a source and a cable of notes, in the words of the controller's: added, drawn, taken out", async ({ page }) => {
  await open(page);
  const [undo, redo] = [page.getByRole("button", { name: /^Undo/ }), page.getByRole("button", { name: /^Redo/ })];
  await page.getByRole("button", { name: "Add a source of notes" }).click();
  await page.getByRole("menuitem", { name: "Keys", exact: true }).click();
  await expect(node(page, "Keys")).toBeVisible();
  await expect(undo).toHaveAccessibleName("Undo add Keys");
  await showAll(page);
  await drag(page, notesOut(node(page, "Keys")), notesIn(node(page, "Oscillator")));
  await expect(notesCables(page)).toHaveCount(1);
  await expect(undo).toHaveAccessibleName("Undo draw a cable of notes");
  await page.getByRole("button", { name: "Remove cable Keys → Oscillator" }).click();
  await expect(undo).toHaveAccessibleName("Undo remove a cable of notes");
  await expect(hearsLinks(page)).toHaveCount(1);
  await undo.click();
  await expect(notesCables(page)).toHaveCount(1);
  await undo.click();
  await expect(notesCables(page)).toHaveCount(0);
  await expect(hearsLinks(page)).toHaveCount(1);
  await undo.click();
  await expect(node(page, "Keys")).toHaveCount(0);
  await expect(redo).toHaveAccessibleName("Redo add Keys");
  await redo.click();
  await expect(node(page, "Keys")).toBeVisible();
  await redo.click();
  await expect(notesCables(page)).toHaveCount(1);
});

mock("a source is taken out by its button, with its cables, and the focus is on the button of the bar that adds one", async ({ page }) => {
  await open(page);
  await addSources(page);
  await drag(page, notesOut(node(page, "Mock Keyboard MIDI In")), notesIn(node(page, "Oscillator")));
  await expect(notesCables(page)).toHaveCount(1);
  await page.getByRole("button", { name: "Remove Mock Keyboard MIDI In" }).click();
  await expect(node(page, "Mock Keyboard MIDI In")).toHaveCount(0);
  await expect(notesCables(page)).toHaveCount(0);
  // (The lane hears the keys by a link again.)
  await expect(hearsLinks(page)).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Add a source of notes" })).toBeFocused();
});

test("the keys are a source of the session that the program keeps: the file has them and the cable of notes, a page that is opened again draws them, and undo puts them back", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  // The program has no MIDI input open (it is told so): the keys are all there is to add, and once they are there nothing is.
  const add = page.getByRole("button", { name: "Add a source of notes" });
  await add.click();
  await expect(page.getByRole("menu", { name: "Add a source of notes" }).getByRole("menuitem")).toHaveText(["Keys"]);
  await page.getByRole("menuitem", { name: "Keys", exact: true }).click();
  await expect(node(page, "Keys")).toBeVisible();
  await expect(add).toBeDisabled();
  await expect(hearsLinks(page)).toHaveCount(1);
  await showAll(page);
  await drag(page, notesOut(node(page, "Keys")), notesIn(node(page, "Oscillator")));
  await expect(notesCables(page)).toHaveCount(1);
  await expect(hearsLinks(page)).toHaveCount(0);

  // The program has it: a page that is opened again draws them, from the session that the program tells.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(node(page, "Keys")).toBeVisible();
  await expect(notesCables(page)).toHaveCount(1);

  // The file has it: the keys, and a cable from them to the lane's place.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved).toMatch(/^\[keys\]$/m);
  expect(saved).toMatch(/\[\[cable\]\]\nfrom = "keys"\nto = "instrument\[0\]"\n/);

  // Undo takes the cable out (the lane hears the keys by a link again) and then the keys; redo puts them back.
  const [undo, redo] = [page.getByRole("button", { name: /^Undo/ }), page.getByRole("button", { name: /^Redo/ })];
  await undo.click();
  await expect(notesCables(page)).toHaveCount(0);
  await expect(hearsLinks(page)).toHaveCount(1);
  await undo.click();
  await expect(node(page, "Keys")).toHaveCount(0);
  await redo.click();
  await expect(node(page, "Keys")).toBeVisible();
  await redo.click();
  await expect(notesCables(page)).toHaveCount(1);

  // A source that is taken out is gone from the file, with its cable.
  await page.getByRole("button", { name: "Remove Keys" }).click();
  await expect(node(page, "Keys")).toHaveCount(0);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const after = readFileSync(own.file, "utf8");
  expect(after).not.toContain("[keys]");
  expect(after).not.toContain("[[cable]]");
});
