import { useSyncExternalStore } from "react";

import type { Notices } from "../client/notices";
import "./NoticeBoard.css";

/**
 * What the page says of what it could not do: a short list, each notice with a way to dismiss it. It
 * is a polite live region, so a screen reader says a notice when it comes, and the list stays until
 * the person has read it (a notice that went by itself could be missed).
 */
export function NoticeBoard({ notices }: { notices: Notices }) {
  const items = useSyncExternalStore(notices.subscribe, notices.getItems);
  return (
    <div className="tang-notices" aria-live="polite" aria-relevant="additions text">
      {items.map((notice) => {
        const text = notice.count > 1 ? `${notice.text} (${String(notice.count)} times)` : notice.text;
        return (
          <p key={notice.id} className="tang-notice">
            <span className="tang-notice__text">{text}</span>
            <button
              type="button"
              className="tang-notice__dismiss"
              aria-label={`Dismiss: ${notice.text}`}
              onClick={() => {
                notices.dismiss(notice.id);
              }}
            >
              ×
            </button>
          </p>
        );
      })}
    </div>
  );
}
