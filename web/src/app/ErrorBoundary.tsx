// What the page does when a part of it cannot be drawn: it says so, where the part was, and leaves the
// rest (the state of the connection, the session as JSON) to be used. A value the server could not write
// (JSON has no infinity: it says null) must not blank the page.
import { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";

interface Props {
  /** What the children draw, for the message: "the patch". */
  what: string;
  children: ReactNode;
}

interface State {
  error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
  override state: State = { error: null };

  static getDerivedStateFromError(error: unknown): State {
    return { error: error instanceof Error ? error : new Error(String(error)) };
  }

  override componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error(`The page could not draw ${this.props.what}`, error, info.componentStack);
  }

  override render(): ReactNode {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <div className="tang-app__problem" role="alert">
        <p>
          The page could not draw {this.props.what}: {error.message}
        </p>
        <button
          type="button"
          onClick={() => {
            this.setState({ error: null });
          }}
        >
          Try again
        </button>
      </div>
    );
  }
}
