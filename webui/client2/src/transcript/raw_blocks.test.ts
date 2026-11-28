// The tag-block parsers: what a leading block claims, what it leaves, and
// what falls through.

import { describe, it, expect } from 'vitest';
import { expandedDiffers, splitCanvasMetrics, splitSkillXml, stripCanvasMetrics } from './raw_blocks';

/** The block as the enqueue path writes it: tag, numbered lines, tag, newline. */
const BLOCK = [
  '<canvas-metrics artifact="quiz.html">',
  '1. clicked-foo',
  '2. flavored-click {"flavor":"vanilla"}',
  '</canvas-metrics>',
].join('\n');

describe('splitCanvasMetrics', () => {
  it('a leading block yields its artifact, event lines, and the text after it', () => {
    // Given a display text that leads with the block, then the user's words.
    const text = `${BLOCK}\nmake it prettier`;

    // When splitting.
    const split = splitCanvasMetrics(text);

    // Then the artifact and the lines come off the block, and the rest is
    // the user's words with the one following newline removed.
    expect(split).toEqual({
      artifact: 'quiz.html',
      events: 2,
      lines: ['1. clicked-foo', '2. flavored-click {"flavor":"vanilla"}'],
      rest: 'make it prettier',
    });
  });

  it('a block with nothing after it leaves an empty rest', () => {
    // Given a display text that is the block alone.
    // When splitting.
    const split = splitCanvasMetrics(BLOCK);

    // Then the rest is empty.
    expect(split?.rest).toBe('');
  });

  it('a tag without an artifact attribute reads as an empty artifact', () => {
    // Given a block whose opening tag names no artifact.
    const text = '<canvas-metrics>\n1. clicked\n</canvas-metrics>\nhello';

    // When splitting.
    const split = splitCanvasMetrics(text);

    // Then the artifact is the empty string.
    expect(split?.artifact).toBe('');
  });

  it.each([
    ['a plain message', 'make it prettier'],
    ['a tag named mid-text', 'I typed <canvas-metrics> once'],
    ['an opening tag that never closes', '<canvas-metrics artifact="x.html">\n1. clicked'],
    ['a closing tag with no opening tag', '1. clicked\n</canvas-metrics>'],
  ])('declines %s', (_case, text) => {
    // Given a text that is not a leading, closed block.
    // When splitting.
    const split = splitCanvasMetrics(text);

    // Then nothing is claimed.
    expect(split).toBeNull();
  });
});

describe('stripCanvasMetrics', () => {
  it('removes a leading block and keeps the text after it', () => {
    // Given a display text that leads with the block.
    // When stripping.
    const rest = stripCanvasMetrics(`${BLOCK}\nmake it prettier`);

    // Then only the user's words remain.
    expect(rest).toBe('make it prettier');
  });

  it('returns a text with no block unchanged', () => {
    // Given a plain message.
    // When stripping.
    const rest = stripCanvasMetrics('make it prettier');

    // Then the text is itself.
    expect(rest).toBe('make it prettier');
  });
});

describe('expandedDiffers', () => {
  it('texts that differ only by the block do not differ', () => {
    // Given a display and an expanded text that both carry the block and
    // the same words.
    const display = `${BLOCK}\nmake it prettier`;
    const expanded = `${BLOCK}\nmake it prettier`;

    // When comparing.
    const differs = expandedDiffers(display, expanded);

    // Then the expanded text does not differ.
    expect(differs).toBe(false);
  });

  it('a real expansion behind the block still differs', () => {
    // Given a display with a bare @path token and an expanded text whose
    // token was expanded, both behind the same block.
    const display = `${BLOCK}\ndescribe @img.png`;
    const expanded = `${BLOCK}\ndescribe <image attached: /abs/img.png>`;

    // When comparing.
    const differs = expandedDiffers(display, expanded);

    // Then the expanded text differs.
    expect(differs).toBe(true);
  });
});

describe('splitSkillXml', () => {
  it('a skill XML yields its name and its body without the tag', () => {
    // Given the content a loaded skill is pinned as.
    const content = '<skill name="phased-task-loop" location="/x/SKILL.md">\nRun each phase in turn.\n</skill>';

    // When splitting.
    const split = splitSkillXml(content);

    // Then the name comes off the tag and the body is the inner text.
    expect(split).toEqual({ name: 'phased-task-loop', body: 'Run each phase in turn.' });
  });

  it.each([
    ['plain text', 'not a skill'],
    ['an empty name', '<skill name="" location="/x">body</skill>'],
    ['no name attribute', '<skill location="/x">body</skill>'],
    ['an opening tag that never closes', '<skill name="x">body'],
    ['text before the tag', 'loaded <skill name="x">body</skill>'],
    ['text after the tag', '<skill name="x">body</skill> and more'],
  ])('declines %s', (_case, content) => {
    // Given a content that is not a leading, closed, named skill XML.
    // When splitting.
    const split = splitSkillXml(content);

    // Then nothing is claimed.
    expect(split).toBeNull();
  });
});
