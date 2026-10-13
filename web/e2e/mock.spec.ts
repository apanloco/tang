import { expect, test } from "@playwright/test";

interface Session {
  graph: { nodes: { name: string }[] };
}

test("with the mock engine the page shows the session without any server", async ({ page }) => {
  await page.goto("/?engine=mock");
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByText("mock engine")).toBeVisible();
  const session = JSON.parse((await page.locator(".tang-session").textContent()) ?? "") as Session;
  expect(session.graph.nodes.map((node) => node.name)).toEqual(expect.arrayContaining(["Oscillator", "Filter"]));
});
