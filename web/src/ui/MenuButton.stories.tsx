import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";

import { MenuButton } from "./MenuButton";

const MODULATORS = [
  { id: "lfo", label: "LFO" },
  { id: "envelope", label: "Envelope" },
  { id: "midiLearn", label: "MIDI control" },
];

/** The button with what it chose written next to it, since a menu on its own does nothing to look at. */
function Demo({ items }: { items: typeof MODULATORS }) {
  const [chosen, setChosen] = useState<string | null>(null);
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 12, padding: 24 }}>
      <MenuButton label="Add a modulator" items={items} onChoose={setChosen}>
        +
      </MenuButton>
      <span>{chosen === null ? "nothing chosen" : `chose ${chosen}`}</span>
    </div>
  );
}

const meta = {
  title: "Controls/MenuButton",
  component: Demo,
} satisfies Meta<typeof Demo>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Modulators: Story = { args: { items: MODULATORS } };

/** With nothing to choose from there is nothing to open: the button is disabled. */
export const NothingToChoose: Story = { args: { items: [] } };
