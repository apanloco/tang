import { readFileSync } from "node:fs";

import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { SESSION_FILE, TANG } from "../playwright.config";
import { still } from "./canvas";
import { resetTracer, sessionAccordingToTheServer } from "./support";

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
  await expect(page.getByRole("slider", { name: "cutoff", exact: true })).toBeVisible();
  await page.getByText("Session as JSON").click();
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

test.afterEach(async ({ page }) => {
  await resetTracer(page);
});

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

  // The resonance knob of the filter shows it.
  await expect(page.getByRole("slider", { name: "resonance" })).toHaveAttribute("aria-valuenow", "3.14159");
});

// -- The keys of the computer --

test("a key of the computer plays a note on the real server: the master's meter rises while it is down and falls when it is up", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("status")).toHaveText("Connected");
  const meter = page.getByRole("meter", { name: "Master level" });
  const level = async (): Promise<number> => Number(await meter.getAttribute("aria-valuenow"));
  // Quiet to begin with: nothing is playing (the meter's floor is -60 dB).
  await expect.poll(level, { message: "the master is quiet before a key" }).toBeLessThan(-50);

  await page.keyboard.down("z");
  await expect.poll(level, { message: "the master has a level while a key is down" }).toBeGreaterThan(-50);
  await page.keyboard.up("z");
  await expect.poll(level, { message: "the master is quiet again when the key is up" }).toBeLessThan(-50);

  // The octave keys change what a key plays, and the page says which octave it is at.
  await expect(page.getByText("Keys: octave 4")).toBeVisible();
  await page.keyboard.press("]");
  await expect(page.getByText("Keys: octave 5")).toBeVisible();
  await page.keyboard.down("z");
  await expect.poll(level, { message: "a key plays at the new octave" }).toBeGreaterThan(-50);
  await page.keyboard.up("z");
  await expect.poll(level).toBeLessThan(-50);
});

// -- Building modulation on the real session --

/** Where the dot of the knob called `name` is on its sweep right now, 0 to 1; null if it is not shown (nothing drives it). */
async function dotOf(page: Page, name: string): Promise<number | null> {
  return page.evaluate((label) => {
    const slider = document.querySelector(`[role="slider"][aria-label="${label}"]`);
    const dot = slider?.closest(".tang-knob")?.querySelector(".tang-knob__dot");
    if (dot?.getAttribute("visibility") !== "visible") return null;
    const x = Number(dot.getAttribute("cx")) - 24;
    const y = 24 - Number(dot.getAttribute("cy"));
    return ((Math.atan2(x, y) * 180) / Math.PI + 135) / 270;
  }, name);
}

test("a modulator added from the menu and a cable dragged to a parameter are in the server's session, and the engine runs them", async ({ page }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto("/");
  await expect(page.getByRole("status")).toHaveText("Connected");
  await resetTracer(page);
  await expect(page.getByRole("slider", { name: "resonance" })).toBeVisible();
  expect(await dotOf(page, "resonance")).toBeNull(); // nothing drives it

  await page.getByRole("button", { name: "Add a modulator to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Envelope" }).click();
  const envelope = page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: /^ADSR$/ }) });
  await expect(envelope).toBeVisible();
  await still(envelope);
  const added = await sessionAccordingToTheServer(page);
  const adsr = added.graph.nodes.find((node) => node.name === "ADSR");
  expect(adsr?.source?.type).toBe("envelope");
  // (The session's own, which hears the lane that the "+" was on: step 94.)
  expect(adsr?.parent).toBeNull();
  expect(adsr?.hears).toBe(added.graph.nodes.find((node) => node.name === "Oscillator")?.id);

  // Drag its output to the resonance knob's port.
  const from = await envelope.locator('.react-flow__handle[data-handleid="modOut"]').boundingBox();
  const to = await page.locator(".tang-cell").filter({ has: page.locator('[aria-label="resonance"]') }).locator(".react-flow__handle").boundingBox();
  if (!from || !to) throw new Error("no handles");
  await page.mouse.move(from.x + from.width / 2, from.y + from.height / 2);
  await page.mouse.down();
  await page.mouse.move(to.x + to.width / 2, to.y + to.height / 2, { steps: 20 });
  await page.mouse.up();
  await expect(page.getByRole("slider", { name: "Depth of ADSR → Filter: resonance" })).toHaveValue("0.5");

  const drawn = await sessionAccordingToTheServer(page);
  const cable = drawn.graph.edges.find((edge) => edge.kind === "modulation" && edge.from?.node === adsr?.id);
  expect(cable?.amount).toBe(0.5);
  expect(cable?.to?.port).toEqual({ type: "param", index: 1 });
  expect(cable?.to?.node).toBe(drawn.graph.nodes.find((node) => node.name === "Filter")?.id);

  // The engine applies it: an envelope at rest is 0, which takes the resonance (2, from 0.5 to 10) down by half
  // of the way to its minimum, to 1.25: a dot a little below where the knob is set.
  await expect.poll(() => dotOf(page, "resonance")).not.toBeNull();
  const setAt = (2 - 0.5) / (10 - 0.5);
  const dot = (await dotOf(page, "resonance")) ?? Number.NaN;
  expect(dot).toBeCloseTo(((1.25 - 0.5) / (10 - 0.5)), 1);
  expect(dot).toBeLessThan(setAt);

  // And taken out with its button: the cable goes with it, and the resonance is its own again.
  await page.getByRole("button", { name: "Remove ADSR" }).click();
  await expect(envelope).toHaveCount(0);
  const gone = await sessionAccordingToTheServer(page);
  expect(gone.graph.nodes.find((node) => node.name === "ADSR")).toBeUndefined();
  expect(gone.graph.edges.filter((edge) => edge.kind === "modulation")).toHaveLength(1);
  await expect.poll(() => dotOf(page, "resonance")).toBeNull();
});

test("a cable dragged from an envelope to the rate of the LFO is in the server's session, and goes with its button", async ({ page }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto("/");
  await expect(page.getByRole("status")).toHaveText("Connected");
  await resetTracer(page);
  await page.getByRole("button", { name: "Add a modulator to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Envelope" }).click();
  const envelope = page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: /^ADSR$/ }) });
  const lfo = page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: /^LFO$/ }) });
  await expect(envelope).toBeVisible();
  await still(envelope);

  const from = await envelope.locator('.react-flow__handle[data-handleid="modOut"]').boundingBox();
  const to = await lfo.locator('.react-flow__handle[data-handleid="rate"]').boundingBox();
  if (!from || !to) throw new Error("no handles");
  await page.mouse.move(from.x + from.width / 2, from.y + from.height / 2);
  await page.mouse.down();
  await page.mouse.move(to.x + to.width / 2, to.y + to.height / 2, { steps: 20 });
  await page.mouse.up();
  await expect(page.getByRole("slider", { name: "Depth of ADSR → LFO: rate" })).toHaveValue("0.5");

  const drawn = await sessionAccordingToTheServer(page);
  const adsr = drawn.graph.nodes.find((node) => node.name === "ADSR");
  const cable = drawn.graph.edges.find((edge) => edge.kind === "modulation" && edge.from?.node === adsr?.id);
  expect(cable?.amount).toBe(0.5);
  expect(cable?.to?.port).toEqual({ type: "rate" });
  expect(cable?.to?.node).toBe(drawn.graph.nodes.find((node) => node.name === "LFO")?.id);
  expect(drawn.graph.edges.filter((edge) => edge.kind === "modulation")).toHaveLength(2);

  // Taken out with its button: the cable goes, and the envelope stays; then the envelope goes too.
  await page.getByRole("button", { name: "Remove cable ADSR → LFO: rate" }).click();
  await expect(page.getByRole("slider", { name: "Depth of ADSR → LFO: rate" })).toHaveCount(0);
  const back = await sessionAccordingToTheServer(page);
  expect(back.graph.edges.filter((edge) => edge.kind === "modulation")).toHaveLength(1);
  expect(back.graph.nodes.find((node) => node.name === "ADSR")).toBeDefined();
  await page.getByRole("button", { name: "Remove ADSR" }).click();
  await expect(envelope).toHaveCount(0);
});

test("a cable drawn from the list, and one taken out with its button, are in the server's session", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("status")).toHaveText("Connected");
  await resetTracer(page);
  await page.getByRole("button", { name: "Add a cable from LFO" }).click();
  await page.getByRole("menuitem", { name: "drive", exact: true }).click();
  await expect(page.getByRole("slider", { name: "Depth of LFO → Filter: drive" })).toBeVisible();
  const drawn = await sessionAccordingToTheServer(page);
  expect(drawn.graph.edges.filter((edge) => edge.kind === "modulation")).toHaveLength(2);
  await page.getByRole("button", { name: "Remove cable LFO → Filter: drive" }).click();
  await expect(page.getByRole("slider", { name: "Depth of LFO → Filter: drive" })).toHaveCount(0);
  const back = await sessionAccordingToTheServer(page);
  expect(back.graph.edges.filter((edge) => edge.kind === "modulation")).toHaveLength(1);
});

// -- Saving --

test("the session is saved from the page, by the button and by the keys, to the server's own file", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("status")).toHaveText("Connected");
  const resonance = page.getByRole("slider", { name: "resonance" });
  const saved = page.getByText("Saved", { exact: true });
  const unsaved = page.getByText("Unsaved changes");
  const save = page.getByRole("button", { name: "Save", exact: true });

  // A change makes the session unsaved, and the file is as it was.
  const before = readFileSync(SESSION_FILE, "utf8");
  await resonance.focus();
  await page.keyboard.press("ArrowUp");
  await expect(unsaved).toBeVisible();
  expect(readFileSync(SESSION_FILE, "utf8")).toBe(before);

  // The button writes it: the file has the change, and the page says there is nothing unsaved.
  await save.click();
  await expect(saved).toBeVisible();
  await expect(save).toBeDisabled();
  const first = readFileSync(SESSION_FILE, "utf8");
  expect(first).not.toBe(before);
  expect(first).toContain("resonance");

  // And Control+S does too, which is not the browser's save-the-page.
  await resonance.focus();
  await page.keyboard.press("ArrowUp");
  await expect(unsaved).toBeVisible();
  await page.keyboard.press("ControlOrMeta+s");
  await expect(saved).toBeVisible();
  expect(readFileSync(SESSION_FILE, "utf8")).not.toBe(first);
});

test("an effect is put in the lane from the menu, saved, and taken out again by its button", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("status")).toHaveText("Connected");
  const effect = (name: string) => page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: new RegExp(`^${name}$`) }) });
  const save = page.getByRole("button", { name: "Save", exact: true });
  const saved = page.getByText("Saved", { exact: true });

  // The menu has what the program found installed: the built-ins are always there, and whatever else the machine has follows them.
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  const menu = page.getByRole("menu", { name: "Add an effect to Oscillator" });
  const builtins = menu.getByRole("group", { name: "Built-in" });
  await expect(builtins.getByRole("menuitem")).toHaveText(["Delay", "Drive", "Filter", "Reverb"]);
  await builtins.getByRole("menuitem", { name: "Reverb" }).click();

  // The node is on the canvas, in the chain after the filter; the server has it, and the session has something to save.
  await expect(effect("Reverb")).toBeVisible();
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  const session = await sessionAccordingToTheServer(page);
  expect(session.graph.nodes.map((node) => node.name)).toEqual(["Oscillator", "Filter", "Reverb", "LFO", "Master"]);
  await save.click();
  await expect(saved).toBeVisible();
  expect(readFileSync(SESSION_FILE, "utf8")).toContain('plugin = "builtin:reverb"');

  // Its button takes it out, and the focus goes back to the one that adds an effect to its lane.
  await effect("Reverb").getByRole("button", { name: "Remove Reverb" }).click();
  await expect(effect("Reverb")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Add an effect to Oscillator" })).toBeFocused();
  expect((await sessionAccordingToTheServer(page)).graph.nodes.map((node) => node.name)).toEqual(["Oscillator", "Filter", "LFO", "Master"]);
  await save.click();
  await expect(saved).toBeVisible();
  expect(readFileSync(SESSION_FILE, "utf8")).not.toContain("builtin:reverb");
});
