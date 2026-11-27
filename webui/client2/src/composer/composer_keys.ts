// The composer's decisions, apart from the component that performs them.
//
// Which keystroke does what, whether a send has anything to send, and
// whether a pointer gesture is a tap: each was inline in `composer.tsx`,
// where no unit test could reach it. Here they are pure functions over plain
// data — no store, no DOM, no events — and the component only carries out
// what they decide. Moving them changed nothing about what a key does.

import type { ImageUpload, Phase } from '../protocol/protocol';
import { isBusy } from '../store/store';

/** One keystroke in the draft, as the key handler sees it. */
export interface DraftKey {
  readonly key: string;
  readonly ctrl: boolean;
  readonly shift: boolean;
}

/** What a keystroke in the draft does. */
export type ComposerKeyAction =
  | { readonly kind: 'send' }
  | { readonly kind: 'close' }
  | { readonly kind: 'move'; readonly delta: 1 | -1 }
  | { readonly kind: 'pick' }
  | { readonly kind: 'none' };

/** The message a send publishes, or null when there is nothing to send. */
export type SendDecision =
  | { readonly type: 'send' | 'queue'; readonly text: string; readonly images: ImageUpload[] }
  | null;

/**
 * Decides what a keystroke in the draft does.
 *
 * Enter with Ctrl or Shift sends, whether or not the popup is open. With no
 * popup open, nothing else is intercepted. With the popup open, Escape
 * closes it, an arrow moves the highlight, and Tab picks — but only when
 * there is a row to pick. Plain Enter is never intercepted: it inserts a
 * newline, so an open completion never turns a key into a pick.
 */
export function composerKeyAction(
  key: DraftKey,
  popupOpen: boolean,
  rowCount: number,
): ComposerKeyAction {
  if (key.key === 'Enter' && (key.ctrl || key.shift)) return { kind: 'send' };
  if (!popupOpen) return { kind: 'none' };
  switch (key.key) {
    case 'Escape':
      return { kind: 'close' };
    case 'ArrowUp':
      return { kind: 'move', delta: -1 };
    case 'ArrowDown':
      return { kind: 'move', delta: 1 };
    case 'Tab':
      return rowCount > 0 ? { kind: 'pick' } : { kind: 'none' };
    default:
      return { kind: 'none' };
  }
}

/** Whether a send has something to send: text, or an image. */
export function hasSendContent(text: string, images: readonly ImageUpload[]): boolean {
  return text.trim() !== '' || images.length > 0;
}

/**
 * The message a send publishes: `queue` while a turn runs, `send` at idle,
 * and null when the draft is empty and no image is pending. The images are
 * copied so the message is not a live view of the composer's chips.
 */
export function decideSend(
  text: string,
  images: readonly ImageUpload[],
  phase: Phase,
): SendDecision {
  if (!hasSendContent(text, images)) return null;
  return { type: isBusy(phase) ? 'queue' : 'send', text, images: [...images] };
}

/** How far a pointer may move between down and up and still be a tap. */
export const TAP_SLOP_PX = 10;

/**
 * Whether a pointer that went down at (downX, downY) and came up at
 * (upX, upY) is a tap. A finger that moved is a scroll, not a pick, so a
 * phone's scroll gesture never picks the row it happened to start on.
 */
export function isTap(downX: number, downY: number, upX: number, upY: number): boolean {
  return Math.hypot(upX - downX, upY - downY) <= TAP_SLOP_PX;
}
