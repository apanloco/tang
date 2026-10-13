import { expect, test } from "@playwright/test";

interface Session {
  graph: { nodes: { name: string }[] };
}

test("with the mock engine the page shows the session as a patch, without any server", async ({ page }) => {
  await page.goto("/?engine=mock");
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByText("mock engine")).toBeVisible();
  // The tracer: an oscillator through a filter, an LFO on its cutoff, and the master.
  await expect(page.getByRole("slider", { name: "cutoff" })).toBeVisible();
  await expect(page.locator(".tang-node__name")).toHaveText(["Oscillator", "Filter", "LFO 0.5Hz sine", "Master"]);
  await expect(page.locator(".react-flow__edge")).toHaveCount(3);
});

test("the session as the server sent it can be opened, and is the same one", async ({ page }) => {
  await page.goto("/?engine=mock");
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.locator(".tang-session")).toHaveCount(0); // closed, and not written, until asked for
  await page.getByText("Session as JSON").click();
  const session = JSON.parse((await page.locator(".tang-session").textContent()) ?? "") as Session;
  expect(session.graph.nodes.map((node) => node.name)).toEqual(expect.arrayContaining(["Oscillator", "Filter"]));
});
