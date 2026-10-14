// Runs before every test file (vitest `setupFiles`). jsdom has no layout, which React Flow
// measures its nodes by, so the parts of the browser it asks are made up here: every element
// is as big as its style says (or one pixel), an element that is observed for its size is
// told at once what it is, and nothing is under any point. Without a window (the tests that need
// none) it does nothing.
if (typeof window !== "undefined") {
  class ObserverOfSizes {
    constructor(private readonly callback: (entries: unknown[], observer: unknown) => void) {}
    observe(target: Element): void {
      this.callback([{ target, contentRect: { width: 1, height: 1 } }], this);
    }
    unobserve = (): void => undefined;
    disconnect = (): void => undefined;
  }
  class Matrix {
    m22 = 1;
  }
  Object.assign(globalThis, { ResizeObserver: ObserverOfSizes, DOMMatrixReadOnly: Matrix });
  // jsdom has no hit testing either: no element is under any point (a test that needs one says which).
  document.elementFromPoint = () => null;
  const sizeOf = (element: HTMLElement, property: "width" | "height"): number => Number.parseFloat(element.style[property]) || 1;
  Object.defineProperties(window.HTMLElement.prototype, {
    offsetWidth: {
      configurable: true,
      get(this: HTMLElement) {
        return sizeOf(this, "width");
      },
    },
    offsetHeight: {
      configurable: true,
      get(this: HTMLElement) {
        return sizeOf(this, "height");
      },
    },
  });
}
