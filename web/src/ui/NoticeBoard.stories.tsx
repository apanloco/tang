import type { Meta, StoryObj } from "@storybook/react-vite";

import { Notices } from "../client/notices";
import { NoticeBoard } from "./NoticeBoard";

const said = (...texts: string[]): Notices => {
  const notices = new Notices();
  for (const text of texts) notices.say(text);
  return notices;
};

const meta = {
  title: "Page/NoticeBoard",
  component: NoticeBoard,
} satisfies Meta<typeof NoticeBoard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Nothing: Story = { args: { notices: new Notices() } };

export const OneRefusal: Story = {
  args: { notices: said("Could not add an LFO to Filter: a modulator goes in a lane or in a group") },
};

export const SeveralRefusals: Story = {
  args: {
    notices: said(
      "Could not draw a cable from LFO to Filter: cutoff: that modulator drives that parameter already",
      "Could not remove Oscillator: only a modulator can be removed so far",
      "Could not take out that cable: there is no cable 99",
    ),
  },
};

export const TheSameOneAgainAndAgain: Story = {
  args: {
    notices: (() => {
      const notices = new Notices();
      for (let i = 0; i < 40; i += 1) notices.say("Could not set a parameter: the plugin has no parameter 99");
      return notices;
    })(),
  },
};
