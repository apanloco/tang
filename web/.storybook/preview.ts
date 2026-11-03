import type { Preview } from "@storybook/react-vite";

import "../src/ui/global.css";
import "../src/ui/tokens.css";

const preview: Preview = {
  parameters: {
    // The page's own background, from the tokens (a story on white would be a lie).
    backgrounds: { disabled: true },
    layout: "padded",
  },
};

export default preview;
