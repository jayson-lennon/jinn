//! The wire drift test: the runtime values in `wire.ts` must equal a
//! fixture mirrored from the Rust contract (`contract-fixture.ts`).
//! The Rust side greps this file's source (`contract_tests.rs`);
//! this test checks the values the browser actually executes against
//! the fixture. Two directions, one contract.

import assert from "node:assert/strict";
import { test } from "node:test";

import * as wire from "./wire.js";

export const FIXTURE = {
  page_root_id: "page-root",
  page_session_data_attr: "data-webui-session",
  sessions_id: "sessions",
  session_row_class: "srow",
  session_id_data_attr: "data-session-id",
  transcript_wrap_id: "transcript-wrap",
  transcript_id: "transcript",
  transcript_cursor_data_attr: "data-webui-cursor",
  controls_id: "composer-controls",
  composer_id: "composer",
  input_id: "input",
  model_picker_id: "model-picker",
  busy_spinner_id: "busy-spinner",
  action_data_attr: "data-webui-action",
  action_send: "send",
  action_cancel: "cancel",
  action_model: "model",
  sse_events: {
    chat: "chat",
    controls: "controls",
    sessions: "sessions",
    transcript_full: "transcript-full",
  },
} as const;


test("every contract constant equals the rust fixture", () => {
  // Given the fixture mirrored from the Rust contract module.
  const rust = FIXTURE;

  // When reading the TypeScript contract's runtime values.
  // Then every value matches, name for name.
  assert.equal(wire.PAGE_ROOT_ID, rust.page_root_id, "page root id");
  assert.equal(wire.PAGE_SESSION_DATA_ATTR, rust.page_session_data_attr, "page session attr");
  assert.equal(wire.SESSIONS_ID, rust.sessions_id, "sessions id");
  assert.equal(wire.SESSION_ROW_CLASS, rust.session_row_class, "row class");
  assert.equal(wire.SESSION_ID_DATA_ATTR, rust.session_id_data_attr, "row id attr");
  assert.equal(wire.TRANSCRIPT_WRAP_ID, rust.transcript_wrap_id, "transcript wrap id");
  assert.equal(wire.TRANSCRIPT_ID, rust.transcript_id, "transcript id");
  assert.equal(
    wire.TRANSCRIPT_CURSOR_DATA_ATTR,
    rust.transcript_cursor_data_attr,
    "cursor attr",
  );
  assert.equal(wire.CONTROLS_ID, rust.controls_id, "controls id");
  assert.equal(wire.COMPOSER_ID, rust.composer_id, "composer id");
  assert.equal(wire.INPUT_ID, rust.input_id, "input id");
  assert.equal(wire.MODEL_PICKER_ID, rust.model_picker_id, "model picker id");
  assert.equal(wire.BUSY_SPINNER_ID, rust.busy_spinner_id, "busy spinner id");
  assert.equal(wire.ACTION_DATA_ATTR, rust.action_data_attr, "action attr");
  assert.equal(wire.ACTION_SEND, rust.action_send, "send action");
  assert.equal(wire.ACTION_CANCEL, rust.action_cancel, "cancel action");
  assert.equal(wire.ACTION_MODEL, rust.action_model, "model action");
});

test("the sse event names equal the rust sse_kind fixture", () => {
  // Given the fixture's SSE section, mirrored from SseKind::event_name.
  const rust = FIXTURE.sse_events;

  // When reading the client's dispatch table.
  // Then each event name matches its Rust counterpart exactly.
  assert.equal(wire.SSE_EVENTS.chat, rust.chat, "chat event");
  assert.equal(wire.SSE_EVENTS.controls, rust.controls, "controls event");
  assert.equal(wire.SSE_EVENTS.sessions, rust.sessions, "sessions event");
  assert.equal(
    wire.SSE_EVENTS.transcriptFull,
    rust.transcript_full,
    "transcript-full event",
  );
});
