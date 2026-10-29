import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Page } from "@playwright/test";

import { audioIn, audioOut, drag, node, open, sends, showAll, still } from "./canvas";
import { test } from "./own";
import { loudest, masterLevel } from "./sound";

// The audio input from the page (docs/web-ui.md step 99; the user's "add my computer microphone as an input that I can connect to other devices via audio cable"): `builtin:input` is an instrument that is
// added from the bar's "+ Instrument" menu in a lane of its own, which starts bare (its way out is cut), so that it is heard only through the cables that are drawn from it. On the null backend, which has no
// sound card to listen to, its input is a tone of 220 Hz in place of the microphone: the real program is run on it, and what is heard is the tone. The first test is against the mock engine in the page
// (what the canvas draws), the last against the real program (what is heard, what the file keeps and what the history does).

/** The links of the audio path that the canvas draws: cables with no level (the amber ones that have a slider are the cables that a person drew). */
const links = (page: Page) => page.locator(".tang-cable--audio");

/** What the saved file says of the audio input's lane before its first sub-table: the keys of its `[[instrument]]` (`plugin`, the way out if it is cut, the volume and the rest). */
const laneOfInput = (file: string): string =>
  (file.split(/^\[\[instrument\]\]\n/m).slice(1).find((table) => table.startsWith('plugin = "builtin:input"')) ?? "").split(/^\[/m)[0] ?? "";

/** The audio input, added from the bar's menu: the lane that it is in is on the canvas, has stopped moving, and every node is in the view (a cable is dragged from the lane's output to the master's input). */
async function addInput(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Add an instrument" }).click();
  await page.getByRole("menu", { name: "Add an instrument" }).getByRole("menuitem", { name: "Audio Input" }).click();
  await expect(node(page, "Audio Input")).toBeVisible();
  await still(node(page, "Audio Input"));
  await showAll(page);
}

mock("the audio input is added from the bar in a lane that has no way out: its node has a gain, a pan and the device to listen to, no link goes from it, and a cable drawn to the master gives the way out back", async ({ page }) => {
  await open(page);
  // (The tracer's lane has its instrument and a filter: a link from the one to the other and one from the filter to the master.)
  await expect(links(page)).toHaveCount(2);
  await addInput(page);

  // The lane is bare: no link, and no button on the canvas that would take one out. The oscillator that is added the same way has its link to the master (the instruments test says so).
  await expect(links(page)).toHaveCount(2);
  await expect(page.getByRole("button", { name: "Remove cable Audio Input → Master" })).toHaveCount(0);
  await expect(sends(page)).toHaveCount(0);

  // Its node has the three knobs of the plugin, at what it starts at (a gain of 1, a pan in the middle, the system's default input), the volume of the lane, and the menu that sends its audio.
  const input = node(page, "Audio Input");
  await expect(input.getByRole("slider", { name: "gain", exact: true })).toHaveAttribute("aria-valuenow", "1");
  await expect(input.getByRole("slider", { name: "pan", exact: true })).toHaveAttribute("aria-valuenow", "0");
  await expect(input.getByRole("combobox", { name: "device", exact: true })).toHaveValue("0");
  await expect(input.getByRole("slider", { name: "Volume", exact: true })).toHaveAttribute("aria-valuenow", "1");
  await expect(page.getByRole("button", { name: "Send audio from Audio Input", exact: true })).toBeVisible();

  // Its instrument takes no notes (step 99c), so that its card has none of what a lane has for them: no key range, no pattern to record, no input of events and no menu that takes notes in. The cells that
  // are not about notes are the lane's all the same: its group, its mute and its solo. (The tracer's lane, which plays notes, has its keys and its pattern: one of each on the canvas.)
  await expect(page.getByLabel(/^Lowest key of /)).toHaveCount(1);
  await expect(page.getByRole("group", { name: /^Pattern of / })).toHaveCount(1);
  await expect(input.getByLabel(/^(Lowest|Highest) key of /)).toHaveCount(0);
  await expect(input.getByRole("group", { name: "Pattern of Audio Input" })).toHaveCount(0);
  await expect(input.getByRole("button", { name: "Record a pattern on Audio Input" })).toHaveCount(0);
  await expect(input.getByRole("button", { name: "Take notes into Audio Input" })).toHaveCount(0);
  await expect(input.locator('.react-flow__handle[data-handleid="eventsIn"]')).toHaveCount(0);
  await expect(input.getByRole("combobox", { name: "Group of Audio Input", exact: true })).toBeVisible();
  await expect(input.getByRole("button", { name: "Mute Audio Input", exact: true })).toBeVisible();
  // Its card has the level of its lane (step 100), as every lane's and every group's has: one meter, along the top edge of the card.
  await expect(input.getByRole("meter", { name: "Level of Audio Input", exact: true })).toHaveCount(1);

  // A cable drawn from its output to the master is the way out given back, a link like every lane's (it has no slider: it has no level), and the history says so.
  await drag(page, audioOut(input), audioIn(node(page, "Master")));
  await expect(links(page)).toHaveCount(3);
  await expect(sends(page)).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Remove cable Audio Input → Master", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: /^Undo/ })).toHaveAccessibleName("Undo give the lane of Audio Input its way out");

  // Undo cuts it again, and the lane that undo took out with the add is put back bare by redo.
  await page.getByRole("button", { name: /^Undo/ }).click();
  await expect(links(page)).toHaveCount(2);
  await page.getByRole("button", { name: /^Undo/ }).click();
  await expect(input).toHaveCount(0);
  await page.getByRole("button", { name: /^Redo/ }).click();
  await expect(input).toBeVisible();
  await expect(links(page)).toHaveCount(2);
});

test("the audio input on the real program is silent until a cable is drawn from it, is heard as a tone through the cable (the null backend has no microphone) and follows its gain, its mute and its cable; the file keeps the lane bare and the cable, and a page that is opened again draws them", async ({ page, own }) => {
  // (A good many waits for silence, each the better part of a second.)
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const heard = async (what: string): Promise<number> => {
    await expect.poll(() => masterLevel(page), { message: `${what} is heard`, timeout: 15_000 }).toBeGreaterThan(-30);
    return loudest(page, 400);
  };
  const silent = async (what: string): Promise<void> => {
    await expect.poll(() => masterLevel(page), { message: `${what} is silent`, timeout: 15_000 }).toBeLessThan(-50);
    expect(await loudest(page, 600), `${what} stays silent`).toBeLessThan(-50);
  };
  const saved = async (): Promise<string> => {
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };

  // Nothing is played, and nothing is heard; the input is added, and it is still not heard: its lane has no way out.
  await expect(links(page)).toHaveCount(2);
  await silent("the session with no key held");
  await addInput(page);
  await expect(links(page)).toHaveCount(2);
  await silent("an audio input that no cable leaves");
  // Its own level (step 100: the meter along the top edge of its card) reads the tone all the same, before any cable is drawn: the lane is bare and nothing of it is in the mix, and what says that it hears is its level
  // (a microphone that the system does not give is a level that stays at the floor). A sine of 0.25 is -12 dB.
  const cardLevel = node(page, "Audio Input").getByRole("meter", { name: "Level of Audio Input", exact: true });
  await expect.poll(async () => Number(await cardLevel.getAttribute("aria-valuenow")), { message: "the level of the bare lane reads the tone", timeout: 15_000 }).toBeGreaterThan(-14);
  expect(Number(await cardLevel.getAttribute("aria-valuenow"))).toBeLessThan(-10);
  // The file has the lane as a bare one: its way out is cut, and there is no cable.
  const bare = await saved();
  expect(laneOfInput(bare)).toContain('route = "none"\n');
  expect(bare).not.toContain("[[cable]]");

  // A cable from it to the master, and the tone is heard (a sine of 0.25 at a gain of 1: about -12 dB, which the lane's volume of 1 leaves as it is). The cable is the lane's way out given back:
  // it is a link, with no slider, and the file has no word of a way out for the lane and no cable of its own.
  const input = node(page, "Audio Input");
  // (The program's graph says that the instrument takes no notes: the card has no keys, no pattern and no input of events, as the mock engine's has.)
  await expect(input.getByLabel(/^(Lowest|Highest) key of /)).toHaveCount(0);
  await expect(input.getByRole("group", { name: "Pattern of Audio Input" })).toHaveCount(0);
  await expect(input.getByRole("button", { name: "Take notes into Audio Input" })).toHaveCount(0);
  await drag(page, audioOut(input), audioIn(node(page, "Master")));
  await expect(links(page)).toHaveCount(3);
  await expect(sends(page)).toHaveCount(0);
  const level = await heard("the audio input through its cable");
  expect(level, "a tone of 0.25").toBeLessThan(-8);
  expect(level, "a tone of 0.25").toBeGreaterThan(-16);

  // Its gain is the level: all the way down is silence, and a gain of 4 (the top of the knob) is four times the tone, a peak at the top of the meter.
  const gain = input.getByRole("slider", { name: "gain", exact: true });
  await gain.focus();
  await page.keyboard.press("Home");
  await expect(gain).toHaveAttribute("aria-valuenow", "0");
  await silent("the audio input at a gain of 0");
  await page.keyboard.press("End");
  await expect(gain).toHaveAttribute("aria-valuenow", "4");
  expect(await heard("the audio input at a gain of 4"), "four times as loud").toBeGreaterThan(-4);
  await gain.dblclick();
  await expect(gain).toHaveAttribute("aria-valuenow", "1");
  await heard("the audio input at its own gain again");

  // The lane's mute is a mute (the lane is bare, and its cable is what it sends: a muted lane sends nothing).
  await page.getByRole("button", { name: "Mute Audio Input", exact: true }).click();
  await silent("a muted audio input");
  // (A lane that is muted has no level: its meter is after the fade that the mute makes.)
  await expect.poll(async () => Number(await cardLevel.getAttribute("aria-valuenow")), { message: "the level of a muted lane falls to the floor", timeout: 15_000 }).toBeLessThan(-50);
  await page.getByRole("button", { name: "Mute Audio Input", exact: true }).click();
  await heard("the audio input, unmuted");

  // The file keeps the lane as it is, with its way out (the link is given back, and the file has no word of it).
  const connected = await saved();
  expect(laneOfInput(connected)).not.toContain("route");
  expect(connected).not.toContain("[[cable]]");

  // The link taken out (it has its button, as every link has): the lane is bare and silent again, and the file has the way out cut once more.
  await page.getByRole("button", { name: "Remove cable Audio Input → Master", exact: true }).click();
  await expect(links(page)).toHaveCount(2);
  await silent("the audio input, disconnected again");
  expect(laneOfInput(await saved())).toContain('route = "none"\n');
  await page.getByRole("button", { name: "Undo cut the way out of the lane of Audio Input", exact: true }).click();
  await heard("the audio input, with its way out given back by undo");

  // A page that is opened again draws what the program has: the lane, with its link to the master, and the tone is still heard.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(node(page, "Audio Input")).toBeVisible();
  await expect(links(page)).toHaveCount(3);
  await heard("the audio input, after a page was opened again");
});
