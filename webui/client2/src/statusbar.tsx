// The session status bar: the persona on the left, the context usage figure
// on the right.
//
// Both are facts the server pushes and the store holds; this component
// renders them and forwards one gesture — a click on the persona name opens
// the persona modal, the same one the menu item opens. The figure is the
// server's own formatted string (the terminal's), so nothing is computed
// here: no colour, no threshold, no hover hint. It is text.

import type { JSX } from '@solidjs/web';

export function StatusBar(props: {
  readonly persona: string;
  readonly contextLabel: string;
  readonly onOpenPersona: () => void;
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
      <span class="status-context" data-testid="status-context">
        {props.contextLabel}
      </span>
    </div>
  );
}
