import type { Meta, StoryObj } from "@storybook/react-vite";

// Every colour token, as it looks (the values are in tokens.css; this only names them).
const COLOURS: [name: string, means: string][] = [
  ["--tang-bg", "the page"],
  ["--tang-surface", "a panel"],
  ["--tang-surface-raised", "something on a panel"],
  ["--tang-border", "lines"],
  ["--tang-text", "text"],
  ["--tang-text-muted", "quiet text"],
  ["--tang-held", "a key that is down"],
  ["--tang-root", "the root of the scale"],
  ["--tang-scale", "a note of the scale"],
  ["--tang-value", "what a parameter is set to"],
  ["--tang-modulation", "modulators and what they drive"],
  ["--tang-group", "groups and their buses"],
  ["--tang-instrument", "an instrument"],
  ["--tang-effect", "an effect"],
  ["--tang-ok", "all is well"],
  ["--tang-warn", "something is under way"],
  ["--tang-danger", "something is wrong"],
];

function Swatches() {
  return (
    <ul style={{ listStyle: "none", margin: 0, padding: 0, display: "grid", gap: 8, gridTemplateColumns: "repeat(auto-fill, minmax(260px, 1fr))" }}>
      {COLOURS.map(([name, means]) => (
        <li key={name} style={{ display: "flex", alignItems: "center", gap: 12 }}>
          <span style={{ width: 40, height: 40, borderRadius: "var(--tang-radius)", border: "1px solid var(--tang-border)", background: `var(${name})` }} />
          <span>
            <code style={{ fontFamily: "var(--tang-mono)" }}>{name}</code>
            <br />
            <span style={{ color: "var(--tang-text-muted)", fontSize: "var(--tang-text-small)" }}>{means}</span>
          </span>
        </li>
      ))}
    </ul>
  );
}

const meta = {
  title: "Design/Colours",
  component: Swatches,
} satisfies Meta<typeof Swatches>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Palette: Story = {};
