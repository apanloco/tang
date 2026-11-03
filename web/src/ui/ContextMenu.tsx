// A context menu (step 107): the menu a right click opens, at the pointer, with items that do something when they are chosen and items that open a menu of their own beside them. It is the pattern of the WAI-ARIA authoring
// practices, as `MenuButton` is, and looks as it does (the same classes), but it has no button: whoever opens it knows where the pointer was and what the menu is of, and hands it the items, which do their own work (`run`) when
// they are chosen. The menu is drawn in the page, over the canvas, and goes when the page is pressed anywhere else, scrolled outside it or sized; the keys of the page, which play notes, are not heard while it is open.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { createPortal } from "react-dom";

import type { MenuItem } from "./MenuButton";
import "./MenuButton.css";
import "./ContextMenu.css";

/** One entry of a context menu: a label and, if it is not a heading's, what it does or the menu it opens. */
export interface ContextItem extends MenuItem {
  /** What it does when it is chosen (an item that opens a menu has none). */
  run?: () => void;
  /** The menu that opens beside it, in place of `run`. */
  items?: readonly ContextItem[];
  /** It is there and cannot be chosen (greyed out); `title` says why. */
  disabled?: boolean;
  /** What it does, or why it cannot be chosen, for the pointer to find out. */
  title?: string | undefined;
  /** A switch, and it is on: a check at its left (`menuitemcheckbox`). */
  checked?: boolean;
  /** It takes something away: drawn in the colour of what does. */
  danger?: boolean;
  /** A rule is drawn above it, to set it apart from what comes before (a heading does it too). */
  rule?: boolean;
}

export interface ContextMenuProps {
  /** What the menu is of, for a screen reader (`Oscillator`, `the canvas`). */
  label: string;
  /** Where the pointer was, in the pixels of the window: the menu opens there, and moves to stay in it. */
  at: { x: number; y: number };
  items: readonly ContextItem[];
  /** The menu is done with: an item was chosen, or the person went elsewhere or pressed Escape. */
  onClose: () => void;
}

/** How long the pointer rests on an item that has a menu before the menu opens (and one that is open gives way): long enough for the pointer to go across the items that are on its way to the menu. */
const HOVER_MS = 140;
/** How long a pause ends the text that was typed to find an item. */
const TYPE_AHEAD_MS = 700;
/** The room left between the menu and the edge of the window. */
const EDGE = 4;
/** The key of an item among the open menus: the ids of the items it is under and its own, which this joins. */
const SEP = "\u001f";

/** A menu of the levels that are open: its items, and the key of the item that opened it (`null` for the first). */
interface Level {
  items: readonly ContextItem[];
  /** The ids of the items that opened this menu and the menus above it, the first first. */
  path: readonly string[];
}

/** A run of items under one heading (or none): a section of the menu. */
interface Section {
  group: string | undefined;
  entries: { item: ContextItem; index: number }[];
}

/** The items in sections: a new one begins where the heading changes and at an item that asks for a rule. */
function sectionsOf(items: readonly ContextItem[]): Section[] {
  const sections: Section[] = [];
  items.forEach((item, index) => {
    const last = sections.at(-1);
    if (last !== undefined && last.group === item.group && item.rule !== true) last.entries.push({ item, index });
    else sections.push({ group: item.group, entries: [{ item, index }] });
  });
  return sections;
}

/** The key that the item `id` has in the menu whose path is `path`. */
const keyOf = (path: readonly string[], id: string): string => [...path, id].join(SEP);

/**
 * A menu at the pointer, whose items do their own work. Arrow keys move between the items of a menu, Right (or Enter) opens the menu of an item that has one and Left (or Escape) closes it, Home and End go to the
 * first and the last, a letter (or the first few of a name, typed together) goes to the item that starts with it, Enter or Space chooses, and Escape on the first menu, or Tab, leaves it. Pointing at an item with a menu
 * opens it after a moment. The menu has the focus when it opens, with no item chosen, and gives it back where it was when it is left with the keyboard.
 */
export function ContextMenu({ label, at, items, onClose }: ContextMenuProps) {
  // The items whose menus are open, the first menu's first, and the item that has the focus, by key (none: the menu itself has it).
  const [open, setOpen] = useState<readonly string[]>([]);
  const [focused, setFocused] = useState<string | null>(null);
  const menus = useRef<(HTMLDivElement | null)[]>([]);
  const itemRefs = useRef<Record<string, HTMLButtonElement | null>>({});
  const typed = useRef({ text: "", at: 0 });
  const pending = useRef<number | undefined>(undefined);
  // Where the focus was when the menu opened, which Escape gives it back to.
  const [before] = useState(() => (typeof document === "undefined" ? null : document.activeElement));

  const levels = useMemo(() => {
    const found: Level[] = [{ items, path: [] }];
    let shown = items;
    const path: string[] = [];
    for (const id of open) {
      const parent = shown.find((item) => item.id === id);
      if (!parent?.items) break;
      path.push(id);
      found.push({ items: parent.items, path: [...path] });
      shown = parent.items;
    }
    return found;
  }, [items, open]);

  // The menu has the focus when it opens (before the page is painted, so that a letter that is typed is never the page's: its keys play notes).
  useLayoutEffect(() => {
    menus.current[0]?.focus({ preventScroll: true });
  }, []);

  // The focus follows the item that has it.
  useEffect(() => {
    if (focused !== null) itemRefs.current[focused]?.focus({ preventScroll: true });
  }, [focused, levels]);

  // Each menu is put where it fits, before the page is painted: the first at the pointer, the ones that open from an item beside it, on the side that has room.
  useLayoutEffect(() => {
    levels.forEach((level, depth) => {
      const element = menus.current[depth];
      if (!element) return;
      element.style.maxHeight = `${String(Math.max(120, window.innerHeight - 2 * EDGE))}px`;
      // (At the corner first, where it is as wide as it wants to be.)
      element.style.left = "0px";
      element.style.top = "0px";
      const box = element.getBoundingClientRect();
      const parent = depth === 0 ? null : itemRefs.current[keyOf(level.path.slice(0, -1), level.path.at(-1) ?? "")]?.getBoundingClientRect();
      let left: number;
      let top: number;
      if (!parent) {
        left = at.x;
        top = at.y;
      } else {
        const room = window.innerWidth - parent.right - EDGE;
        left = room >= box.width ? parent.right + 2 : parent.left - box.width - 2;
        top = parent.top - EDGE;
      }
      element.style.left = `${String(Math.max(EDGE, Math.min(left, window.innerWidth - box.width - EDGE)))}px`;
      element.style.top = `${String(Math.max(EDGE, Math.min(top, window.innerHeight - box.height - EDGE)))}px`;
      element.style.visibility = "visible";
    });
  }, [levels, at]);

  // A press anywhere else, the page being sized or the window left: the menu goes; so does it for a wheel that is not over it (a long list is scrolled by the wheel).
  useEffect(() => {
    const inside = (target: EventTarget | null): boolean => target instanceof Node && menus.current.some((element) => element?.contains(target) === true);
    const away = (event: Event): void => {
      if (!inside(event.target)) onClose();
    };
    const wheel = (event: Event): void => {
      if (!inside(event.target)) onClose();
    };
    // Escape closes the menu wherever the focus is; where it is in the menu, the menu takes the key itself (it closes the menu that has the focus, which may be one of its second menus).
    const escape = (event: KeyboardEvent): void => {
      if (event.key !== "Escape" || inside(event.target)) return;
      event.preventDefault();
      event.stopPropagation();
      onClose();
    };
    document.addEventListener("pointerdown", away, true);
    window.addEventListener("wheel", wheel, true);
    window.addEventListener("keydown", escape, true);
    window.addEventListener("resize", onClose);
    window.addEventListener("blur", onClose);
    return () => {
      document.removeEventListener("pointerdown", away, true);
      window.removeEventListener("wheel", wheel, true);
      window.removeEventListener("keydown", escape, true);
      window.removeEventListener("resize", onClose);
      window.removeEventListener("blur", onClose);
    };
  }, [onClose]);

  useEffect(
    () => () => {
      window.clearTimeout(pending.current);
    },
    [],
  );

  /** Opens the menu of the item `id` of the menu `depth` (the ones that were open from there on are closed), with the focus on its first item if `focus`. */
  const show = (depth: number, id: string, focus: boolean): void => {
    const level = levels[depth];
    const item = level?.items.find((candidate) => candidate.id === id);
    if (!level || !item?.items) return;
    setOpen([...level.path, id]);
    if (focus) {
      const first = item.items[0];
      setFocused(first ? keyOf([...level.path, id], first.id) : null);
    }
  };

  /** Keeps the first `keep` of the menus that are open (the first menu is not counted) and closes the rest. */
  const trim = (keep: number): void => {
    setOpen((now) => now.slice(0, keep));
  };

  /** Puts the focus back where it was before the menu opened, if that is still there. */
  const giveBackTheFocus = (): void => {
    if (before instanceof HTMLElement && before.isConnected) before.focus({ preventScroll: true });
  };

  /** Chooses an item: it runs, or opens its menu. */
  const choose = (depth: number, item: ContextItem): void => {
    if (item.items) {
      show(depth, item.id, true);
      return;
    }
    if (item.disabled === true) return;
    onClose();
    item.run?.();
  };

  /** What the pointer does to an item: it has the focus, and its menu opens (and another's closes) after a moment. */
  const pointed = (depth: number, item: ContextItem): void => {
    const level = levels[depth];
    if (!level) return;
    setFocused(keyOf(level.path, item.id));
    window.clearTimeout(pending.current);
    const there = open[depth] === item.id;
    if (there && depth === levels.length - 2) return;
    pending.current = window.setTimeout(() => {
      if (item.items) show(depth, item.id, false);
      else trim(depth);
    }, HOVER_MS);
  };

  /** A letter was typed in the menu `depth`: the focus goes to an item of it that starts with the text typed lately. */
  const typeAhead = (depth: number, key: string): void => {
    const level = levels[depth];
    if (!level) return;
    const now = Date.now();
    const text = (now - typed.current.at < TYPE_AHEAD_MS ? typed.current.text : "") + key.toLowerCase();
    typed.current = { text, at: now };
    const search = /^(.)\1*$/s.test(text) ? text.slice(0, 1) : text;
    // (-1: nothing of this menu has the focus, and a letter goes to the first item that starts with it, the first of the menu included.)
    const current = level.items.findIndex((item) => keyOf(level.path, item.id) === focused);
    const from = search.length === 1 ? current + 1 : Math.max(0, current);
    for (let step = 0; step < level.items.length; step += 1) {
      const found = level.items[(from + step) % level.items.length];
      if (found?.label.toLowerCase().startsWith(search)) {
        setFocused(keyOf(level.path, found.id));
        return;
      }
    }
  };

  /** Closes the menu `level` and gives the focus back to the item that opened it. */
  const leave = (level: Level): void => {
    trim(level.path.length - 1);
    const parent = level.path.at(-1);
    if (parent !== undefined) setFocused(keyOf(level.path.slice(0, -1), parent));
  };

  const onKey = (event: ReactKeyboardEvent<HTMLDivElement>, depth: number): void => {
    const level = levels[depth];
    if (!level) return;
    const count = level.items.length;
    const place = level.items.findIndex((item) => keyOf(level.path, item.id) === focused);
    const current = place < 0 ? undefined : level.items[place];
    const go = (index: number): void => {
      const target = level.items[index];
      if (target) setFocused(keyOf(level.path, target.id));
    };
    // (Not the page's, whatever they are: its keys play notes.)
    const take = (): void => {
      event.preventDefault();
      event.stopPropagation();
    };
    switch (event.key) {
      case "ArrowDown":
        take();
        go(place < 0 || place >= count - 1 ? 0 : place + 1);
        break;
      case "ArrowUp":
        take();
        go(place <= 0 ? count - 1 : place - 1);
        break;
      case "Home":
        take();
        go(0);
        break;
      case "End":
        take();
        go(count - 1);
        break;
      case "ArrowRight":
        take();
        if (current?.items) show(depth, current.id, true);
        break;
      case "ArrowLeft":
        take();
        if (depth > 0) leave(level);
        break;
      case "Enter":
      case " ":
        take();
        if (current) choose(depth, current);
        break;
      case "Escape":
        take();
        if (depth > 0) {
          leave(level);
        } else {
          onClose();
          giveBackTheFocus();
        }
        break;
      case "Tab":
        // (The menu is drawn at the end of the page: a Tab from it would go to what comes after the page's last control, so the focus goes back first, and the Tab goes on from there.)
        onClose();
        giveBackTheFocus();
        break;
      default:
        if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
          take();
          typeAhead(depth, event.key);
        }
    }
  };

  return createPortal(
    levels.map((level, depth) => (
      <MenuLevel
        key={level.path.join(SEP)}
        level={level}
        depth={depth}
        label={depth === 0 ? label : (levels[depth - 1]?.items.find((candidate) => candidate.id === level.path.at(-1))?.label ?? label)}
        focused={focused}
        opened={open[depth]}
        menuRef={(element) => {
          menus.current[depth] = element;
        }}
        itemRef={(key, element) => {
          itemRefs.current[key] = element;
        }}
        onKey={onKey}
        onPoint={pointed}
        onChoose={choose}
      />
    )),
    document.body,
  );
}

interface MenuLevelProps {
  level: Level;
  depth: number;
  /** What the menu is called, for a screen reader: what the context menu is of, or the item that opened this one. */
  label: string;
  /** The key of the item that has the focus, if it is one of the open menus'. */
  focused: string | null;
  /** The id of the item of this menu whose menu is open, if one is. */
  opened: string | undefined;
  menuRef: (element: HTMLDivElement | null) => void;
  itemRef: (key: string, element: HTMLButtonElement | null) => void;
  onKey: (event: ReactKeyboardEvent<HTMLDivElement>, depth: number) => void;
  onPoint: (depth: number, item: ContextItem) => void;
  onChoose: (depth: number, item: ContextItem) => void;
}

/** One of the menus that are open: its items in sections, each a button; where it is put is the context menu's (`style` is the part of it that React does not own). */
function MenuLevel({ level, depth, label, focused, opened, menuRef, itemRef, onKey, onPoint, onChoose }: MenuLevelProps) {
  return (
    <div
      ref={menuRef}
      role="menu"
      aria-label={label}
      aria-orientation="vertical"
      tabIndex={-1}
      className="tang-menu tang-context"
      style={{ position: "fixed", left: 0, top: 0, visibility: "hidden", overflowY: "auto" }}
      onKeyDown={(event) => {
        onKey(event, depth);
      }}
      onContextMenu={(event) => {
        // (Not the browser's own menu, over ours.)
        event.preventDefault();
      }}
    >
      {sectionsOf(level.items).map((section) => (
        <div key={section.entries[0]?.item.id} role="group" aria-label={section.group} className="tang-menu__group tang-context__section">
          {section.group === undefined ? null : (
            <div className="tang-menu__heading" aria-hidden="true">
              {section.group}
            </div>
          )}
          {section.entries.map(({ item }) => {
            const key = keyOf(level.path, item.id);
            const submenu = item.items !== undefined;
            return (
              <button
                key={item.id}
                ref={(element) => {
                  itemRef(key, element);
                }}
                type="button"
                role={item.checked === undefined ? "menuitem" : "menuitemcheckbox"}
                className={`tang-menu__item tang-context__item${item.danger === true ? " tang-menu__item--danger" : ""}${submenu ? " tang-context__item--submenu" : ""}${item.checked === true ? " tang-context__item--checked" : ""}`}
                title={item.title}
                tabIndex={focused === key ? 0 : -1}
                aria-current={item.current === true ? "true" : undefined}
                aria-checked={item.checked}
                aria-disabled={item.disabled === true ? true : undefined}
                aria-haspopup={submenu ? "menu" : undefined}
                aria-expanded={submenu ? opened === item.id : undefined}
                onPointerEnter={() => {
                  onPoint(depth, item);
                }}
                onClick={() => {
                  onChoose(depth, item);
                }}
              >
                {item.label}
              </button>
            );
          })}
        </div>
      ))}
    </div>
  );
}
