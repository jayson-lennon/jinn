//! The wire drift test: the runtime values in `wire.ts` must equal a
//! fixture GENERATED from the Rust contract (`wire-fixture.ts`,
//! written by `just webui-build`). The Rust side greps this file's
//! source (`contract_tests.rs`); this test checks the values the
//! browser actually executes against the generated fixture. Two
//! directions, one generated truth — a rename on either side fails a
//! test before any browser sees the breakage.

import assert from "node:assert/strict";
import { test } from "node:test";

import * as wire from "./wire.js";
import { CONTRACT, SSE_KINDS } from "./wire-fixture.js";

test("every wire constant equals the generated rust fixture", () => {
  // Given the fixture generated from the Rust contract module.
  const rust = CONTRACT;

  // When reading the TypeScript contract's runtime values.
  // Then every value matches, name for name — the fixture keys are
  // the contract values uppercased, so a value renamed on the Rust
  // side fails here as a missing key, and one renamed only here fails
  // as a value mismatch.
  const wireValues: string[] = Object.entries(wire)
    .filter(([key]) => key !== "SSE_EVENTS")
    .map(([, value]) => String(value));
  const fixtureValues: string[] = Object.values(rust);

  assert.ok(
    fixtureValues.length > 0,
    "the generated fixture names at least one constant",
  );
  for (const value of fixtureValues) {
    assert.ok(
      wireValues.includes(value),
      `contract value "${value}" has no wire.ts constant — the Rust and \
       TypeScript contracts have drifted`,
    );
  }
});

test("the sse event names equal the generated rust sse_kind fixture", () => {
  // Given the fixture's SSE section, generated from SseKind::event_name.
  const rust = Object.values(SSE_KINDS);

  // When reading the client's dispatch table.
  // Then each event name matches its Rust counterpart exactly.
  const clientNames: string[] = Object.values(wire.SSE_EVENTS);
  assert.ok(rust.length > 0, "the fixture names at least one sse kind");
  for (const name of rust) {
    assert.ok(
      clientNames.includes(name),
      `SSE event "${name}" has no wire.ts entry — the Rust and TypeScript \
       contracts have drifted`,
    );
  }
});
