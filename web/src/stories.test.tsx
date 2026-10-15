// @vitest-environment jsdom
// Every story of every component renders: a story that throws is found here, in the
// ordinary tests, and not when somebody opens Storybook.
import { composeStories } from "@storybook/react-vite";
import type { Meta } from "@storybook/react-vite";
import { cleanup, render } from "@testing-library/react";
import { readdirSync } from "node:fs";
import type { ComponentType } from "react";
import { afterEach, describe, expect, test } from "vitest";

afterEach(cleanup);

const modules = import.meta.glob<{ default: Meta }>("./**/*.stories.tsx", { eager: true });

describe("the stories", () => {
  test("are there", () => {
    expect(Object.keys(modules).length).toBeGreaterThanOrEqual(3);
  });

  // Vitest keeps what it made of this file, the glob expanded, in node_modules/.vitest-cache; a story file added after that is not found
  // until the cache is cleared (`rm -rf node_modules/.vitest-cache`: `vitest --no-cache` does not do it, and a change to this file does), and its stories go unrendered without a word. The disk is asked.
  test("are all of the story files there are, not those a cached glob knew", () => {
    const onDisk = readdirSync(import.meta.dirname, { recursive: true, encoding: "utf8" })
      .map((file) => file.replaceAll("\\", "/"))
      .filter((file) => file.endsWith(".stories.tsx"))
      .map((file) => `./${file}`);
    expect(Object.keys(modules).sort()).toEqual(onDisk.sort());
  });

  for (const [path, module] of Object.entries(modules)) {
    const stories = composeStories(module as never) as Record<string, ComponentType>;
    describe(path, () => {
      for (const [name, Story] of Object.entries(stories)) {
        test(`${name} renders`, () => {
          const { container } = render(<Story />);
          expect(container.innerHTML).not.toBe("");
        });
      }
    });
  }
});
