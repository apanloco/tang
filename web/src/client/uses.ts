// When a parameter has an effect (step 95b): what a plugin says of it (`Param.usedWhen`: the Math's `bits` only in its logic modes, its carrier's `pitch` only while nothing is patched at its second
// input), read against the values the user has set and the cables of the session, for a control to be dimmed by. The model's `PluginSlot::unused_because` is the rule, written again here and pinned to
// it by `fixtures/uses.json`. A parameter that has no effect is dimmed and says why; it can still be turned, and keeps its value for when it has one.
import type { Graph, Node, NodeId, Param, UsedCondition } from "../protocol";

/** Whether a cable ends at the second input of the effect in node `id`: what the plugin makes of its own is what is heard there when none does. */
export function sideIsPatched(graph: Graph, id: NodeId): boolean {
  return graph.edges.some((edge) => edge.kind === "cable" && edge.to.node === id && edge.to.port.type === "audioInSide");
}

/**
 * Whether `condition` holds, with `valueOf` saying what the user set each parameter of the plugin to (nothing for one it has not: a rule that names no parameter is no rule, and holds) and `sidePatched`
 * whether a cable ends at the plugin's second input. A choice holds when its value rounds to one of its places, as Rust rounds (a half away from nothing: a place below the list is none of them, since the places are
 * whole numbers from 0 up).
 */
export function conditionHolds(condition: UsedCondition, valueOf: (index: number) => number | undefined, sidePatched: boolean): boolean {
  switch (condition.type) {
    case "choice": {
      const value = valueOf(condition.param);
      if (value === undefined) return true;
      const place = Math.sign(value) * Math.round(Math.abs(value));
      return condition.values.includes(place);
    }
    case "sideUnpatched":
      return !sidePatched;
  }
}

/**
 * Why `param` of the plugin in `node` has no effect now, in the plugin's words (`only while nothing is patched at B`), or null if it has one, or the plugin does not say when it has: the value that the
 * user set is the one that counts (a choice that a modulator moves is read where it is set).
 */
export function unusedBecause(graph: Graph, node: Node, param: Param): string | null {
  if (!param.usedWhen) return null;
  if (node.kind !== "instrument" && node.kind !== "effect") return null;
  const params = node.plugin?.params ?? [];
  const holds = conditionHolds(param.usedWhen.condition, (index) => params.find((p) => p.index === index)?.value, sideIsPatched(graph, node.id));
  return holds ? null : param.usedWhen.why;
}
