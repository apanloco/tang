import "./Choice.css";

export interface ChoiceProps {
  label: string;
  /** The names of the values: the value of the parameter is an index into them. */
  labels: readonly string[];
  /** The index of the chosen one (a value that is not whole is taken as the nearest whole). */
  value: number;
  /** The index of the one that was chosen; without it the choice only shows. */
  onChange?: (index: number) => void;
  disabled?: boolean;
  /** A choice that sits in a line with other things (the header of a node), where there is no room for its label over it: the label is not drawn, and is still its name. */
  inline?: boolean;
  /** What the pointer says about it, for a choice whose name is not enough. */
  title?: string;
}

/** A parameter that is one of a few named values (a waveform, a filter's type). */
export function Choice({ label, labels, value, onChange, disabled = false, inline = false, title }: ChoiceProps) {
  return (
    <label className={inline ? "tang-choice tang-choice--inline" : "tang-choice"} title={title}>
      <span className="tang-choice__label">{label}</span>
      <select
        className="tang-choice__select"
        value={String(Math.round(value))}
        disabled={disabled || !onChange}
        onChange={(event) => {
          onChange?.(Number(event.target.value));
        }}
      >
        {labels.map((name, index) => (
          <option key={`${String(index)}:${name}`} value={String(index)}>
            {name}
          </option>
        ))}
      </select>
    </label>
  );
}
