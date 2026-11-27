// The composer: the draft, the pending images, and the one row they sit in.
//
// The draft and the chips are store state rather than the component's own,
// so a session switch — which replaces the transcript — leaves them where
// they are and the user never loses unsent text. This component reads
// them, writes them, and turns a submit into the one message a send or a
// queue is. Nothing here holds view state of its own — the one signal is
// the draft textarea's ref, so the popup can read the caret.
//
// The `#` popup is the one thing it draws besides the draft. Whether it is
// open is derived from the caret — re-read on every input, key, click, and
// selection change — so a caret moved out of its token by any means closes
// it. The popup is not a modal: it has no backdrop and never takes focus.

import { createSignal, For, Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientMsg, ImageUpload, PromptTemplate } from './protocol/protocol';
import { filterPrompts, findPromptCompletion } from './prompts';
import { type ClientStore, isBusy } from './store/store';
import { addOptimistic } from './transcript_entries';
import {
  addChips,
  clearComposer,
  movePromptHighlight,
  pickPrompt,
  promptHighlightOf,
  removeChip,
  setDraft,
  setPromptCompletion,
} from './composer_state';
import {
  composerKeyAction,
  decideSend,
  hasSendContent,
  isTap,
  type DraftKey,
} from './composer_keys';

export interface ComposerProps {
  store: ClientStore;
  send: (msg: ClientMsg) => void;
}

/** Reads a picked or pasted file into the wire's base64 shape. */
function readImage(file: File, name: string): Promise<ImageUpload> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error);
    reader.onload = () => {
      const url = typeof reader.result === 'string' ? reader.result : '';
      resolve({ name, data_base64: url.slice(url.indexOf(',') + 1) });
    };
    reader.readAsDataURL(file);
  });
}

/** The image files a clipboard event carries, if any. */
function pastedImages(event: ClipboardEvent): File[] {
  const files = event.clipboardData?.files;
  if (!files) return [];
  return Array.from(files).filter((file) => file.type.startsWith('image/'));
}

export function Composer(props: ComposerProps): JSX.Element {
  const busy = (): boolean => isBusy(props.store.state.phase);
  // How many messages already wait behind the running turn — what tells the
  // user a queued message was taken, since it does not enter the transcript
  // until its turn runs.
  const queueLen = (): number => props.store.state.queueLen;
  const draft = (): string => props.store.state.draft;
  const chips = (): ImageUpload[] => props.store.state.chips;
  // A send needs something to send: text, or an image.
  const hasContent = (): boolean => hasSendContent(draft(), chips());

  const placeholder = (): string => {
    if (!busy()) return 'Message jinn…';
    return queueLen() === 0
      ? 'Queue a message for after this turn…'
      : `Queue a message — ${queueLen()} already queued`;
  };

  // The draft's textarea, read for the caret. A signal so the popup, which
  // is rendered beside the textarea, re-renders once the ref is set.
  const [draftRef, setDraftRef] = createSignal<HTMLTextAreaElement | undefined>();
  const caret = (): number => draftRef()?.selectionStart ?? 0;

  // The popup's rows: the session's templates filtered by what follows the
  // `#` the caret sits in. Derived, never stored, so a new list from the
  // server re-filters an open popup in place.
  const completion = () => props.store.state.promptCompletion;
  const rows = (): PromptTemplate[] =>
    filterPrompts(props.store.state.promptTemplates, completion()?.filter ?? '');
  const highlight = (): number => promptHighlightOf(props.store, rows().length);

  /** Re-derives the completion from where the caret is now. */
  const syncCompletion = (): void => {
    const textarea = draftRef();
    if (textarea === undefined) return;
    setPromptCompletion(
      props.store,
      findPromptCompletion(props.store.state.draft, textarea.selectionStart),
    );
  };

  /** Picks a template and places the caret after the inserted name. */
  // Set while a tap's pick is in flight. A touch ends with the browser
  // moving focus off the textarea — after pointerup, on its own schedule —
  // and a timer cannot be placed after that reliably. So the blur itself
  // is what restores focus, when a pick caused it. Cleared once it has.
  let picking = false;

  const pick = (name: string): void => {
    const at = pickPrompt(props.store, name);
    const textarea = draftRef();
    if (at === null || textarea === undefined) return;
    textarea.setSelectionRange(at, at);
    textarea.focus();
    picking = true;
  };

  /** The draft's blur: a pick's own blur gives focus straight back; any
   *  other blur closes the popup, since there is no caret to complete at. */
  const onDraftBlur = (): void => {
    if (picking) {
      picking = false;
      draftRef()?.focus();
      return;
    }
    setPromptCompletion(props.store, null);
  };

  const attach = (files: File[]): void => {
    for (const file of files) {
      readImage(file, file.name || 'pasted image')
        .then((image) => addChips(props.store, [image]))
        .catch(() => undefined);
    }
  };

  const submit = (): void => {
    // Everything is read before the composer is cleared: the images ride
    // the message, and reading them after the clear would send none.
    const text = draft();
    const images = chips();
    const message = decideSend(text, images, props.store.state.phase);
    if (message === null) return;
    // The message appears now; the server's own entry replaces it.
    if (text.trim() !== '') addOptimistic(props.store, text);
    clearComposer(props.store);
    props.send(message);
  };

  // Set when keydown fully settled a key — Escape closed the popup, Tab
  // picked, an arrow moved only the highlight. The keyup that follows must
  // not re-derive the completion from the unmoved caret, or it would undo
  // the close and the pick, and reset the highlight an arrow just moved.
  let settledKey: string | undefined;

  const onDraftKeyDown = (e: KeyboardEvent): void => {
    // Which key does what is `composerKeyAction`'s decision; this only
    // carries it out. Ctrl+Enter and Shift+Enter send or queue; Enter
    // inserts a newline; an open completion never turns a key into a pick —
    // only Tab picks.
    const action = composerKeyAction(
      { key: e.key, ctrl: e.ctrlKey, shift: e.shiftKey },
      completion() !== null,
      rows().length,
    );
    switch (action.kind) {
      case 'send': {
        e.preventDefault();
        submit();
        return;
      }
      case 'none': {
        return;
      }
      case 'close': {
        e.preventDefault();
        settledKey = e.key;
        setPromptCompletion(props.store, null);
        return;
      }
      case 'move': {
        e.preventDefault();
        settledKey = e.key;
        movePromptHighlight(props.store, rows().length, action.delta);
        return;
      }
      case 'pick': {
        // Tab with nothing to pick never reaches here: the action is `none`
        // instead, and focus moves on as it would with no popup open.
        e.preventDefault();
        settledKey = e.key;
        pick(rows()[highlight()]?.name ?? '');
        return;
      }
    }
    // No fallback arm: an action kind without a case above must fail the
    // type check here, not fall through unperformed.
    const unhandled: never = action;
    return void unhandled;
  };

  /** Re-derives the completion once a key has landed — unless keydown
   *  already settled that key, in which case there is nothing to re-read. */
  const onDraftKeyUp = (e: KeyboardEvent): void => {
    if (settledKey !== undefined) {
      const settled = settledKey === e.key;
      settledKey = undefined;
      if (settled) return;
    }
    syncCompletion();
  };

  // A tap on a popup row picks it — but a finger that moves is a scroll,
  // not a tap. The pointer's down position is remembered, and the pick
  // happens on pointerup only when `isTap` says the pointer stayed within
  // the slop of where it went down. Without this a phone's scroll gesture
  // would pick whatever row the finger happened to land on.
  let pressAt: { x: number; y: number } | undefined;

  const onRowPointerDown = (e: PointerEvent): void => {
    pressAt = { x: e.clientX, y: e.clientY };
    // For a mouse, preventing the default keeps the textarea focused: the
    // browser would otherwise blur it to focus the row. A touch must keep
    // its default so the browser can start a scroll from this row; the
    // focus it loses is restored by the pick, once the gesture is over.
    if (e.pointerType === 'mouse') e.preventDefault();
  };

  // A tap picks on pointerup. A touch ends with the browser moving focus
  // off the textarea right after — and no click reaches the row on a
  // phone — so the pick's own blur is what gives focus back (see
  // `onDraftBlur`). For a mouse a click does follow, and the row swallows
  // it so nothing underneath takes focus.
  const onRowPointerUp = (e: PointerEvent, name: string): void => {
    const at = pressAt;
    pressAt = undefined;
    if (at === undefined) return;
    if (isTap(at.x, at.y, e.clientX, e.clientY)) pick(name);
  };

  /** Swallows the click a mouse tap leaves behind: the popup has closed
   *  and unmounted this row, so an unswallowed click would land on the
   *  transcript and blur the draft. */
  const onRowClick = (e: MouseEvent): void => {
    e.preventDefault();
    e.stopPropagation();
  };

  return (
    <footer class="composer" data-testid="composer">
      <Show when={completion() !== null}>
        <ul class="prompt-popup" role="listbox" data-testid="prompt-popup">
          <Show when={rows().length === 0}>
            <li class="prompt-row muted" data-testid="prompt-empty">
              no prompts found
            </li>
          </Show>
          <For each={rows()}>
            {(template, index) => (
              <li
                class={`prompt-row${index() === highlight() ? ' active' : ''}`}
                data-testid="prompt-row"
                data-prompt={template.name}
                role="option"
                aria-selected={index() === highlight() ? 'true' : 'false'}
                onPointerDown={onRowPointerDown}
                onPointerUp={(e) => onRowPointerUp(e, template.name)}
                // A pointer that left the row, or was cancelled, is no
                // longer a tap on it.
                onPointerLeave={() => (pressAt = undefined)}
                onPointerCancel={() => (pressAt = undefined)}
                // The pick on pointerup closes the popup, which unmounts
                // this row before the browser dispatches its click — so the
                // click lands on whatever is underneath and blurs the
                // draft. Swallowing it here keeps the focus the pick set.
                onClick={onRowClick}
              >
                <span class="prompt-name">{template.name}</span>
                <Show when={template.description}>
                  <span class="prompt-desc"> - {template.description}</span>
                </Show>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={chips().length > 0}>
        <div class="chips" data-testid="chips">
          <For each={chips()}>
            {(chip, index) => (
              <div class="chip" data-testid="chip" data-chip-name={chip.name}>
                {/* The wire carries no media type — sniffing is the server's
                    job — so the browser is told nothing and figures it out
                    from the bytes. */}
                <img class="chip-img" src={`data:;base64,${chip.data_base64}`} alt="" />
                <span class="chip-name">{chip.name}</span>
                <button
                  type="button"
                  class="chip-x"
                  data-testid="chip-remove"
                  aria-label={`Remove ${chip.name}`}
                  onClick={() => removeChip(props.store, index())}
                >
                  ✕
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>
      <div class="composer-row">
        <textarea
          ref={setDraftRef}
          class="draft"
          data-testid="draft"
          placeholder={placeholder()}
          value={draft()}
          onInput={(e) => {
            setDraft(props.store, e.currentTarget.value);
            syncCompletion();
          }}
          onKeyDown={onDraftKeyDown}
          // The keys the popup does not intercept still move the caret, and
          // the browser applies them after keydown — so the completion is
          // re-read once the key has landed.
          onKeyUp={onDraftKeyUp}
          onClick={syncCompletion}
          onSelect={syncCompletion}
          // Losing focus closes the popup: there is no caret in the draft
          // to complete at anymore.
          onBlur={onDraftBlur}
          onPaste={(e) => {
            const images = pastedImages(e);
            if (images.length === 0) return;
            e.preventDefault();
            attach(images);
          }}
        />
        <label class="attach" data-testid="attach" title="Attach an image">
          <input
            type="file"
            data-testid="attach-input"
            accept="image/*"
            multiple
            onChange={(e) => {
              attach(Array.from(e.currentTarget.files ?? []));
              e.currentTarget.value = '';
            }}
          />
          +
        </label>
        <div class="composer-actions">
          <Show
            when={busy()}
            fallback={
              <button class="primary" data-testid="send" onClick={submit} disabled={!hasContent()}>
                Send
              </button>
            }
          >
            <button data-testid="queue" onClick={submit} disabled={!hasContent()}>
              Q
              <Show when={queueLen() > 0}>
                <span class="queue-depth" data-testid="queue-depth">
                  {queueLen()}
                </span>
              </Show>
            </button>
            <button class="danger" data-testid="cancel" onClick={() => props.send({ type: 'cancel' })}>
              Cancel
            </button>
          </Show>
        </div>
      </div>
    </footer>
  );
}
