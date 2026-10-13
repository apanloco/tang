// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, test } from "vitest";

import { App } from "./App";

afterEach(cleanup);

test("the app says what it is", () => {
  render(<App />);
  expect(screen.getByRole("heading", { name: "Tang" })).toBeTruthy();
});
