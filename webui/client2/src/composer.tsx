// The composer: the draft, the pending images, and the one row they sit in.
//
// The draft and the chips are store state rather than the component's own,
// so a session switch — which replaces the transcript — leaves them where
// they are and the user never loses unsent text. This component reads
// them, writes them, and turns a submit into the one message a send or a
// queue is. Nothing here holds state of its own.

import { For, Show } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientMsg, ImageUpload } from './protocol';
import {
  type ClientStore,
  addChips,
  addOptimistic,
  clearComposer,
  isBusy,
  removeChip,
  setDraft,
} from './store';

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
  const hasContent = (): boolean => draft().trim() !== '' || chips().length > 0;

  const placeholder = (): string => {
    if (!busy()) return 'Message jinn…';
    return queueLen() === 0
      ? 'Queue a message for after this turn…'
      : `Queue a message — ${queueLen()} already queued`;
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
    if (text.trim() === '' && images.length === 0) return;
    // The message appears now; the server's own entry replaces it.
    if (text.trim() !== '') addOptimistic(props.store, text);
    clearComposer(props.store);
    props.store.set((s) => { s.error = null; });
    props.send({ type: busy() ? 'queue' : 'send', text, images });
  };

  return (
    <footer class="composer" data-testid="composer">
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
          class="draft"
          data-testid="draft"
          placeholder={placeholder()}
          value={draft()}
          onInput={(e) => setDraft(props.store, e.currentTarget.value)}
          onKeyDown={(e) => {
            // Enter sends; Shift+Enter is a newline.
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
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
      <Show when={props.store.state.error}>
        <div class="composer-error" role="alert" data-testid="composer-error">
          {props.store.state.error}
        </div>
      </Show>
    </footer>
  );
}
