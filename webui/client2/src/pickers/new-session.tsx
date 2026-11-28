// The new-session modal: project, lifecycle, and setup arguments.
//
// The choices are the server's — asked for when the modal opens and held
// in the store — because they are the live `jinn.toml`. With no project
// configured there is nothing to create against, and the modal says so
// rather than offering a form that cannot submit. The form's own fields
// are component state: nobody else reads a half-filled form, and a closed
// modal should reopen blank.

import { For, Show, createEffect, createSignal, onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';
import type { ClientMsg, LifecycleOption } from '../protocol/protocol';
import type { ClientStore } from '../store/store';

export interface NewSessionModalProps {
  store: ClientStore;
  send: (msg: ClientMsg) => void;
  /** Closes the modal. */
  onClose: () => void;
}

export function NewSessionModal(props: NewSessionModalProps): JSX.Element {
  const options = () => props.store.state.newSession;
  const projects = (): string[] => options()?.projects ?? [];
  const lifecycles = () => options()?.lifecycles ?? [];

  const [project, setProject] = createSignal('');
  const [lifecycle, setLifecycle] = createSignal('');
  const [args, setArgs] = createSignal('');

  // A `<select>` shows its first option the moment the options land, but
  // no `change` fired to say so: the signal would still read as empty and
  // a submit would silently do nothing. The first project is seeded as the
  // choice the user is already looking at.
  createEffect(
    () => projects()[0],
    (first) => {
      if (first !== undefined) setProject(first);
    },
  );

  // The selected lifecycle's argument minimum; the field shows only when
  // a lifecycle declares at least one.
  const chosen = (): LifecycleOption | undefined =>
    lifecycles().find((option) => option.name === lifecycle());
  const paramCount = (): number => chosen()?.param_count ?? 0;
  // What the setup's template actually calls each argument: `<branch>`,
  // not a label the page made up from its position.
  const argTokens = (): string => (chosen()?.params ?? []).join(' ');

  // Escape closes the modal. The listener is removed on unmount so a closed
  // modal never swallows the key another overlay wants.
  const onKeydown = (event: KeyboardEvent): void => {
    if (event.key === 'Escape') {
      event.stopPropagation();
      props.onClose();
    }
  };
  document.addEventListener('keydown', onKeydown);
  onCleanup(() => document.removeEventListener('keydown', onKeydown));

  const create = (): void => {
    const chosen = project();
    if (chosen === '') return;
    props.send({
      type: 'create_session',
      project: chosen,
      lifecycle: lifecycle(),
      args: args(),
    });
    props.onClose();
  };

  return (
    <div
      class="modal-backdrop"
      onClick={(e) => {
        if (e.target === e.currentTarget) props.onClose();
      }}
    >
      <div class="modal new-session-modal" role="dialog" aria-label="New session" data-testid="new-session-modal">
        <h2 class="new-session-title">New session</h2>
        <Show
          when={projects().length > 0}
          fallback={
            <p class="new-session-empty" data-testid="new-session-empty">
              No projects are configured. Add a <code>[[project.entry]]</code> with a{' '}
              <code>path</code> to your <code>jinn.toml</code>, then reload this page.
            </p>
          }
        >
          <div class="new-session-field">
            <label for="new-session-project">Project</label>
            <select
              id="new-session-project"
              data-testid="new-session-project"
              autocomplete="off"
              value={project()}
              onChange={(e) => setProject(e.currentTarget.value)}
            >
              <For each={projects()}>{(path) => <option value={path}>{path}</option>}</For>
            </select>
          </div>
          <div class="new-session-field">
            <label for="new-session-lifecycle">Lifecycle</label>
            <select
              id="new-session-lifecycle"
              data-testid="new-session-lifecycle"
              autocomplete="off"
              value={lifecycle()}
              onChange={(e) => {
                setLifecycle(e.currentTarget.value);
                setArgs('');
              }}
            >
              <option value="">blank, new empty session</option>
              <For each={lifecycles()}>{(option) => <option value={option.name}>{option.name}</option>}</For>
            </select>
          </div>
          <Show when={paramCount() > 0}>
            <div class="new-session-field">
              <label for="new-session-args">Setup arguments</label>
              <input
                id="new-session-args"
                data-testid="new-session-args"
                type="text"
                autocomplete="off"
                placeholder={`Please enter: ${argTokens()}`}
                value={args()}
                onInput={(e) => setArgs(e.currentTarget.value)}
              />
              <p class="new-session-hint">fill {argTokens()}, space-separated, quoting spaces</p>
            </div>
          </Show>
          <div class="new-session-actions">
            <button
              type="button"
              class="new-session-submit"
              data-testid="create-session"
              onClick={create}
            >
              Create session
            </button>
          </div>
        </Show>
      </div>
    </div>
  );
}
