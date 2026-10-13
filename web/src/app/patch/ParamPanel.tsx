// The parameters of one plugin, all of them (docs/web-ui.md §9.3): a node has room for a few knobs, and a plugin
// can have thousands of parameters, so the rest are here, as rows with their whole names and a filter to find one by.
// A row is turned like a knob, and the two agree: both are the same parameter of the session.
import { useEffect, useMemo, useRef, useState } from "react";

import { matchParams } from "../../client/paramSearch";
import { paramListOf, sameParamList } from "../../client/selectors";
import type { NodeId } from "../../protocol";
import { useClientSelection } from "../ClientProvider";
import { ParamRow } from "./controls";
import "./ParamPanel.css";

/** How many rows are drawn at a time: a plugin can have thousands, and a person looking through them is looking for one. */
const PAGE = 40;

/**
 * The panel of the plugin in `node`: a filter by name, how many parameters there are, a page of rows, and a button for the
 * next page. `onClose` is asked for when it is closed by the person (its button, Escape) and when the node is gone.
 */
export function ParamPanel({ node, onClose }: { node: NodeId; onClose: (focusBack: boolean) => void }) {
  const list = useClientSelection((state) => paramListOf(state, node), sameParamList);
  const [query, setQuery] = useState("");
  const [limit, setLimit] = useState(PAGE);
  const search = useRef<HTMLInputElement>(null);
  const matches = useMemo(() => (list ? matchParams(list.params, query) : []), [list, query]);

  // (When it is opened, it is for typing in.)
  useEffect(() => {
    search.current?.focus();
  }, []);
  // (A node that is gone, or has no plugin any longer, has nothing to show, and nowhere to give the focus back to.)
  useEffect(() => {
    if (!list) onClose(false);
  }, [list, onClose]);
  if (!list) return null;

  const total = list.params.length;
  const filtered = query.trim() !== "";
  const shown = matches.slice(0, limit);
  const left = matches.length - shown.length;
  return (
    <aside
      className="tang-panel"
      aria-label={`Parameters of ${list.name}`}
      onKeyDown={(event) => {
        if (event.key === "Escape") onClose(true);
      }}
    >
      <header className="tang-panel__header">
        <h2 className="tang-panel__title" title={list.name}>
          {list.name}
        </h2>
        <span className="tang-node__badge">{list.format}</span>
        <button
          type="button"
          className="tang-menu-button"
          aria-label={`Close the parameters of ${list.name}`}
          title="Close"
          onClick={() => {
            onClose(true);
          }}
        >
          ×
        </button>
      </header>
      <input
        ref={search}
        type="search"
        className="tang-panel__search"
        aria-label={`Filter the parameters of ${list.name}`}
        placeholder="Filter by name"
        value={query}
        onChange={(event) => {
          setQuery(event.target.value);
          setLimit(PAGE);
        }}
      />
      <p className="tang-panel__count">
        {filtered ? `${String(matches.length)} of ${String(total)} parameters` : `${String(total)} ${total === 1 ? "parameter" : "parameters"}`}
      </p>
      <div className="tang-panel__rows">
        {shown.map((index) => (
          <ParamRow key={index} node={node} index={index} />
        ))}
        {filtered && matches.length === 0 ? <p className="tang-panel__none">{`No parameter has "${query.trim()}" in its name`}</p> : null}
        {left > 0 ? (
          <button
            type="button"
            className="tang-panel__more"
            onClick={() => {
              setLimit(limit + PAGE);
            }}
          >
            {`Show more (${String(left)} not shown)`}
          </button>
        ) : null}
      </div>
    </aside>
  );
}
