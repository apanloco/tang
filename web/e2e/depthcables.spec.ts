import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Page } from "@playwright/test";

import { addEnvelope, cables, drag, expectNodesApart, node, output } from "./canvas";
import { test } from "./own";

// Driving how deep a cable goes, from the page, on the real program (a program of its own: the file it saves is read): a cable dragged from an
// envelope to the row of the LFO's cable is in the session that the server keeps and in the file, undo takes it out and redo puts it back, and a
// cable that is taken out takes with it the cables that drive its depth.

const WORDS = "ADSR → depth of LFO → Filter: cutoff";

async function open(page: Page, url: string): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff" })).toBeVisible();
}

test("a cable dragged to the depth of another is in the server's session and the saved file, undo takes it out and redo puts it back", async ({ page, own }) => {
  await open(page, own.url);
  const envelope = await addEnvelope(page);
  const lfo = node(page, "LFO");
  const rowPort = lfo.locator(".tang-cable-row .react-flow__handle");
  await drag(page, output(envelope), rowPort);
  const depth = page.getByRole("slider", { name: `Depth of ${WORDS}` });
  await expect(depth).toHaveValue("0.5");
  await expect(cables(page)).toHaveCount(2);
  await expect(rowPort).toHaveClass(/tang-port--driven/);
  await expectNodesApart(page);

  // The server has it: a page that is opened again draws it, from the session that the server tells.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByRole("slider", { name: `Depth of ${WORDS}` })).toHaveValue("0.5");
  await expect(cables(page)).toHaveCount(2);

  // The file has it: the envelope's cable to the LFO's cable (the first target of the first modulator of the lane), at the depth it was made at.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved).toMatch(/\[\[instrument\.modulator\.target\]\]\nmod_depth = \[0, 0\]\ndepth = 0\.5/);

  // Undo takes the cable out, with the page as it was before it (the row's port is not driven, the envelope stays), and redo draws it again.
  await page.getByRole("button", { name: "Undo" }).click();
  await expect(cables(page)).toHaveCount(1);
  await expect(page.getByRole("slider", { name: `Depth of ${WORDS}` })).toHaveCount(0);
  await expect(lfo.locator(".tang-cable-row .react-flow__handle")).not.toHaveClass(/tang-port--driven/);
  await expect(node(page, "ADSR")).toBeVisible();
  await page.getByRole("button", { name: "Redo" }).click();
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: `Depth of ${WORDS}` })).toHaveValue("0.5");
  await expect(lfo.locator(".tang-cable-row .react-flow__handle")).toHaveClass(/tang-port--driven/);
});

test("a cable that is taken out takes with it the cables that drive its depth, and a cable to a depth is taken out alone", async ({ page, own }) => {
  await open(page, own.url);
  const envelope = await addEnvelope(page);
  await drag(page, output(envelope), node(page, "LFO").locator(".tang-cable-row .react-flow__handle"));
  await expect(cables(page)).toHaveCount(2);

  // By its own button, the cable that drives the depth goes alone, and the LFO's stays, as it was.
  await page.getByRole("button", { name: `Remove cable ${WORDS}` }).click();
  await expect(cables(page)).toHaveCount(1);
  await expect(page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff" })).toHaveValue("0.4");
  await expect(node(page, "ADSR")).toBeVisible();

  // Drawn again, and the LFO's cable taken out: the one that drove its depth goes with it, and the row of it.
  await drag(page, output(envelope), node(page, "LFO").locator(".tang-cable-row .react-flow__handle"));
  await expect(cables(page)).toHaveCount(2);
  await page.getByRole("button", { name: "Remove cable LFO → Filter: cutoff" }).click();
  await expect(cables(page)).toHaveCount(0);
  await expect(page.locator(".tang-cable-row")).toHaveCount(0);
  await expect(node(page, "LFO")).toBeVisible();
  await expect(node(page, "ADSR")).toBeVisible();
});

test("the removal of a cable whose depth another cable drives is put back by undo, with the cable that drove it, and done again by redo", async ({ page, own }) => {
  await open(page, own.url);
  const envelope = await addEnvelope(page);
  const rowPort = node(page, "LFO").locator(".tang-cable-row .react-flow__handle");
  await drag(page, output(envelope), rowPort);
  await expect(cables(page)).toHaveCount(2);
  await page.getByRole("slider", { name: `Depth of ${WORDS}` }).fill("0.25");
  await expect(page.getByRole("slider", { name: `Depth of ${WORDS}` })).toHaveValue("0.25");

  // The LFO's cable goes, and the cable that drives its depth with it.
  await page.getByRole("button", { name: "Remove cable LFO → Filter: cutoff" }).click();
  await expect(cables(page)).toHaveCount(0);
  await expect(page.locator(".tang-cable-row")).toHaveCount(0);

  // Undo puts both back, each at the depth it had, and the row marks that a cable ends at it again.
  await page.getByRole("button", { name: "Undo" }).click();
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff", exact: true })).toHaveValue("0.4");
  await expect(page.getByRole("slider", { name: `Depth of ${WORDS}`, exact: true })).toHaveValue("0.25");
  await expect(rowPort).toHaveClass(/tang-port--driven/);
  await expectNodesApart(page);

  // Redo takes them out again, and undo again puts them back: the history was not given up for them.
  await page.getByRole("button", { name: "Redo" }).click();
  await expect(cables(page)).toHaveCount(0);
  await page.getByRole("button", { name: "Undo" }).click();
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: `Depth of ${WORDS}` })).toHaveValue("0.25");
});
