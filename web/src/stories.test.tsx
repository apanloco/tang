// @vitest-environment jsdom
// Every story of every component renders: a story that throws is found here, in the
// ordinary tests, and not when somebody opens Storybook.
import { composeStories } from "@storybook/react-vite";
import type { Meta } from "@storybook/react-vite";
import { cleanup, render } from "@testing-library/react";
import type { ComponentType } from "react";
import { afterEach, describe, expect, test } from "vitest";

afterEach(cleanup);

const modules = import.meta.glob<{ default: Meta }>("./**/*.stories.tsx", { eager: true });

describe("the stories", () => {
  test("are there", () => {
    expect(Object.keys(modules).length).toBeGreaterThanOrEqual(3);
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
