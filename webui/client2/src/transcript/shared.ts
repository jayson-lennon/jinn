// The readers every entry-kind view shares: how an entry's body is unwrapped
// from its envelope, and how the text of a few kinds is read.
//
// A kind file imports from here; nothing here imports from a kind file.

import type { EntryEnvelope } from '../protocol/protocol';

export const ATTACHMENT_PREFIX = '/attachment/';
export const TOOL_OUTPUT_PREFIX = '/tool-output/';

export function body(envelope: EntryEnvelope): unknown {
  const inner = envelope.entry.kind as Record<string, unknown>;
  const tag = Object.keys(inner)[0];
  return tag === undefined ? {} : inner[tag];
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
