// The note splitter on the canvas (docs/patch-graph.md §11.6, P4i-C; the user's "I want a midi-box that takes keyboard in, finds the lowest or highest note (configurable) and only sends that forward. Imagine I play a chord and I want
// the bass note sent to one synth, and the rest to another? Maybe we can have 2 outputs from it, the bass and the rest?"): a node that takes notes in at its port on the left and has two outputs of notes on the right. The first sends the
// lowest (or the highest) of the keys that are held, the second sends every other key; a key is judged when it is pressed and stays on the output it began on until it is let go. Which note the first takes is a switch in the node.
//
// What the splitter picks is read from the store here, and not from the node's data: the data is the structure of the session, and the node draws the switch from what the session says at the moment.
import { memo, useCallback, useState } from "react";
import type { NodeProps } from "@xyflow/react";

import { splitterPickOf } from "../../client/selectors";
import type { NodeId, SplitterPick } from "../../protocol";
import { useClientState, useEditor } from "../ClientProvider";
import { focusWhenPossible } from "../focus";
import { NotesPort } from "./controls";
import { Header, NameField, RemoveNodeButton, RenameButton } from "./nodes";
import { NotesFromMenu, NotesToMenu } from "./NotesMenus";
import type { PatchNode } from "./toFlow";

/** The `id` of the button that turns the name of the splitter `node` into a field. */
const renameSplitterId = (node: NodeId): string => `rename-splitter-${String(node)}`;

/** The two notes that a splitter can pick, as the switch has them: what it says, and what it means to a person who has not used it. */
const PICKS: readonly { pick: SplitterPick; text: string; title: string }[] = [
  { pick: "lowest", text: "Lowest", title: "The lowest key that is held goes to the first output (the bass of a chord), and every other key to the second" },
  { pick: "highest", text: "Highest", title: "The highest key that is held goes to the first output (the top of a chord), and every other key to the second" },
];

/** What the first output of a splitter that picks `pick` is called: the note it sends. */
const firstOutput = (pick: SplitterPick): string => (pick === "highest" ? "Highest" : "Lowest");

/** What the first output of a splitter that picks `pick` sends, in the words of the label of its menu (`Send the lowest note from Bass line`). */
const firstOutputSends = (pick: SplitterPick): string => (pick === "highest" ? "the highest note" : "the lowest note");

/** The switch of what the splitter picks, and its two outputs, each with the menu that sends its notes to a lane and the port that a cable is dragged from. */
export const SplitterBody = memo(function SplitterBody({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const pick = useClientState((state) => splitterPickOf(state, node));
  // (A node that is no splitter has nothing to draw.)
  if (pick === undefined) return null;
  return (
    <div className="tang-splitter nodrag nopan nowheel" role="group" aria-label={`Note splitter ${name}`}>
      <div className="tang-splitter__row">
        <span className="tang-splitter__label">Picks</span>
        {PICKS.map(({ pick: option, text, title }) => (
          <button
            key={option}
            type="button"
            className="tang-pattern__button tang-pattern__switch"
            aria-label={`Pick the ${option} note with ${name}`}
            aria-pressed={pick === option}
            title={title}
            onClick={() => {
              // (If it is refused the page says so: `Editor` puts the reason among the notices.)
              void editor.setSplitterField(node, { type: "pick", pick: option });
            }}
          >
            {text}
          </button>
        ))}
      </div>
      <div className="tang-splitter__row tang-splitter__out" data-output="pick">
        <span className="tang-splitter__label" title="What the first output sends: the note that the splitter picks">
          {firstOutput(pick)}
        </span>
        <NotesToMenu node={node} name={name} glyph="♫▸" output="pick" what={firstOutputSends(pick)} />
        <NotesPort node={node} direction="out" output="pick" />
      </div>
      <div className="tang-splitter__row tang-splitter__out" data-output="rest">
        <span className="tang-splitter__label" title="What the second output sends: every key that is not the pick">
          Rest
        </span>
        <NotesToMenu node={node} name={name} glyph="♫▸" output="rest" what="the other notes" />
        <NotesPort node={node} direction="out" output="rest" />
      </div>
    </div>
  );
});

/**
 * A note splitter: notes come in at its port on the left, and go out at the two on the right, the note it picks at the first and the others at the second. The menu of its name takes notes in from a source or a box (▸♫), for a keyboard,
 * which cannot drag a cable, and each output has the menu that sends its notes on to a lane (♫▸); its name is changed in place, and its button takes it out with the cables of notes it has.
 */
export function SplitterNode({ data }: NodeProps<PatchNode>) {
  const editor = useEditor();
  const [editing, setEditing] = useState(false);
  const rename = useCallback((name: string) => editor.setSplitterField(data.node, { type: "name", name }), [editor, data.node]);
  return (
    <div className="tang-node tang-node--splitter nokey">
      <NotesPort node={data.node} direction="in" />
      <Header
        name={data.name}
        badge="Note splitter"
        sub={<NotesFromMenu node={data.node} name={data.name} glyph="▸♫" />}
        field={
          editing ? (
            <NameField
              name={data.name}
              rename={rename}
              done={(focusBack) => {
                setEditing(false);
                if (focusBack) focusWhenPossible(renameSplitterId(data.node));
              }}
            />
          ) : undefined
        }
      >
        <span className="tang-node__actions">
          <RenameButton
            id={renameSplitterId(data.node)}
            name={data.name}
            onEdit={() => {
              setEditing(true);
            }}
          />
          <RemoveNodeButton node={data.node} name={data.name} focusTo={null} />
        </span>
      </Header>
      <SplitterBody node={data.node} name={data.name} />
    </div>
  );
}
