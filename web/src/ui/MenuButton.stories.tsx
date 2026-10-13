import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";

import { MenuButton } from "./MenuButton";

const MODULATORS = [
  { id: "lfo", label: "LFO" },
  { id: "envelope", label: "Envelope" },
  { id: "midiLearn", label: "MIDI control" },
];

/** The parameters of two plugins, as the list of a cable has them: under a heading for each. */
const PARAMETERS = [
  ...["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan"].map((name) => ({ id: `1:${name}`, label: name, group: "Oscillator" })),
  ...["resonance", "type", "drive"].map((name) => ({ id: `2:${name}`, label: name, group: "Filter" })),
];

/** A plugin with a hundred parameters: the list scrolls (and a letter goes to a parameter). */
const MANY = Array.from({ length: 100 }, (_, index) => ({ id: `p${String(index)}`, label: `parameter ${String(index)}`, group: `Section ${String(Math.floor(index / 10) + 1)}` }));

interface DemoProps {
  items: readonly { id: string; label: string; group?: string }[];
  /** The button says what the list is for, as the list of a cable does, and not just "+". */
  field?: boolean;
}

/** The button with what it chose written next to it, since a menu on its own does nothing to look at. */
function Demo({ items, field = false }: DemoProps) {
  const [chosen, setChosen] = useState<string | null>(null);
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 12, padding: 24 }}>
      <MenuButton label="Add a modulator" items={items} onChoose={setChosen}>
        {field ? "Add a cable…" : "+"}
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

/** Items in groups, under headings: the parameters a cable can be drawn to. */
export const Grouped: Story = { args: { items: PARAMETERS, field: true } };

/** A long list is as tall as it may be and scrolls; the arrow keys, Home, End and the letters find an item. */
export const Long: Story = { args: { items: MANY, field: true } };
