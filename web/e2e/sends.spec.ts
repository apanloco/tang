import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Page } from "@playwright/test";

import { audioIn, audioOut, drag, node, open, sends } from "./canvas";
import { test } from "./own";

// Audio cables from the page (docs/patch-graph.md §11.6, P3d): a cable dragged from the output of audio of a lane, an effect or a group to an input of audio that takes it, its level on the
// cable, the menu that does the same from the keyboard, and what the program keeps of it (its session, the saved file, the history). The first tests are against the mock engine in the page
// (the canvas and its rules), the last against the real program.

const LEVEL = "Level of Oscillator → Master";

/** The slider of the level of the cable from the tracer's oscillator to the master. */
const level = (page: Page) => page.getByRole("slider", { name: LEVEL, exact: true });

mock("an audio cable is dragged from an output to an input that takes it: the inputs that take it say so while it is dragged, its line is the cable's colour, and it has its level when it is let go", async ({ page }) => {
  await open(page);
  const [oscillator, filter, master] = [node(page, "Oscillator"), node(page, "Filter"), node(page, "Master")];
  await drag(page, audioOut(oscillator), audioIn(master), false);

  // While it is dragged: it is audio, the input that takes it (the master's: the filter's has the link of the chain, which is a cable that is there already) says so, and the line is the colour of the cable, plain.
  await expect(page.getByTestId("patch")).toHaveAttribute("data-connecting", "audio");
  await expect(page.locator(".tang-port--audio.tang-port--takes")).toHaveCount(1);
  await expect(audioIn(filter)).not.toHaveClass(/tang-port--takes/);
  await expect(audioIn(master)).toHaveClass(/tang-port--takes/);
  await expect(page.locator(".react-flow__connection-path")).toHaveCSS("stroke", "rgb(242, 153, 74)");
  await expect(page.locator(".react-flow__connection-path")).toHaveCSS("stroke-dasharray", "none");

  // Let go on the master's input: the cable is drawn, at the level a new one has, and nothing says that a cable is dragged any more.
  await page.mouse.up();
  await expect(sends(page)).toHaveCount(1);
  await expect(level(page)).toHaveValue("1");
  await expect(page.getByTestId("patch")).not.toHaveAttribute("data-connecting");
  await expect(page.locator(".tang-port--takes")).toHaveCount(0);
  // (The link of the audio path from the filter to the master is where it was, beside the new cable.)
  await expect(page.locator(".tang-cable--audio")).toHaveCount(2);
});

mock("a cable of modulation is still magenta and dashed while it is dragged, and says which ports take it", async ({ page }) => {
  await open(page);
  const lfo = node(page, "LFO");
  await drag(page, lfo.locator('.react-flow__handle[data-handleid="modOut"]'), { x: 900, y: 120 }, false);
  await expect(page.getByTestId("patch")).toHaveAttribute("data-connecting", "modulation");
  await expect(page.locator(".react-flow__connection-path")).toHaveCSS("stroke", "rgb(208, 112, 208)");
  await expect(page.locator(".react-flow__connection-path")).toHaveCSS("stroke-dasharray", "7px, 5px");
  await expect(page.locator(".tang-port--audio.tang-port--takes")).toHaveCount(0);
  await page.mouse.up();
  await expect(sends(page)).toHaveCount(0);
});

mock("a cable that is let go where nothing takes it is not drawn: on the canvas, or at an input that does not take it", async ({ page }) => {
  await open(page);
  const [oscillator, filter] = [node(page, "Oscillator"), node(page, "Filter")];
  // On the canvas, away from every node.
  await drag(page, audioOut(oscillator), { x: 400, y: 200 });
  await expect(sends(page)).toHaveCount(0);
  await expect(page.getByTestId("patch")).not.toHaveAttribute("data-connecting");
  await expect(page.locator(".tang-notice")).toHaveCount(0);
  // From the filter's output: nothing takes it (the mix has the link of the chain, which is a cable that is there already, and the filter's own input is where the audio would go round), and let go on the
  // filter's own input nothing is drawn.
  await drag(page, audioOut(filter), audioIn(filter), false);
  await expect(page.locator(".tang-port--audio.tang-port--takes")).toHaveCount(0);
  await expect(audioIn(node(page, "Master"))).not.toHaveClass(/tang-port--takes/);
  await expect(audioIn(filter)).not.toHaveClass(/tang-port--takes/);
  await page.mouse.up();
  await expect(sends(page)).toHaveCount(0);
  // (Nothing is said of it: the canvas did not let the cable go, which the controller would have refused and the page said.)
  await expect(page.locator(".tang-notice")).toHaveCount(0);
});

mock("an audio cable can be drawn and its level turned from the keyboard, and taken out, with the focus on the button that sends from the node, where the next one is drawn from", async ({ page }) => {
  await open(page);
  // (The oscillator's filter is the link of its chain, which is a cable that is there already and is not offered: a reverb at the end of the chain is a place beside the master.)
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  await expect(node(page, "Reverb")).toBeVisible();
  const menu = page.getByRole("button", { name: "Send audio from Oscillator" });
  await menu.focus();
  await page.keyboard.press("Enter");
  // The places that take audio from the oscillator, under what each is, the first with the focus.
  await expect(page.getByRole("menu", { name: "Send audio from Oscillator" }).getByRole("group")).toHaveText(["EffectsReverb", "MixMaster"]);
  await expect(page.getByRole("menuitem", { name: "Reverb" })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(sends(page)).toHaveCount(1);
  await expect(level(page)).toHaveValue("1");
  await expect(menu).toBeFocused();
  // The menu no longer has the master, and has the reverb still.
  await page.keyboard.press("Enter");
  await expect(page.getByRole("menu", { name: "Send audio from Oscillator" }).getByRole("menuitem")).toHaveText(["Reverb"]);
  await page.keyboard.press("Escape");

  // The level, by the keys of a slider: a hundredth a press, and the end of its travel is twice.
  await level(page).focus();
  await page.keyboard.press("ArrowLeft");
  await expect(level(page)).toHaveValue("0.99");
  await page.keyboard.press("End");
  await expect(level(page)).toHaveValue("2");
  await expect(level(page)).toHaveAttribute("aria-valuetext", "200%");

  await page.getByRole("button", { name: "Remove cable Oscillator → Master" }).click();
  await expect(sends(page)).toHaveCount(0);
  await expect(menu).toBeFocused();
});

mock("the history has what is done to an audio cable, in the words of the controller's: drawn, its level, taken out", async ({ page }) => {
  await open(page);
  const [undo, redo] = [page.getByRole("button", { name: /^Undo/ }), page.getByRole("button", { name: /^Redo/ })];
  await drag(page, audioOut(node(page, "Oscillator")), audioIn(node(page, "Master")));
  await expect(undo).toHaveAccessibleName("Undo draw an audio cable");
  await level(page).fill("1.5");
  await expect(level(page)).toHaveValue("1.5");
  await expect(undo).toHaveAccessibleName("Undo set the level of an audio cable");
  await page.getByRole("button", { name: "Remove cable Oscillator → Master" }).click();
  await expect(undo).toHaveAccessibleName("Undo remove an audio cable");
  await undo.click();
  await expect(level(page)).toHaveValue("1.5");
  await undo.click();
  await expect(level(page)).toHaveValue("1");
  await undo.click();
  await expect(sends(page)).toHaveCount(0);
  await redo.click();
  await expect(level(page)).toHaveValue("1");
  await expect(redo).toHaveAccessibleName("Redo set the level of an audio cable");
});

test("an audio cable is in the session that the program keeps, in the saved file, and is put back by undo and drawn again by redo, at its level", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await drag(page, audioOut(node(page, "Oscillator")), audioIn(node(page, "Master")));
  await expect(sends(page)).toHaveCount(1);
  await level(page).fill("1.5");
  await expect(level(page)).toHaveValue("1.5");

  // The program has it: a page that is opened again draws it, from the session that the program tells, at its level.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(sends(page)).toHaveCount(1);
  await expect(level(page)).toHaveValue("1.5");

  // The file has it: from the lane's instrument to the mix, at the level it was turned to.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(readFileSync(own.file, "utf8")).toMatch(/\[\[cable\]\]\nfrom = "instrument\[0\]"\nto = "master"\ngain = 1\.5\n/);

  // Undo puts the level back, and then takes the cable out; redo draws it again, and then turns it up.
  const [undo, redo] = [page.getByRole("button", { name: /^Undo/ }), page.getByRole("button", { name: /^Redo/ })];
  await undo.click();
  await expect(level(page)).toHaveValue("1");
  await undo.click();
  await expect(sends(page)).toHaveCount(0);
  await redo.click();
  await expect(sends(page)).toHaveCount(1);
  await expect(level(page)).toHaveValue("1");
  await redo.click();
  await expect(level(page)).toHaveValue("1.5");

  // A cable that is taken out is gone from the file, and a cable drawn is a cable that is not in the file until it is saved.
  await page.getByRole("button", { name: "Remove cable Oscillator → Master" }).click();
  await expect(sends(page)).toHaveCount(0);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(readFileSync(own.file, "utf8")).not.toContain("[[cable]]");
});

test("two oscillators go into one reverb: the second lane's oscillator is drawn to the reverb in the first lane's chain, and the file has the cable by the places of the lanes", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  // A reverb at the end of the first lane's chain (after its filter), and a second lane with an oscillator.
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  await expect(node(page, "Reverb")).toBeVisible();
  await page.getByRole("button", { name: "Add an instrument" }).click();
  await page.getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(node(page, "Oscillator 2")).toBeVisible();
  await expect(node(page, "Oscillator 1")).toBeVisible();

  // The canvas moves to show a node that was added, and stops: a cable that is dragged at once would start where its port was a moment before. Then the second lane's output goes to the reverb's input.
  await page.getByRole("button", { name: "Fit View" }).click();
  await expect(node(page, "Oscillator 2")).toBeVisible();
  await page.waitForTimeout(600);
  await drag(page, audioOut(node(page, "Oscillator 2")), audioIn(node(page, "Reverb")));
  await expect(sends(page)).toHaveCount(1);
  await expect(page.getByRole("slider", { name: "Level of Oscillator 2 → Reverb", exact: true })).toHaveValue("1");

  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(readFileSync(own.file, "utf8")).toMatch(/\[\[cable\]\]\nfrom = "instrument\[1\]"\nto = "instrument\[0\]\.effect\[1\]"\n/);
});
