import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { expect } from "@playwright/test";
import type { Page } from "@playwright/test";

import { test } from "./own";

// Save as, on the real program (a program of its own, with a copy of the tracer in a directory of its own: what it writes there is read). The page asks for a name;
// the program says what a name may be, and puts the file beside the one the session has.

async function open(page: Page, url: string): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByText("tracer.toml", { exact: true })).toBeVisible();
}

const opener = (page: Page) => page.getByRole("button", { name: "Save as…", exact: true });
const field = (page: Page) => page.getByRole("textbox", { name: "File name", exact: true });
const save = (page: Page) => page.getByRole("button", { name: "Save", exact: true });

/** The session files that the directory of the program has, by name. */
const sessions = (file: string): string[] =>
  readdirSync(dirname(file))
    .filter((name) => name.endsWith(".toml"))
    .sort();

test("a session saved as another name is written beside its file and is that file from then on: the bar shows it, a save writes there and not into the first, and a second page is told", async ({ page, browser, own }) => {
  await open(page, own.url);
  const dir = dirname(own.file);
  const first = readFileSync(own.file, "utf8");

  // The field has the name of the file the session has, selected and with the focus, in place of the button.
  await opener(page).click();
  await expect(field(page)).toBeFocused();
  await expect(field(page)).toHaveValue("tracer.toml");
  await expect(opener(page)).toHaveCount(0);
  await field(page).fill("copy");
  await field(page).press("Enter");

  // The session is that file now, and has nothing unsaved; the field is gone and the button has the focus again.
  await expect(page.getByText("copy.toml", { exact: true })).toBeVisible();
  await expect(page.getByText("tracer.toml", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  await expect(field(page)).toHaveCount(0);
  await expect(opener(page)).toBeFocused();

  // The copy is the session, beside the file the session had, which was not written.
  expect(sessions(own.file)).toEqual(["copy.toml", "tracer.toml"]);
  const copy = readFileSync(join(dir, "copy.toml"), "utf8");
  expect(copy).toContain('plugin = "builtin:filter"');
  expect(copy).toContain('param = "cutoff"');
  expect(readFileSync(own.file, "utf8")).toBe(first);

  // A page that is opened now is told the name the session has.
  const other = await browser.newPage();
  await other.setViewportSize({ width: 1600, height: 1000 });
  await other.goto(own.url);
  await expect(other.getByRole("status")).toHaveText("Connected");
  await expect(other.getByText("copy.toml", { exact: true })).toBeVisible();

  // A change and a save: the new file has it, and the first does not.
  await page.getByRole("button", { name: "Lock to scale" }).click();
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  await expect(other.getByText("Unsaved changes")).toBeVisible();
  await save(page).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(readFileSync(join(dir, "copy.toml"), "utf8")).toContain("locked = true");
  expect(readFileSync(own.file, "utf8")).toBe(first);
  expect(sessions(own.file)).toEqual(["copy.toml", "tracer.toml"]);

  // Another page saves the session as a third file, and this one is told.
  await opener(other).click();
  await field(other).fill("third.toml");
  await field(other).press("Enter");
  await expect(page.getByText("third.toml", { exact: true })).toBeVisible();
  expect(sessions(own.file)).toEqual(["copy.toml", "third.toml", "tracer.toml"]);
  expect(readFileSync(join(dir, "third.toml"), "utf8")).toContain("locked = true");
  await other.close();
});

test("a name that is a path, an absolute one and the name of a file that is there are refused in the program's words: the field stays with the name, nothing is written but what was asked for, and a name that is fine then is written", async ({
  page,
  own,
}) => {
  const dir = dirname(own.file);
  writeFileSync(join(dir, "other.toml"), "keep me\n");
  await open(page, own.url);
  await opener(page).click();

  const refuse = async (name: string, words: string): Promise<void> => {
    await field(page).fill(name);
    await field(page).press("Enter");
    await expect(page.getByText(words, { exact: true })).toBeVisible();
    await expect(field(page)).toHaveValue(name);
    await expect(field(page)).toBeFocused();
  };
  const folder = "a file name has no folder in it: `/` and `\\` are not allowed";
  await refuse("../escape", `Could not save the session as "../escape": ${folder}`);
  await refuse("/tmp/tang-e2e-escape", `Could not save the session as "/tmp/tang-e2e-escape": ${folder}`);
  await refuse("other", 'Could not save the session as "other": a file called other.toml is there already');
  await refuse("com1", 'Could not save the session as "com1": com1 is a name that Windows keeps for itself');

  // Nothing was written, not here and not where the names pointed; the file that was there is as it was, and the bar has the file it had.
  expect(sessions(own.file)).toEqual(["other.toml", "tracer.toml"]);
  expect(readFileSync(join(dir, "other.toml"), "utf8")).toBe("keep me\n");
  expect(existsSync(join(dir, "..", "escape.toml"))).toBe(false);
  expect(existsSync("/tmp/tang-e2e-escape.toml")).toBe(false);
  await expect(page.getByText("tracer.toml", { exact: true })).toBeVisible();

  // The same field takes a name that is fine.
  await field(page).fill("fine");
  await field(page).press("Enter");
  await expect(page.getByText("fine.toml", { exact: true })).toBeVisible();
  await expect(field(page)).toHaveCount(0);
  expect(sessions(own.file)).toEqual(["fine.toml", "other.toml", "tracer.toml"]);
  expect(readFileSync(join(dir, "other.toml"), "utf8")).toBe("keep me\n");

  // Saved as the name it has is a save, though that file is there: the name that the session has is not refused.
  await opener(page).click();
  await field(page).fill("fine");
  await field(page).press("Enter");
  await expect(field(page)).toHaveCount(0);
  await expect(page.getByText("fine.toml", { exact: true })).toBeVisible();
});

test("Control+Shift+S and Command+Shift+S open the field with the name of the file selected, letters typed in it are text and not notes, and Escape leaves it with the focus on the button", async ({ page, own }) => {
  await open(page, own.url);
  const key = (note: number) => page.locator(`[data-note="${String(note)}"]`);

  // (The control: Q is the C of the octave above the lower row, C4, when the focus is not in a field.)
  await page.keyboard.down("q");
  await expect(key(60)).toHaveAttribute("data-held", "");
  await page.keyboard.up("q");
  await expect(key(60)).not.toHaveAttribute("data-held", "");

  await page.keyboard.press("Control+Shift+S");
  await expect(field(page)).toBeFocused();
  await expect(field(page)).toHaveValue("tracer.toml");
  expect(await field(page).evaluate((input: HTMLInputElement) => [input.selectionStart, input.selectionEnd])).toEqual([0, "tracer.toml".length]);
  // A key typed in it is a letter: it replaces what was selected, and no note is lit (the telemetry comes thirty times a second: a note would be by now).
  await page.keyboard.down("q");
  await expect(field(page)).toHaveValue("q");
  await page.waitForTimeout(400);
  await expect(key(60)).not.toHaveAttribute("data-held", "");
  await page.keyboard.up("q");

  await page.keyboard.press("Escape");
  await expect(field(page)).toHaveCount(0);
  await expect(opener(page)).toBeFocused();
  expect(sessions(own.file)).toEqual(["tracer.toml"]);

  // Command does the same, and a click on the button is another way in; what was typed before is not kept.
  await page.keyboard.press("Meta+Shift+S");
  await expect(field(page)).toBeFocused();
  await expect(field(page)).toHaveValue("tracer.toml");
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(field(page)).toHaveCount(0);
  await expect(opener(page)).toBeFocused();
  await opener(page).click();
  await expect(field(page)).toBeFocused();
  await expect(save(page)).toBeDisabled();
  expect(sessions(own.file)).toEqual(["tracer.toml"]);
});
