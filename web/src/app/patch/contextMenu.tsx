// The right-click menus of the canvas (step 107): one menu at a time, opened by whatever was right-clicked (the canvas, a node, a cable, what is chosen) with the items that are of it. The page that has the menus puts the host
// round them; whatever is in it opens a menu with `useContextMenu()`, given the event and the items, and the host draws it at the pointer. What the items are is `contextMenus.ts`'s business, and how a menu looks and answers keys is
// `ui/ContextMenu`'s.
import { createContext, useCallback, useContext, useRef, useState } from "react";
import type { ReactNode } from "react";

import { ContextMenu } from "../../ui/ContextMenu";
import type { ContextItem } from "../../ui/ContextMenu";

/** What a menu is: what it is of, for a screen reader, and its items. */
export interface MenuSpec {
  label: string;
  items: readonly ContextItem[];
}

/** The event that asks for a menu: where the pointer was, and what is done to it so that the browser's own menu does not open too. */
export interface MenuEvent {
  clientX: number;
  clientY: number;
  preventDefault: () => void;
  stopPropagation: () => void;
}

/** Opens the menu `spec` at the pointer of `event`, whichever was open (a menu with no item is no menu: the browser's own stays away all the same, and nothing opens). */
export type OpenMenu = (event: MenuEvent, spec: MenuSpec) => void;

const OpenMenuContext = createContext<OpenMenu>(() => undefined);

/** The function that opens a menu, for whatever is drawn in a `ContextMenuHost`. */
export const useContextMenu = (): OpenMenu => useContext(OpenMenuContext);

/** The menu that is open: where, and what it is. */
interface Shown {
  at: { x: number; y: number };
  spec: MenuSpec;
  /** A number of its own, so that a menu that is opened while another is open is a new one: nothing of the one before is kept (the menus that were open, the item that had the focus). */
  id: number;
}

/** Draws the menu that is open, and lets what is inside open one. */
export function ContextMenuHost({ children }: { children: ReactNode }) {
  const [shown, setShown] = useState<Shown | null>(null);
  const made = useRef(0);
  const open = useCallback<OpenMenu>((event, spec) => {
    event.preventDefault();
    event.stopPropagation();
    if (spec.items.length === 0) {
      setShown(null);
      return;
    }
    made.current += 1;
    setShown({ at: { x: event.clientX, y: event.clientY }, spec, id: made.current });
  }, []);
  const close = useCallback(() => {
    setShown(null);
  }, []);
  return (
    <OpenMenuContext.Provider value={open}>
      {children}
      {shown ? <ContextMenu key={shown.id} label={shown.spec.label} at={shown.at} items={shown.spec.items} onClose={close} /> : null}
    </OpenMenuContext.Provider>
  );
}
