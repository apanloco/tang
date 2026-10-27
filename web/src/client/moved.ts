// What the program does to a session when it loads one, as far as its modulators go, for the mock engine's scenes.
import type { Node, Snapshot } from "../protocol";

type Modulator = Extract<Node, { kind: "modulator" }>;

/**
 * The session as the program has it once it has loaded it (`SessionConfig::move_modulators_to_the_session`, step 94): the modulators of the lanes and of the groups are the session's own, each hearing the lane or the group it
 * was in, so that what is heard is what was, and any of them can be drawn to any plugin. They are listed where the session's own modulators are, the lanes' first and then the groups', in front of the ones the session had; their
 * cables, which name what they drive by node, are as they were. The mock engine's scenes are such sessions, so that a page in front of the mock is what a page in front of the program is (the ids and the telemetry's
 * slots are the scene's own, and not those that a program makes). A session that has no modulator in a lane or a group is the same snapshot.
 */
export function moved(snapshot: Snapshot): Snapshot {
  const { nodes } = snapshot.graph;
  const kinds = new Map(nodes.map((node) => [node.id, node.kind]));
  const inRack = (node: Node): node is Modulator => node.kind === "modulator" && node.parent !== null && (kinds.get(node.parent) === "instrument" || kinds.get(node.parent) === "group");
  const racks = nodes.filter(inRack);
  if (racks.length === 0) return snapshot;
  const own = (node: Modulator): Modulator => (node.parent === null ? node : { ...node, parent: null, hears: node.parent });
  // (A modulator that is in a rack is not among them, and the ones that are not are modulators of the session's own, or any other kind of node.)
  const rest: Node[] = nodes.filter((node) => !inRack(node));
  const first = rest.findIndex((node) => node.kind === "modulator");
  const master = rest.findIndex((node) => node.kind === "master");
  const at = first >= 0 ? first : master >= 0 ? master : rest.length;
  const ordered = [...racks.filter((node) => kinds.get(node.parent ?? -1) === "instrument"), ...racks.filter((node) => kinds.get(node.parent ?? -1) === "group")];
  return { ...snapshot, graph: { ...snapshot.graph, nodes: [...rest.slice(0, at), ...ordered.map(own), ...rest.slice(at)] } };
}
