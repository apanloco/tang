import { expect, test } from "@playwright/test";

import { TANG } from "../playwright.config";

interface Session {
  meta: { file: string | null };
  graph: { nodes: { id: number; name: string }[] };
}

/** A message of the server, of which a test looks at the kind and, for a snapshot, the session. */
interface Message {
  type: string;
  snapshot?: Session;
}

test("the page connects to the real server and shows its session", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByText("server", { exact: true })).toBeVisible();
  const session = JSON.parse((await page.locator(".tang-session").textContent()) ?? "") as Session;
  expect(session.meta.file).toBe("tracer.toml");
  expect(session.graph.nodes.map((node) => node.name)).toEqual(expect.arrayContaining(["Oscillator", "Filter"]));
});

/** The messages of a socket, as they come, so that none is missed between two awaits. */
function inbox(socket: WebSocket): { next: () => Promise<Message> } {
  const arrived: Message[] = [];
  const waiting: ((message: Message) => void)[] = [];
  socket.addEventListener("message", (event: MessageEvent<unknown>) => {
    if (typeof event.data !== "string") return;
    const message = JSON.parse(event.data) as Message;
    const waiter = waiting.shift();
    if (waiter) waiter(message);
    else arrived.push(message);
  });
  return {
    next: () =>
      new Promise<Message>((resolve) => {
        const message = arrived.shift();
        if (message) resolve(message);
        else waiting.push(resolve);
      }),
  };
}

test("a change another front-end makes appears on the page", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("status")).toHaveText("Connected");

  // Another client, as the TUI would be: its own WebSocket to the server, which sends no Origin.
  const socket = new WebSocket(`ws://127.0.0.1:${TANG}/ws`);
  const messages = inbox(socket);
  expect((await messages.next()).type).toBe("hello");
  socket.send(JSON.stringify({ type: "subscribe" }));
  const session = (await messages.next()).snapshot;
  if (!session) throw new Error("no snapshot after subscribe");
  const filter = session.graph.nodes.find((node) => node.name === "Filter");
  expect(filter).toBeDefined();

  socket.send(JSON.stringify({ type: "request", reqId: 1, intent: { type: "setParam", node: filter?.id, param: 1, value: 3.14159 } }));
  // (The requester hears the event, and then the answer.)
  let reply = await messages.next();
  while (reply.type !== "ack") reply = await messages.next();
  socket.close();

  await expect(page.locator(".tang-session")).toContainText("3.14159");
});
