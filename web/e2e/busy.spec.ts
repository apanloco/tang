import { expect } from "@playwright/test";

import { node } from "./canvas";
import { test } from "./own";

// A server that is slow to answer, seen from the page, on the real program (a program of its own, which the test adds a group to): a plugin that is slow
// to load is done by the program on the thread that answers, so that the page waits for its answer. There is no such plugin on the machine of a test (they load in a third
// of a second at most),
// so the words of the server are held back on the way to the page instead, which is what a slow answer looks like from it.

test("a request that the server is slow to answer shows Working… until it is answered, and the page is where it was meanwhile", async ({ page, own }) => {
  let holding = false;
  const held: (() => void)[] = [];
  await page.routeWebSocket(/\/ws(\?|$)/, (socket) => {
    const server = socket.connectToServer();
    socket.onMessage((message) => {
      server.send(message);
    });
    server.onMessage((message) => {
      // (Only what is said in words: the telemetry, in bytes, goes on, as it does while a plugin loads.)
      if (holding && typeof message === "string") {
        held.push(() => {
          socket.send(message);
        });
      } else {
        socket.send(message);
      }
    });
  });
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const busy = page.getByRole("progressbar", { name: "Working" });
  await expect(busy).toHaveCount(0);
  await expect(node(page, "Oscillator")).toBeVisible();

  // (Exactly: the button of Undo says "Undo add a group" once there is one to undo.)
  // A request that is answered at once is not slow: the group is there, and the light never came (it is not there a second after, longer than it waits).
  await page.getByRole("button", { name: "Add a group", exact: true }).click();
  await expect(node(page, "Group 1")).toBeVisible();
  await page.waitForTimeout(700);
  await expect(busy).toHaveCount(0);

  // One that is not answered: the light comes, over the canvas and out of the way, and the page is as it was.
  holding = true;
  const before = await node(page, "Oscillator").boundingBox();
  await page.getByRole("button", { name: "Add a group", exact: true }).click();
  await expect(busy).toBeVisible();
  await expect(busy).toHaveText("Working…");
  await expect(node(page, "Group 2")).toHaveCount(0);
  expect(await node(page, "Oscillator").boundingBox()).toEqual(before);
  // The light is over the canvas and does not take the pointer: the node under it can be pressed as before.
  await expect(busy).toHaveCSS("pointer-events", "none");
  await expect(busy).toHaveCSS("position", "absolute");

  // The answer comes: the light goes, and what was asked is there.
  holding = false;
  for (const tell of held.splice(0)) tell();
  await expect(busy).toHaveCount(0);
  await expect(node(page, "Group 2")).toBeVisible();
});
