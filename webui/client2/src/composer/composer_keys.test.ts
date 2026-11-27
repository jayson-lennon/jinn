// The composer's decisions: which keystroke does what, whether a send has
// anything to send, and whether a pointer gesture is a tap.
//
// Each test is one rule, and each rule was inline in the component before
// it moved here — so a failure names the one key or gesture that broke.

import { describe, it, expect } from 'vitest';
import {
  composerKeyAction,
  decideSend,
  hasSendContent,
  isTap,
  TAP_SLOP_PX,
  type DraftKey,
} from './composer_keys';
import type { ImageUpload } from '../protocol/protocol';

/** A keystroke with no modifier held. */
const key = (name: string): DraftKey => ({ key: name, ctrl: false, shift: false });

/** One pending image, as the composer holds it. */
const image: ImageUpload = { name: 'a.png', data_base64: 'AAAA' };

describe('composerKeyAction', () => {
  it.each([
    ['Ctrl+Enter', { key: 'Enter', ctrl: true, shift: false }],
    ['Shift+Enter', { key: 'Enter', ctrl: false, shift: true }],
  ])('%s sends with the popup closed', (_label, press) => {
    // Given the popup is closed.
    // When pressing a send key.
    const action = composerKeyAction(press, false, 0);

    // Then it is a send.
    expect(action).toEqual({ kind: 'send' });
  });

  it.each([
    ['Ctrl+Enter', { key: 'Enter', ctrl: true, shift: false }],
    ['Shift+Enter', { key: 'Enter', ctrl: false, shift: true }],
  ])('%s sends with the popup open', (_label, press) => {
    // Given the popup is open with a row to pick.
    // When pressing a send key.
    const action = composerKeyAction(press, true, 1);

    // Then it is still a send — the popup never intercepts it.
    expect(action).toEqual({ kind: 'send' });
  });

  it('plain Enter with the popup open inserts a newline, not a pick', () => {
    // Given the popup is open with a row to pick.
    // When pressing Enter with no modifier.
    const action = composerKeyAction(key('Enter'), true, 1);

    // Then nothing is intercepted.
    expect(action).toEqual({ kind: 'none' });
  });

  it('escape closes the popup when it is open', () => {
    // Given the popup is open.
    // When pressing Escape.
    const action = composerKeyAction(key('Escape'), true, 1);

    // Then it is a close.
    expect(action).toEqual({ kind: 'close' });
  });

  it('escape does nothing when no popup is open', () => {
    // Given the popup is closed.
    // When pressing Escape.
    const action = composerKeyAction(key('Escape'), false, 0);

    // Then nothing is intercepted.
    expect(action).toEqual({ kind: 'none' });
  });

  it.each([
    ['ArrowUp', -1],
    ['ArrowDown', 1],
  ] as const)('%s moves the highlight', (name, delta) => {
    // Given the popup is open.
    // When pressing an arrow key.
    const action = composerKeyAction(key(name), true, 3);

    // Then it moves the highlight by the key's direction.
    expect(action).toEqual({ kind: 'move', delta });
  });

  it('tab picks when there is a row to pick', () => {
    // Given the popup is open with a row.
    // When pressing Tab.
    const action = composerKeyAction(key('Tab'), true, 1);

    // Then it is a pick.
    expect(action).toEqual({ kind: 'pick' });
  });

  it('tab does nothing when there is no row to pick', () => {
    // Given the popup is open but lists no row.
    // When pressing Tab.
    const action = composerKeyAction(key('Tab'), true, 0);

    // Then nothing is intercepted: focus moves on, as with no popup.
    expect(action).toEqual({ kind: 'none' });
  });

  it.each(['a', 'ArrowLeft', 'Backspace', ' '])(
    'an ordinary key (%s) does nothing',
    (name) => {
      // Given the popup is open.
      // When pressing a key the popup does not handle.
      const action = composerKeyAction(key(name), true, 1);

      // Then nothing is intercepted.
      expect(action).toEqual({ kind: 'none' });
    },
  );
});

describe('hasSendContent', () => {
  it.each([
    ['empty text and no images', '', []],
    ['whitespace-only text and no images', '   \n\t', []],
  ])('%s has nothing to send', (_label, text, images) => {
    // Given a draft and no pending images.
    // When asking whether a send has content.
    const has = hasSendContent(text, images);

    // Then it does not.
    expect(has).toBe(false);
  });

  it('text alone has something to send', () => {
    // Given a draft with text and no pending images.
    // When asking whether a send has content.
    const has = hasSendContent('hello', []);

    // Then it does.
    expect(has).toBe(true);
  });

  it('an image alone has something to send', () => {
    // Given an empty draft and one pending image.
    // When asking whether a send has content.
    const has = hasSendContent('', [image]);

    // Then it does.
    expect(has).toBe(true);
  });
});

describe('decideSend', () => {
  it.each([
    ['empty', ''],
    ['whitespace-only', '  \n'],
  ])('a draft that is %s with no image sends nothing', (_label, text) => {
    // Given a draft with nothing to send.
    // When deciding the message.
    const message = decideSend(text, [], 'Idle');

    // Then there is none.
    expect(message).toBeNull();
  });

  it('text at Idle is a send', () => {
    // Given text in the draft and an idle session.
    // When deciding the message.
    const message = decideSend('hello', [], 'Idle');

    // Then it is a send carrying the text.
    expect(message).toEqual({ type: 'send', text: 'hello', images: [] });
  });

  it.each(['Streaming', 'Working', 'Cancelling'] as const)(
    'text while %s is a queue',
    (phase) => {
      // Given text in the draft and a session running a turn.
      // When deciding the message.
      const message = decideSend('hello', [], phase);

      // Then it is a queue, not a send.
      expect(message).toEqual({ type: 'queue', text: 'hello', images: [] });
    },
  );

  it('an image with no text is a send', () => {
    // Given an empty draft and one pending image.
    // When deciding the message.
    const message = decideSend('', [image], 'Idle');

    // Then it is a send carrying the image.
    expect(message).toEqual({ type: 'send', text: '', images: [image] });
  });

  it('the message carries a copy of the images, not the composer array', () => {
    // Given a draft with one pending image.
    const chips = [image];

    // When deciding the message.
    const message = decideSend('', chips, 'Idle');

    // Then the message's images are a distinct array with the same content.
    expect(message?.images).toEqual(chips);
    expect(message?.images).not.toBe(chips);
  });
});

describe('isTap', () => {
  it('a pointer that does not move is a tap', () => {
    // Given a pointer that came up where it went down.
    // When asking whether it is a tap.
    const tap = isTap(100, 100, 100, 100);

    // Then it is.
    expect(tap).toBe(true);
  });

  it('a pointer that moves within the slop is a tap', () => {
    // Given a pointer that came up 9px from where it went down.
    // When asking whether it is a tap.
    const tap = isTap(100, 100, 109, 100);

    // Then it is.
    expect(tap).toBe(true);
  });

  it('a pointer that moves past the slop is not a tap', () => {
    // Given a pointer that came up 11px from where it went down.
    // When asking whether it is a tap.
    const tap = isTap(100, 100, 111, 100);

    // Then it is not: it was a scroll.
    expect(tap).toBe(false);
  });

  it('a pointer that moves exactly the slop is a tap', () => {
    // Given a slop of 10, a 6/8 diagonal moves exactly 10.
    // When asking whether it is a tap.
    const tap = isTap(0, 0, 6, 8);

    // Then it is: the slop is inclusive.
    expect(tap).toBe(true);
    expect(TAP_SLOP_PX).toBe(10);
  });
});
