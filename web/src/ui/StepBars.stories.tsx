import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";

import { StepBars } from "./StepBars";
import type { StepBarsProps } from "./StepBars";

const STAIRCASE = Array.from({ length: 16 }, (_, step) => step / 8 - 1);

/** Where a step sequencer of `length` steps in use is when it goes four steps a second: the step as the whole part, how far into it as the fraction. */
const going =
  (length: number): (() => number | undefined) =>
  () =>
    ((performance.now() / 1000) * 4) % length;

/** The bars as the app holds them: the values are the story's, and what a stroke paints stays. */
function Painted(props: StepBarsProps) {
  const [values, setValues] = useState(props.values);
  return <StepBars {...props} values={values} onChange={props.onChange ? setValues : undefined} />;
}

const meta = {
  title: "Controls/StepBars",
  component: StepBars,
  args: { label: "Steps of Steps", values: STAIRCASE, length: 16, onChange: () => undefined, read: going(16) },
  render: (args) => <Painted {...args} />,
  decorators: [
    (Story) => (
      <div style={{ width: 280 }}>
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof StepBars>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A new step sequencer: a rising staircase, all sixteen steps in use, with the playhead going through them. Drag across the bars to paint, or use the keys. */
export const Staircase: Story = {};

/** Five steps in use: the others keep their values and are dimmed, and the playhead goes round at five. */
export const Short: Story = { args: { length: 5, read: going(5) } };

/** A pattern with the values on both sides of the middle line. */
export const Pattern: Story = {
  args: { values: [1, 0, 0.5, -0.5, 1, 0, -1, 0.25, 0.75, -0.25, 0, 0, 0.5, -0.5, 1, -1], length: 8, read: going(8) },
};

/** Nothing to follow: the bars, and no playhead. */
export const NoPlayhead: Story = { render: ({ label, values, length, onChange }) => <Painted label={label} values={values} length={length} onChange={onChange} /> };

/** The bars only show: no stroke paints them. */
export const ReadOnly: Story = { render: ({ label, values, length, read }) => <Painted label={label} values={values} length={length} read={read} /> };
