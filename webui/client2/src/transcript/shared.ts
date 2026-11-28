// The readers every entry-kind view shares: how an entry's body is unwrapped
// from its envelope, and how the text of a few kinds is read.
//
// A kind file imports from here; nothing here imports from a kind file.

import type { EntryEnvelope, EntryKind, EntryKindBody, EntryKindMap } from '../protocol/protocol';

export const ATTACHMENT_PREFIX = '/attachment/';
export const TOOL_OUTPUT_PREFIX = '/tool-output/';

/** The PascalCase tag the persisted map carries for a kind name. */
const TAG: { [K in EntryKind]: keyof EntryKindMap[K] } = {
  user: 'User',
  system: 'System',
  error: 'Error',
  assistant: 'Assistant',
  actor: 'Actor',
  thinking: 'Thinking',
  tool_call: 'ToolCall',
  tool_result: 'ToolResult',
  transient: 'Transient',
  compaction: 'Compaction',
  annotation: 'Annotation',
  rule_interrupt: 'RuleInterrupt',
  intersession: 'Intersession',
};

/**
 * An entry's body: the text itself for a string variant (`Assistant`), the
 * struct for a struct variant (`ToolCall`).
 *
 * Typed by the envelope's own kind name, which is the one the page
 * dispatches on: the persisted map is read under that name's tag, so a map
 * whose tag disagrees with its envelope reads as undefined rather than as a
 * body of the wrong kind. The result is typed, so a consumer that reads
 * `text` off an `Assistant` — a string — does not compile.
 */
export function body<K extends EntryKind>(envelope: EntryEnvelope<K>): EntryKindBody<K> {
  const tag = TAG[envelope.kind];
  return (envelope.entry.kind as Record<string, unknown>)[tag] as EntryKindBody<K>;
}

export function str(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

export function opt(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

/** A tool call's arguments, pretty-printed when they are valid JSON. */
export function prettyArgs(raw: string): string {
  const trimmed = raw.trim();
  if (trimmed === '') return '';
  try {
    return JSON.stringify(JSON.parse(trimmed), null, 2);
  } catch {
    return raw;
  }
}


export function citationTitle(citation: { url?: string; title?: string }): string {
  return citation.title || '(untitled)';
}

/**
 * The interrupt body with its prompt wrapper taken off.
 *
 * A rule interrupt's body is the text the model reads, wrapped in a
 * `<system-interrupt>` tag whose attributes name the reason and the rule.
 * The wrapper is prompt machinery: the page shows the rule as the block's
 * header and the reason as a label, so neither is left in the prose. A body
 * without the wrapper is returned as it is, with no reason.
 */
export function parseInterruptBody(body: string): { text: string; reason: string | null } {
  const match = /^\s*<system-interrupt\b([^>]*)>([\s\S]*?)<\/system-interrupt>\s*$/i.exec(body);
  if (match === null) return { text: body, reason: null };
  const attrs = match[1] ?? '';
  const reason = /\breason="([^"]*)"/i.exec(attrs)?.[1] ?? null;
  return { text: (match[2] ?? '').trim(), reason };
}

/**
 * Guidance the harness injected mid-turn, as a warning-tinted block.
 *
 * Collapsed to its header by default: an interrupt explains why a turn
 * changed course, which is worth a line in the transcript but not the whole
 * message until the reader asks for it. The header names the rule that
 * fired, the label gives the reason, and the body is the guidance itself.
 */
