import { expect, test } from "@playwright/test";

import { TANG } from "../playwright.config";

// The program serves the interface itself, as a person who has only the program gets it:
// the page and its files from web/dist (`npm run build` first), and the WebSocket on the
// same address, with no Vite in between.
test.use({ baseURL: `http://127.0.0.1:${TANG}` });

interface Session {
  meta: { file: string | null };
  graph: { nodes: { name: string }[] };
}

test("the page the program serves connects to the program and shows its session", async ({ page }) => {
  const problems: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") problems.push(message.text());
  });
  page.on("pageerror", (error) => problems.push(error.message));

  const response = await page.goto("/");
  expect(response?.headers()["content-security-policy"]).toContain("default-src 'self'");
  expect(await page.content(), "web/dist is not built: run `npm run build`").toContain('id="root"');
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByText("server", { exact: true })).toBeVisible();
  await expect(page.getByRole("slider", { name: "cutoff" })).toBeVisible();
  await page.getByText("Session as JSON").click();
  const session = JSON.parse((await page.locator(".tang-session").textContent()) ?? "") as Session;
  expect(session.meta.file).toBe("tracer.toml");
  expect(session.graph.nodes.map((node) => node.name)).toEqual(expect.arrayContaining(["Oscillator", "Filter"]));
  // What the policy forbids is reported as an error: the page did everything it does within it.
  expect(problems).toEqual([]);
});

test("its files are kept for ever, and what is not a file is not found", async ({ request }) => {
  const html = await (await request.get("/")).text();
  const script = /src="(\/assets\/[^"]+\.js)"/.exec(html)?.[1];
  expect(script, "the page names a script").toBeDefined();
  const file = await request.get(script ?? "");
  expect(file.status()).toBe(200);
  expect(file.headers()["content-type"]).toContain("javascript");
  expect(file.headers()["cache-control"]).toContain("immutable");

  for (const path of ["/nothing", "/.gitkeep", "/api/session", "/assets/../../Cargo.toml"]) {
    expect((await request.get(path)).status(), path).toBe(404);
  }
});
