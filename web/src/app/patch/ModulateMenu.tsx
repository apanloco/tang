// The button on a control that modulates what the control sets (the TUI's `m` and `l`): a menu of a new LFO, a new envelope, a new macro and a new MIDI control
// that learns, made for the parameter or setting with a cable to it in one change (`Modulate`), and of the modulators that are there and could drive it, which
// get a cable (`AddModEdge`). What could drive it is looked up when the menu is opened, not when anything changes: there is a button on every knob.
import { useMemo, useState } from "react";

import { driversOf } from "../../client/reach";
import { displayNames } from "../../client/names";
import type { ModulatorKind, NodeId, Port } from "../../protocol";
import { MenuButton } from "../../ui/MenuButton";
import type { MenuItem } from "../../ui/MenuButton";
import { useClient, useEditor } from "../ClientProvider";

/** The kinds of modulator that can be made for a control, as the menu says them. */
const NEW: readonly { kind: ModulatorKind; label: string }[] = [
  { kind: "lfo", label: "New LFO" },
  { kind: "envelope", label: "New envelope" },
  { kind: "macro", label: "New macro" },
  { kind: "midiLearn", label: "New MIDI control" },
];

/** A modulator that is there and could drive the control, by the name the canvas gives it. */
interface Driver {
  node: NodeId;
  name: string;
}

/**
 * The button that modulates the port `port` of the node `node`: a parameter of its plugin, a setting of a modulator or the depth of one of its cables.
 * `name` is what the control is called, for the button. A new modulator goes in the rack that reaches the port, the lane's for a lane's plugin and the
 * rack of the modulator for a setting of it; a MIDI control is armed, and learns the next controller that is moved. One that is there gets a cable.
 */
export function ModulateMenu({ node, port, name, className }: { node: NodeId; port: Port; name: string; className?: string }) {
  const client = useClient();
  const editor = useEditor();
  const [drivers, setDrivers] = useState<readonly Driver[]>([]);
  // What each item is: a kind to make a modulator of, or the modulator to draw a cable from.
  const { items, choices } = useMemo(() => {
    const items: MenuItem[] = NEW.map(({ kind, label }) => ({ id: `new:${kind}`, label, group: "New" }));
    const choices = new Map<string, { kind: ModulatorKind } | { modulator: NodeId }>(NEW.map(({ kind }) => [`new:${kind}`, { kind }]));
    for (const driver of drivers) {
      items.push({ id: `attach:${String(driver.node)}`, label: driver.name, group: "Attach" });
      choices.set(`attach:${String(driver.node)}`, { modulator: driver.node });
    }
    return { items, choices };
  }, [drivers]);
  return (
    <MenuButton
      label={`Modulate ${name}`}
      className={className === undefined ? "tang-modulate" : `tang-modulate ${className}`}
      items={items}
      onOpen={() => {
        const graph = client.state.snapshot?.graph;
        const names = graph ? displayNames(graph) : undefined;
        setDrivers(graph ? driversOf(graph, node, port).map((driver) => ({ node: driver.id, name: names?.get(driver.id) ?? driver.name })) : []);
      }}
      onChoose={(choice) => {
        const made = choices.get(choice);
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        if (made && "kind" in made) void editor.modulate(node, port, made.kind);
        else if (made) void editor.addCable(made.modulator, node, port);
      }}
    >
      ~
    </MenuButton>
  );
}
