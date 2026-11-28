// The change-directory modal: a text field seeded with the session's cwd,
// a live red/green footer, Accept and Cancel.
//
// The footer's verdict is the server's, asked for each time the text
// settles for a moment. An edit marks the held verdict stale: the footer
// keeps showing it, so it does not blink on every keystroke, while Accept
// is disabled until the current text's own verdict lands. A late answer for
// a path the user typed through is dropped by the store, never shown.
//
// Dismissal matches the page's other modals: Cancel, a tap on the backdrop,
// or Escape. Accept sends the draft to the server, which resolves it again
// before anything changes, and closes the modal.

import { onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientStore } from '../store/store';
import type { ClientMsg, CwdVerdict } from '../protocol/protocol';
import { CWD_CHECK_DEBOUNCE_MS, closeCwdModal, editCwdDraft } from './cwd_state';

type Send = (msg: ClientMsg) => void;

/** What the footer says and how it is coloured, by the verdict held. */
function footerOf(verdict: CwdVerdict): { readonly tone: 'ok' | 'bad' | 'none'; readonly text: string } {
  switch (verdict.kind) {
    case 'ok':
      return { tone: 'ok', text: verdict.path };
    case 'not_a_directory':
      return { tone: 'bad', text: `not a directory: ${verdict.path}` };
    case 'empty':
      return { tone: 'none', text: 'type a path (use ~ or a relative path)' };
  }
}

export function CwdModal(props: { readonly store: ClientStore; readonly send: Send }): JSX.Element {
  const close = (): void => closeCwdModal(props.store);

  // The field takes focus with its whole seed selected, so typing replaces
  // the seed the way the terminal popup's does. Both are done here, on the
  // element, because nothing else does them: a dynamically mounted field's
  // autofocus attribute never focuses it, and a focus that arrives later puts
  // the caret at the start, where typing would prepend to the seed instead.
  const mountField = (el: HTMLInputElement): void => {
    el.value = props.store.state.cwdDraft;
    queueMicrotask(() => {
      el.focus();
      el.select();
    });
  };

  const onKeydown = (event: KeyboardEvent): void => {
    if (event.key !== 'Escape') return;
    event.stopPropagation();
    close();
  };
  document.addEventListener('keydown', onKeydown);
  onCleanup(() => document.removeEventListener('keydown', onKeydown));

  // One check per pause in typing: each edit clears the last timer and arms
  // a new one, so a fast typer pays one round trip per pause, not per key.
  let timer: ReturnType<typeof setTimeout> | undefined;
  const onInput = (value: string): void => {
    editCwdDraft(props.store, value);
    clearTimeout(timer);
    timer = setTimeout(() => props.send({ type: 'check_cwd', text: value }), CWD_CHECK_DEBOUNCE_MS);
  };
  onCleanup(() => clearTimeout(timer));

  const footer = () => footerOf(props.store.state.cwdVerdict.verdict);
  const canAccept = (): boolean => props.store.state.cwdVerdict.verdict.kind === 'ok' && !props.store.state.cwdVerdict.stale;

  const accept = (): void => {
    if (!canAccept()) return;
    props.send({ type: 'set_cwd', text: props.store.state.cwdDraft });
    close();
  };

  return (
    <div
      class="modal-backdrop"
      onClick={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div class="modal cwd-modal" role="dialog" aria-label="Change directory" data-testid="cwd-modal">
        <h2 class="cwd-title">Change directory</h2>
        <input
          class="cwd-input"
          data-testid="cwd-input"
          spellcheck={false}
          ref={mountField}
          onInput={(e) => onInput(e.currentTarget.value)}
        />
        <div class="cwd-footer" data-tone={footer().tone} data-testid="cwd-footer">
          {footer().text}
        </div>
        <div class="cwd-actions">
          <button type="button" class="cwd-cancel" data-testid="cwd-cancel" onClick={close}>
            Cancel
          </button>
          <button type="button" class="cwd-accept" data-testid="cwd-accept" disabled={!canAccept()} onClick={accept}>
            Accept
          </button>
        </div>
      </div>
    </div>
  );
}
