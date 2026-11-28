import { describe, it, expect } from 'vitest';
import { streamDebugEnabled } from './debug_stream';

// The switch for the page's stream trace. Only the switch is tested: the
// lines it lets through are console output, which is the trace's whole
// purpose and has no behaviour to assert beyond being written or not.

describe('the stream trace switch', () => {
  it('is on when the page is opened with ?debug=stream', () => {
    // Given a page URL carrying the flag.
    // When asking whether the trace is on.
    const on = streamDebugEnabled('?debug=stream');

    // Then it is.
    expect(on).toBe(true);
  });

  it('is off for any other value of debug', () => {
    // Given a page URL whose debug flag names some other trace.
    // When asking whether the stream trace is on.
    const on = streamDebugEnabled('?debug=unread');

    // Then it is not.
    expect(on).toBe(false);
  });

  it('is off when the page is opened with no query at all', () => {
    // Given a plain page URL.
    // When asking whether the stream trace is on.
    const on = streamDebugEnabled('');

    // Then it is not.
    expect(on).toBe(false);
  });
});
