// The jinn-authored tag blocks a transcript entry can carry, split off its
// text before the page renders it.
//
// Two shapes reach the page as plain text inside an entry, and the terminal
// gives each a dedicated formatter rather than showing the tags: the
// `<canvas-metrics>` block the enqueue path prepends to a user message, and
// the `<skill>` XML a loaded skill is pinned as. The page splits them here,
// with the terminal's own rule — a block claims only when it leads the text
// and closes — so a user who merely types a tag is never claimed and the two
// surfaces cannot disagree about what a block is.

/** The opening tag of the canvas-metrics block, as enqueue writes it. */
const CANVAS_METRICS_OPEN = '<canvas-metrics';
/** The closing tag of the canvas-metrics block. */
const CANVAS_METRICS_CLOSE = '</canvas-metrics>';

/** A canvas-metrics block, split off a user entry's display text. */
export interface CanvasMetrics {
  /** The artifact basename the tag names, or '' when the tag carries none. */
  artifact: string;
  /** How many events the block lists — one per non-empty body line. */
  events: number;
  /** The numbered event lines, without the tags. */
  lines: string[];
  /** The text after the block, with the one newline that followed it removed. */
  rest: string;
}

/**
 * The leading canvas-metrics block of `text`, with the text after it.
 *
 * Returns null when `text` does not start with the opening tag or never
 * closes it: the whole text then goes through the ordinary markdown path,
 * exactly as the terminal's formatter declines and falls through.
 */
export function splitCanvasMetrics(text: string): CanvasMetrics | null {
  if (!text.startsWith(CANVAS_METRICS_OPEN)) return null;
  const close = text.indexOf(CANVAS_METRICS_CLOSE);
  if (close === -1) return null;
  const openTag = /^<canvas-metrics\b[^>]*>/.exec(text);
  if (openTag === null) return null;
  const artifact = /\bartifact="([^"]*)"/.exec(openTag[0])?.[1] ?? '';
  const body = text.slice(openTag[0].length, close);
  const lines = body.split('\n').filter((line) => line !== '');
  const rest = text.slice(close + CANVAS_METRICS_CLOSE.length);
  return {
    artifact,
    events: lines.length,
    lines,
    rest: rest.startsWith('\n') ? rest.slice(1) : rest,
  };
}

/** `text` with a leading canvas-metrics block removed; the text itself when it leads with none. */
export function stripCanvasMetrics(text: string): string {
  return splitCanvasMetrics(text)?.rest ?? text;
}

/**
 * Whether a user entry's expanded text differs from its display text beyond a
 * leading canvas-metrics block.
 *
 * The enqueue path writes the block into both texts, so a difference that is
 * only the block made the page's "expanded" disclosure appear and repeat the
 * block. Compared with the block off each, the disclosure shows exactly when
 * a real expansion — an `@path` image token, a `#template` body — differs.
 */
export function expandedDiffers(display: string, expanded: string): boolean {
  return stripCanvasMetrics(display) !== stripCanvasMetrics(expanded);
}

/** A loaded skill's pinned content, split into its name and body. */
export interface SkillXml {
  /** The skill's name, from the opening tag's `name` attribute. */
  name: string;
  /** The skill's body, with the wrapping tag removed. */
  body: string;
}

/**
 * The `<skill name="X" …>body</skill>` content a loaded skill is pinned as,
 * split into the name and the body.
 *
 * Returns null when `content` is not that shape or names no skill — the
 * terminal's summary label reads an unparsable content the same way, and the
 * page then falls back to the raw text.
 */
export function splitSkillXml(content: string): SkillXml | null {
  const match = /^<skill\b([^>]*)>([\s\S]*?)<\/skill>$/.exec(content);
  if (match === null) return null;
  const name = /\bname="([^"]*)"/.exec(match[1] ?? '')?.[1];
  if (name === undefined || name === '') return null;
  return { name, body: (match[2] ?? '').trim() };
}
