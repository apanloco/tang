import { useEffect, useId, useRef, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import { createPortal } from "react-dom";

import "./MenuButton.css";

export interface MenuItem<Id extends string = string> {
  id: Id;
  label: string;
}

export interface MenuButtonProps<Id extends string = string> {
  /** What pressing the button does, for a screen reader (and a tooltip): the button itself is a glyph. */
  label: string;
  /** The `id` of the button, for whatever has to give the focus back to it. */
  id?: string;
  items: readonly MenuItem<Id>[];
  /** The id of the item that was chosen. */
  onChoose: (id: Id) => void;
  /** What the button shows. */
  children: ReactNode;
}

/** About how tall an item is, to tell whether the menu fits under the button before it has been drawn. */
const ITEM_HEIGHT = 30;

interface Place {
  left: number;
  top: number;
  above: boolean;
}

/**
 * A button that opens a menu: the pattern of the WAI-ARIA authoring practices. The menu is drawn in the page and
 * not where the button is (the button is in a node of the canvas, which would put the menu under the next node
 * and move it with the canvas), so it closes when the page is scrolled, sized, or pressed anywhere else. The
 * arrow keys, Home and End move between its items, Enter or Space chooses, Escape leaves it for the button
 * and Tab leaves it for what comes after the button (or before it, with Shift).
 */
export function MenuButton<Id extends string>({ label, id, items, onChoose, children }: MenuButtonProps<Id>) {
  const [place, setPlace] = useState<Place | null>(null);
  const [focused, setFocused] = useState(0);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const menuId = useId();
  const open = place !== null;

  const openAt = (index: number): void => {
    const box = trigger.current?.getBoundingClientRect();
    if (!box) return;
    const needed = items.length * ITEM_HEIGHT + 8;
    const above = box.bottom + needed > window.innerHeight && box.top - needed > 0;
    setPlace({ left: box.right, top: above ? box.top - 2 : box.bottom + 2, above });
    setFocused(index);
  };

  const close = (refocus: boolean): void => {
    setPlace(null);
    if (refocus) trigger.current?.focus();
  };

  // The focus follows the item that has it.
  useEffect(() => {
    if (open) itemRefs.current[focused]?.focus();
  }, [open, focused]);

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

  const onMenuKey = (event: KeyboardEvent<HTMLDivElement>): void => {
    const last = items.length - 1;
    const moves: Record<string, number> = { ArrowDown: focused >= last ? 0 : focused + 1, ArrowUp: focused <= 0 ? last : focused - 1, Home: 0, End: last };
    const move = moves[event.key];
    if (move !== undefined) {
      event.preventDefault();
      event.stopPropagation();
      setFocused(move);
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close(true);
    } else if (event.key === "Tab") {
      // The menu is drawn at the end of the page, so a Tab from it would go to whatever comes after the page's
      // last control: the focus goes back to the button first, and the Tab that comes after goes on from there.
      close(true);
    }
  };

  return (
    <>
      <button
        ref={trigger}
        id={id}
        type="button"
        className="tang-menu-button nodrag nopan"
        aria-label={label}
        title={label}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        disabled={items.length === 0}
        onClick={() => {
          if (open) close(false);
          else openAt(0);
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
              style={{ position: "fixed", left: `${String(place.left)}px`, top: `${String(place.top)}px`, transform: place.above ? "translate(-100%, -100%)" : "translateX(-100%)" }}
              onKeyDown={onMenuKey}
            >
              {items.map((item, index) => (
                <button
                  key={item.id}
                  ref={(element) => {
                    itemRefs.current[index] = element;
                  }}
                  type="button"
                  role="menuitem"
                  className="tang-menu__item"
                  tabIndex={index === focused ? 0 : -1}
                  onClick={() => {
                    onChoose(item.id);
                    close(true);
                  }}
                >
                  {item.label}
                </button>
              ))}
            </div>,
            document.body,
          )
        : null}
    </>
  );
}
