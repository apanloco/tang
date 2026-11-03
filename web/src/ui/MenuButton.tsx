import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import { createPortal } from "react-dom";

import "./MenuButton.css";

export interface MenuItem<Id extends string = string> {
  id: Id;
  label: string;
  /** The heading it is listed under: a run of items that have the same one is a group of the menu, drawn under it. */
  group?: string;
  /** It is the choice now (the preset a plugin has): marked, and where the menu opens when the button is pressed. */
  current?: boolean;
}

export interface MenuButtonProps<Id extends string = string> {
  /** What pressing the button does, for a screen reader (and a tooltip): the button itself is a glyph. */
  label: string;
  /** The `id` of the button, for whatever has to give the focus back to it. */
  id?: string;
  /** More classes for the button, for a page that draws it as something wider than a glyph. */
  className?: string;
  items: readonly MenuItem<Id>[];
  /** The id of the item that was chosen. */
  onChoose: (id: Id) => void;
  /**
   * Told each time the menu is opened (by the button, or by an arrow on it), as it is drawn: for a page whose items are looked up when a person asks
   * for them and not each time anything changes, as a button on every knob of a patch would have to.
   */
  onOpen?: () => void;
  /** What the button shows. */
  children: ReactNode;
}

/** About how tall an item is, to tell whether the menu fits under the button before it has been drawn. */
const ITEM_HEIGHT = 30;
/** About how tall the heading of a group is. */
const HEADING_HEIGHT = 24;
/** The most the menu grows to: a longer one scrolls. */
const MAX_HEIGHT = 360;
/** How long a pause ends the text that was typed to find an item. */
const TYPE_AHEAD_MS = 700;

interface Place {
  left: number;
  top: number;
  above: boolean;
}

/** A run of items that are under one heading (or have none). */
interface Section<Id extends string> {
  group: string | undefined;
  entries: { item: MenuItem<Id>; index: number }[];
}

/** The items in runs of one group: one that comes up again after another is a run of its own. */
function sectionsOf<Id extends string>(items: readonly MenuItem<Id>[]): Section<Id>[] {
  const sections: Section<Id>[] = [];
  items.forEach((item, index) => {
    const last = sections.at(-1);
    if (last !== undefined && last.group === item.group) last.entries.push({ item, index });
    else sections.push({ group: item.group, entries: [{ item, index }] });
  });
  return sections;
}

/**
 * A button that opens a menu: the pattern of the WAI-ARIA authoring practices. The menu is drawn in the page and
 * not where the button is (the button is in a node of the canvas, which would put the menu under the next node
 * and move it with the canvas), so it closes when the page is scrolled, sized, or pressed anywhere else. The
 * arrow keys, Home and End move between its items, a letter (or the first few of a name, typed together) goes to
 * the item that starts with it, Enter or Space chooses, Escape leaves it for the button and Tab leaves it for what
 * comes after the button (or before it, with Shift). Items can be in groups, under headings, and the list scrolls
 * when it is long; what is on it can change while it is open (the focus stays on its item, or goes to the first).
 */
export function MenuButton<Id extends string>({ label, id, className, items, onChoose, onOpen, children }: MenuButtonProps<Id>) {
  const [place, setPlace] = useState<Place | null>(null);
  // The item that has the focus is kept by its id, not by its place, which moves when what comes before it goes.
  const [focusedId, setFocusedId] = useState<Id | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const typed = useRef({ text: "", at: 0 });
  const menuId = useId();
  const open = place !== null;
  const sections = useMemo(() => sectionsOf(items), [items]);
  const focused = Math.max(
    0,
    items.findIndex((item) => item.id === focusedId),
  );
  const focusedKey = items[focused]?.id;

  const openAt = (index: number): void => {
    const box = trigger.current?.getBoundingClientRect();
    if (!box) return;
    onOpen?.();
    const headings = sections.filter((section) => section.group !== undefined).length;
    const needed = Math.min(items.length * ITEM_HEIGHT + headings * HEADING_HEIGHT + 8, MAX_HEIGHT);
    const above = box.bottom + needed > window.innerHeight && box.top - needed > 0;
    setPlace({ left: box.right, top: above ? box.top - 2 : box.bottom + 2, above });
    setFocusedId(items[index]?.id ?? null);
  };

  const close = (refocus: boolean): void => {
    setPlace(null);
    if (refocus) trigger.current?.focus();
  };

  // The focus follows the item that has it (and goes to the one that is first when the one that had it is gone).
  useEffect(() => {
    if (open) itemRefs.current[focused]?.focus();
  }, [open, focused, focusedKey]);

  // There is nothing to choose from any more: the menu goes, and does not come back when there are items again.
  if (items.length === 0 && place !== null) setPlace(null);

  // A press anywhere else, the page being scrolled or sized: the menu goes (and the focus stays where the person put it).
  useEffect(() => {
    if (!open) return;
    const away = (event: Event): void => {
      const target = event.target;
      if (target instanceof Node && (menu.current?.contains(target) || trigger.current?.contains(target))) return;
      setPlace(null);
    };
    const leave = (): void => {
      setPlace(null);
    };
    document.addEventListener("pointerdown", away, true);
    window.addEventListener("wheel", leave, true);
    window.addEventListener("resize", leave);
    return () => {
      document.removeEventListener("pointerdown", away, true);
      window.removeEventListener("wheel", leave, true);
      window.removeEventListener("resize", leave);
    };
  }, [open]);

  const onTriggerKey = (event: KeyboardEvent<HTMLButtonElement>): void => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      openAt(event.key === "ArrowDown" ? 0 : items.length - 1);
    }
  };

  /** A letter was typed: the focus goes to an item that starts with the text typed lately. */
  const typeAhead = (key: string): void => {
    const now = Date.now();
    const text = (now - typed.current.at < TYPE_AHEAD_MS ? typed.current.text : "") + key.toLowerCase();
    typed.current = { text, at: now };
    // One letter, or the same one over and over, goes to the next item that starts with it; more go to the first that
    // starts with all of them, the one that has the focus included.
    const search = /^(.)\1*$/s.test(text) ? text.slice(0, 1) : text;
    const from = search.length === 1 ? focused + 1 : focused;
    for (let step = 0; step < items.length; step += 1) {
      const index = (from + step) % items.length;
      const found = items[index];
      if (found?.label.toLowerCase().startsWith(search)) {
        setFocusedId(found.id);
        return;
      }
    }
  };

  const onMenuKey = (event: KeyboardEvent<HTMLDivElement>): void => {
    const last = items.length - 1;
    const moves: Record<string, number> = { ArrowDown: focused >= last ? 0 : focused + 1, ArrowUp: focused <= 0 ? last : focused - 1, Home: 0, End: last };
    const move = moves[event.key];
    if (move !== undefined) {
      event.preventDefault();
      event.stopPropagation();
      setFocusedId(items[move]?.id ?? null);
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close(true);
    } else if (event.key === "Tab") {
      // The menu is drawn at the end of the page, so a Tab from it would go to whatever comes after the page's
      // last control: the focus goes back to the button first, and the Tab that comes after goes on from there.
      close(true);
    } else if (event.key.length === 1 && event.key !== " " && !event.ctrlKey && !event.metaKey && !event.altKey) {
      // (Not heard by the page, whose keys play notes: the letters are the menu's while it is open.)
      event.preventDefault();
      event.stopPropagation();
      typeAhead(event.key);
    }
  };

  return (
    <>
      <button
        ref={trigger}
        id={id}
        type="button"
        className={className === undefined ? "tang-menu-button nodrag nopan" : `tang-menu-button nodrag nopan ${className}`}
        aria-label={label}
        title={label}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        disabled={items.length === 0}
        onClick={() => {
          if (open) close(false);
          else openAt(Math.max(0, items.findIndex((item) => item.current === true)));
        }}
        onKeyDown={onTriggerKey}
      >
        {children}
      </button>
      {place
        ? createPortal(
            <div
              ref={menu}
              id={menuId}
              role="menu"
              aria-label={label}
              className="tang-menu"
              style={{
                position: "fixed",
                left: `${String(place.left)}px`,
                top: `${String(place.top)}px`,
                transform: place.above ? "translate(-100%, -100%)" : "translateX(-100%)",
                maxHeight: `${String(MAX_HEIGHT)}px`,
                overflowY: "auto",
              }}
              onKeyDown={onMenuKey}
            >
              {sections.map((section) => {
                const entries = section.entries.map(({ item, index }) => (
                  <button
                    key={item.id}
                    ref={(element) => {
                      itemRefs.current[index] = element;
                    }}
                    type="button"
                    role="menuitem"
                    className="tang-menu__item"
                    aria-current={item.current === true ? "true" : undefined}
                    tabIndex={index === focused ? 0 : -1}
                    onClick={() => {
                      onChoose(item.id);
                      close(true);
                    }}
                  >
                    {item.label}
                  </button>
                ));
                const first = section.entries[0]?.item.id;
                return section.group === undefined ? (
                  entries
                ) : (
                  <div key={first} role="group" aria-label={section.group} className="tang-menu__group">
                    <div className="tang-menu__heading" aria-hidden="true">
                      {section.group}
                    </div>
                    {entries}
                  </div>
                );
              })}
            </div>,
            document.body,
          )
        : null}
    </>
  );
}
