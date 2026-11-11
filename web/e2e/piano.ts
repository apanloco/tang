import { expect } from "@playwright/test";
import type { Page } from "@playwright/test";

// What the tests that play the page's piano, or read a key of it that is lit, have in common.

/** Open the keys of the piano, which are shut to begin with (step 119): the bar's button draws them, and they are there to press or to read when it has. */
export async function openTheKeys(page: Page): Promise<void> {
  await page.getByRole("region", { name: "Piano" }).getByRole("button", { name: "Keyboard" }).click();
  await expect(page.getByRole("group", { name: "Piano keys" })).toBeVisible();
}
