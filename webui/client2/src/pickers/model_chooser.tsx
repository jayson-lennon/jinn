// The chooser the model chip opens: two rows, one per picker.
//
// The chip used to open the provider picker directly. The market needed a
// way in without displacing that picker — alloys and non-OpenRouter
// providers still select through it — so the chip now asks which one, and
// each row opens its picker as before. No descriptions: the two names say
// what they are.

import { onCleanup } from 'solid-js';
import type { JSX } from '@solidjs/web';

/** The two-row chooser behind the model chip. */
export function ModelChooser(props: {
  /** Opens the OpenRouter model market. */
  onMarket: () => void;
  /** Opens the provider model picker. */
  onProvider: () => void;
  onClose: () => void;
}): JSX.Element {
  // Escape closes the chooser; the listener lives for the mount only.
  const onKey = (e: KeyboardEvent): void => {
    if (e.key === 'Escape') props.onClose();
  };
  document.addEventListener('keydown', onKey);
  onCleanup(() => document.removeEventListener('keydown', onKey));
  return (
    <div
      class="modal-backdrop"
      onClick={(e) => { if (e.target === e.currentTarget) props.onClose(); }}
    >
      <div class="modal chooser" role="dialog" aria-label="Choose a model picker" data-testid="model-chooser">
        <button
          type="button"
          class="chooser-row"
          data-testid="chooser-market"
          onClick={() => { props.onClose(); props.onMarket(); }}
        >
          OpenRouter Model Market
        </button>
        <button
          type="button"
          class="chooser-row"
          data-testid="chooser-provider"
          onClick={() => { props.onClose(); props.onProvider(); }}
        >
          Provider Model Picker
        </button>
      </div>
    </div>
  );
}
