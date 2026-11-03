import type { StorybookConfig } from "@storybook/react-vite";

// The components of the interface, one at a time and in every state they have, on the
// mock engine where they need a server (docs/web-ui.md §9.2).
const config: StorybookConfig = {
  stories: ["../src/**/*.stories.tsx"],
  framework: "@storybook/react-vite",
  // Nothing about how the components are used leaves this machine, or the CI runner.
  core: { disableTelemetry: true },
};

export default config;
