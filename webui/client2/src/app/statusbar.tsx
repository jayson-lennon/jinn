// The session status bar: the persona on the left, the context usage figure
// on the right.
//
// Both are facts the server pushes and the store holds; this component
// renders them and forwards two gestures — a click on the persona name opens
// the persona modal, the same one the menu item opens, and a click on the
// figure opens the session dash. The figure is the server's own formatted
// string (the terminal's), so nothing is computed here: no colour, no
// threshold. It is text, and a button.

import type { JSX } from '@solidjs/web';

export function StatusBar(props: {
  readonly persona: string;
  readonly contextLabel: string;
  readonly onOpenPersona: () => void;
  readonly onOpenDash: () => void;
}): JSX.Element {
  return (
    <div class="status-bar" data-testid="status-bar">
      <button
        type="button"
        class="status-persona"
        data-testid="status-persona"
        title="Switch persona"
        onClick={props.onOpenPersona}
      >
        {props.persona}
      </button>
      <button
        type="button"
        class="status-context"
        data-testid="status-context"
        title="Session dashboard"
        onClick={props.onOpenDash}
      >
        {props.contextLabel}
      </button>
    </div>
  );
}
