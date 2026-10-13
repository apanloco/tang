import js from "@eslint/js";
import { defineConfig, globalIgnores } from "eslint/config";
import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";
import tseslint from "typescript-eslint";

export default defineConfig(
  // The protocol types are generated (docs/web-ui.md §7), and the rest is build output.
  globalIgnores(["dist", "storybook-static", "playwright-report", "test-results", "src/protocol/generated"]),

  js.configs.recommended,
  tseslint.configs.strictTypeChecked,
  tseslint.configs.stylisticTypeChecked,
  reactHooks.configs.flat["recommended-latest"],

  {
    languageOptions: {
      globals: globals.browser,
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
    rules: {
      // A number in a template is fine, and `${rev}` is how a message is written.
      "@typescript-eslint/restrict-template-expressions": ["error", { allowNumber: true }],
    },
  },

  // The app runs in a browser: no Node in it (the configs and the browser tests are another matter).
  {
    files: ["src/**"],
    rules: { "no-restricted-globals": ["error", "process", "Buffer", "require", "__dirname"] },
  },

  // The scripts and configs are plain JavaScript run by Node: no types to check against.
  {
    files: ["**/*.{js,mjs}"],
    extends: [tseslint.configs.disableTypeChecked],
    languageOptions: { globals: globals.node },
  },
);
