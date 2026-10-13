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
}

/** A parameter that is one of a few named values (a waveform, a filter's type). */
export function Choice({ label, labels, value, onChange, disabled = false }: ChoiceProps) {
  return (
    <label className="tang-choice">
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
