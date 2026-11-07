//! Citations actor — detects citable web sources in tool traffic and
//! flushes them once per finished turn.
//!
//! Port of the retired `url-citations` component's lifecycle: subscribes
//! to [`ToolCallReceived`] / [`ToolExecutionCompleted`] / [`StreamCompleted`]
//! and detects citations by shape (see [`crate::detect`]) — URLs in
//! tool-call arguments, `{url/link, title}` objects in successful
//! result JSON, and the `web-search` DuckDuckGo carve-out. Detections
//! accumulate in a per-session buffer, deduplicated by URL, and flush
//! as one [`CitationsReceived`] when the turn finishes (`Finished`);
//! aborted turns retain the buffer so a later successful turn still
//! surfaces the sources.
//!
//! Ordering note (inherited from the retired component): the fabric delivers
//! events per-publishing-actor in order, and the tool orchestrator
//! publishes `ToolCallReceived`/`ToolExecutionCompleted` along the tool
//! loop ahead of the stream's terminal `StreamCompleted` — so a flush
//! always lands within the correct turn.
//!
//! Kernel dependency (see Cargo.toml): publishes through `Services`'
//! bus, granted at slice activation.

use std::collections::HashMap;

use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::SessionId;
use jinn_core_types::UrlCitation;
use jinn_inference_msg::StreamCompleted;
use jinn_inference_msg::StreamCompletedReason;
use jinn_kernel::Services;
use jinn_session_history_msg::CitationsReceived;
use jinn_tools_msg::ToolCallReceived;
use jinn_tools_msg::ToolExecutionCompleted;

use crate::detect;

/// The citations actor's static trouper path.
pub const CITATIONS_PATH: &str = "url-citations";

/// Dependencies for [`CitationsActor`].
#[derive(Clone)]
pub struct CitationsActorDeps {
    /// Application-wide runtime services (bus publish).
    pub services: Services,
}

/// The citations actor.
pub struct CitationsActor {
    services: Services,
    /// Call-rule candidates keyed by `tool_call_id`, awaiting their result.
    pending: HashMap<jinn_core_types::ToolCallId, Vec<UrlCitation>>,
    /// Confirmed citations per session, deduplicated by URL, in order.
    buffer: HashMap<SessionId, Vec<UrlCitation>>,
}

impl ServiceActor for CitationsActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the deps via `init_with`
        // (Services carries typed handles that cannot ride JSON args).
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("CitationsActor is spawned via init_with"),
        )
    }
}

impl CitationsActor {
    /// Spawns the actor at its static trouper path and returns the path.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    pub fn spawn(system: &ActorSystem, deps: CitationsActorDeps) -> ActorPath {
        let path = ActorPath::new(CITATIONS_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move {
                        Ok(Self {
                            services: deps.services,
                            pending: HashMap::new(),
                            buffer: HashMap::new(),
                        })
                    })
                }
            })
            .handles::<ToolCallReceived>()
            .handles::<ToolExecutionCompleted>()
            .handles::<StreamCompleted>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }

    /// A citations actor over an explicit `Services` (test seam).
    #[must_use]
    pub fn with_services(services: Services) -> Self {
        Self {
            services,
            pending: HashMap::new(),
            buffer: HashMap::new(),
        }
    }

    /// Stashes call-rule candidates (and the `web-search` carve-out) for a
    /// tool call, keyed by its id.
    pub fn on_tool_call(&mut self, session_id: &SessionId, tool_call: &jinn_core_types::ToolCall) {
        let mut candidates = Vec::new();
        if tool_call.name == "web-search"
            && let Some(citation) = detect::ddg_citation(&tool_call.arguments)
        {
            candidates.push(citation);
        }
        for url in detect::urls_from_call_args(&tool_call.arguments) {
            candidates.push(UrlCitation {
                url,
                title: String::new(),
                content: None,
                start_index: None,
                end_index: None,
            });
        }
        if !candidates.is_empty() {
            self.pending.insert(tool_call.id.clone(), candidates);
        }
        let _ = session_id;
    }

    /// Promotes pending candidates on success, extracts result-rule
    /// citations, and stashes both into the session buffer (deduped).
    pub fn on_tool_result(&mut self, session_id: &SessionId, result: &jinn_core_types::ToolResult) {
        if !result.success {
            // A failed call is not citable — drop its candidates.
            self.pending.remove(&result.tool_call_id);
            return;
        }
        let mut citations = self
            .pending
            .remove(&result.tool_call_id)
            .unwrap_or_default();
        // A dumped result carries only a pointer stub in `content`; the
        // original — with the URLs — survives in `full_content`.
        let source = result.full_content.as_ref().unwrap_or(&result.content);
        citations.extend(detect::citations_from_result_content(source));
        self.record(session_id, citations);
    }

    /// Flushes the session's buffered citations as one
    /// [`CitationsReceived`] on a genuine final answer; retains them
    /// otherwise (a later successful turn still surfaces the sources).
    /// An empty drained buffer publishes nothing.
    pub async fn on_turn_end(&mut self, session_id: &SessionId, reason: StreamCompletedReason) {
        if reason != StreamCompletedReason::Finished {
            return;
        }
        let Some(citations) = self.buffer.remove(session_id) else {
            return;
        };
        if citations.is_empty() {
            return;
        }
        self.services
            .bus
            .publish(CitationsReceived {
                session_id: session_id.clone(),
                citations,
            })
            .await;
    }

    /// Appends citations to the session buffer, deduplicating by URL.
    fn record(&mut self, session_id: &SessionId, citations: Vec<UrlCitation>) {
        let buffered = self.buffer.entry(session_id.clone()).or_default();
        for citation in citations {
            // The call rule stashes URLs with an empty title; a later
            // same-URL citation with a real title wins.
            if let Some(existing) = buffered.iter_mut().find(|c| c.url == citation.url) {
                if existing.title.is_empty() {
                    *existing = citation;
                }
                continue;
            }
            buffered.push(citation);
        }
    }
}

impl MsgHandler<ToolCallReceived> for CitationsActor {
    async fn handle(&mut self, msg: &ToolCallReceived, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_call(&msg.session_id, &msg.tool_call);
    }
}

impl MsgHandler<ToolExecutionCompleted> for CitationsActor {
    async fn handle(&mut self, msg: &ToolExecutionCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_result(&msg.session_id, &msg.result);
    }
}

impl MsgHandler<StreamCompleted> for CitationsActor {
    async fn handle(&mut self, msg: &StreamCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_turn_end(&msg.session_id, msg.reason).await;
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]

    use jinn_testutil::bus_harness::TestHarness;
    use jinn_testutil::bus_harness::await_recorded;
    use std::time::Duration;

    use super::*;

    /// The actor wired to the harness's recording bus: flush publishes
    /// flow through the real fabric so `CitationsReceived` is observable
    /// via a recorder (the delivery path under test).
    async fn actor(harness: &TestHarness) -> CitationsActor {
        CitationsActor::with_services(
            jinn_kernel::common::services::Services::new_fake_with_bus(harness.bus()).await,
        )
    }

    /// A `ToolCall` factory.
    fn call(id: &str, name: &str, arguments: &str) -> jinn_core_types::ToolCall {
        jinn_core_types::ToolCall {
            id: id.to_owned().into(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        }
    }

    /// A `ToolResult` factory.
    fn result(id: &str, content: &str, success: bool) -> jinn_core_types::ToolResult {
        jinn_core_types::ToolResult {
            tool_call_id: id.to_owned().into(),
            name: "web-fetch".to_owned(),
            content: content.to_owned(),
            success,
            full_content: None,
            truncation: None,
            pin_position: None,
            attachments: Vec::new(),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn call_rule_candidates_promote_on_success() {
        // Given a recorder and a builtin web-fetch call whose result succeeded.
        let session = SessionId::new();
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<CitationsReceived>().await;
        let mut state = actor(&harness).await;
        state.on_tool_call(
            &session,
            &call("c1", "web-fetch", r#"{"url":"https://example.com"}"#),
        );
        state.on_tool_result(&session, &result("c1", "# page markdown", true));

        // When the turn finishes.
        state
            .on_turn_end(&session, StreamCompletedReason::Finished)
            .await;

        // Then the flush carries the call-rule citation.
        let flushed = await_recorded(&recorder, 1, Duration::from_secs(5)).await;
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].citations.len(), 1);
        assert_eq!(flushed[0].citations[0].url, "https://example.com");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn call_rule_candidates_discarded_on_failure() {
        // Given a recorder and a web-fetch call whose result failed.
        let session = SessionId::new();
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<CitationsReceived>().await;
        let mut state = actor(&harness).await;
        state.on_tool_call(
            &session,
            &call("c1", "web-fetch", r#"{"url":"https://example.com"}"#),
        );
        state.on_tool_result(&session, &result("c1", "fetch failed", false));

        // When the turn finishes.
        state
            .on_turn_end(&session, StreamCompletedReason::Finished)
            .await;

        // Then nothing is flushed — a failed call is not citable.
        let flushed = await_recorded(&recorder, 1, Duration::from_millis(200)).await;
        assert!(flushed.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn result_rule_yields_titled_citations_with_snippets() {
        // Given a parallel web_search call and its JSON result.
        let session = SessionId::new();
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<CitationsReceived>().await;
        let mut state = actor(&harness).await;
        let content = r#"{"search_id":"x","results":[{"url":"https://doc.rust-lang.org","title":"The Rust Book","publish_date":null,"excerpts":["Learn Rust."]}]}"#;
        state.on_tool_result(&session, &result("c1", content, true));

        // When the turn finishes.
        state
            .on_turn_end(&session, StreamCompletedReason::Finished)
            .await;

        // Then the flushed citation is the result-rule entry with its
        // excerpt as content.
        let flushed = await_recorded(&recorder, 1, Duration::from_secs(5)).await;
        let citation = &flushed[0].citations[0];
        assert_eq!(citation.url, "https://doc.rust-lang.org");
        assert_eq!(citation.title, "The Rust Book");
        assert_eq!(citation.content.as_deref(), Some("Learn Rust."));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dumped_result_yields_citations_from_full_content() {
        // Given a dumped web_search result: a pointer stub in content, the
        // original JSON in full_content.
        let session = SessionId::new();
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<CitationsReceived>().await;
        let mut state = actor(&harness).await;
        let mut dumped = result("c1", "[Tool result written to page.md (210B).]", true);
        dumped.full_content = Some(
            r#"{"search_id":"x","results":[{"url":"https://doc.rust-lang.org","title":"The Rust Book","publish_date":null,"excerpts":["Learn Rust."]}]}"#.to_owned(),
        );

        // When handling the dumped result and finishing the turn.
        state.on_tool_result(&session, &dumped);
        state
            .on_turn_end(&session, StreamCompletedReason::Finished)
            .await;

        // Then the citation is extracted from the original, not the stub.
        let flushed = await_recorded(&recorder, 1, Duration::from_secs(5)).await;
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].citations[0].url, "https://doc.rust-lang.org");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn web_search_carve_out_flushes_ddg_url() {
        // Given a `web-search` call and its plain-text result.
        let session = SessionId::new();
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<CitationsReceived>().await;
        let mut state = actor(&harness).await;
        state.on_tool_call(
            &session,
            &call("c1", "web-search", r#"{"query":"rust async"}"#),
        );
        state.on_tool_result(&session, &result("c1", "1. Title — url\n snippet", true));

        // When the turn finishes.
        state
            .on_turn_end(&session, StreamCompletedReason::Finished)
            .await;

        // Then the flush carries the DDG re-run URL with the encoded query.
        let flushed = await_recorded(&recorder, 1, Duration::from_secs(5)).await;
        assert!(
            flushed[0]
                .citations
                .iter()
                .any(|c| c.url == "https://duckduckgo.com/?q=rust+async")
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn buffer_dedups_by_url_across_rules() {
        // Given a parallel web_search citing a URL, then a web_fetch of it.
        let session = SessionId::new();
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<CitationsReceived>().await;
        let mut state = actor(&harness).await;
        let content =
            r#"{"results":[{"url":"https://same.example","title":"Titled","excerpts":["e"]}]}"#;
        state.on_tool_result(&session, &result("c1", content, true));
        state.on_tool_call(
            &session,
            &call("c2", "web-fetch", r#"{"urls":["https://same.example"]}"#),
        );
        state.on_tool_result(&session, &result("c2", "{}", true));

        // When the turn ends.
        state
            .on_turn_end(&session, StreamCompletedReason::Finished)
            .await;

        // Then the URL appears once, keeping the titled entry.
        let flushed = await_recorded(&recorder, 1, Duration::from_secs(5)).await;
        let matches: Vec<_> = flushed[0]
            .citations
            .iter()
            .filter(|c| c.url == "https://same.example")
            .collect();
        assert_eq!(matches.len(), 1, "deduped by URL");
        assert_eq!(matches[0].title, "Titled", "titled entry wins");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn zai_turn_buffers_and_flushes_deduped_citations() {
        // Given a Z.ai-shaped doubly-encoded result with a duplicate URL.
        let session = SessionId::new();
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<CitationsReceived>().await;
        let mut state = actor(&harness).await;
        let entries = r#"{"title":"Mega Man Legends (series) - MMKB - Fandom","link":"https://megaman.fandom.com/wiki/Mega_Man_Legends_(series)","content":"It is centered around MegaMan Volnutt.","refer":"ref_1"},{"title":"Mega Man Legends","link":"https://en.wikipedia.org/wiki/Mega_Man_Legends","content":"The player controls Mega Man Volnutt.","refer":"ref_2"},{"title":"Same Page Again","link":"https://megaman.fandom.com/wiki/Mega_Man_Legends_(series)","content":"duplicate url","refer":"ref_3"}"#;
        let escaped = entries.replace('"', "\\\"");
        let content = format!("\"[{escaped}]\"");
        state.on_tool_result(&session, &result("c1", &content, true));

        // When the turn reaches a final answer.
        state
            .on_turn_end(&session, StreamCompletedReason::Finished)
            .await;

        // Then the flushed citations carry each unique source in payload
        // order with title + snippet, the duplicate URL appearing once.
        let flushed = await_recorded(&recorder, 1, Duration::from_secs(5)).await;
        assert_eq!(flushed[0].citations.len(), 2);
        assert_eq!(
            flushed[0].citations[0].url,
            "https://megaman.fandom.com/wiki/Mega_Man_Legends_(series)"
        );
        assert_eq!(
            flushed[0].citations[0].content.as_deref(),
            Some("It is centered around MegaMan Volnutt.")
        );
        assert_eq!(
            flushed[0].citations[1].url,
            "https://en.wikipedia.org/wiki/Mega_Man_Legends"
        );
    }

    #[rstest::rstest]
    #[case(StreamCompletedReason::Error)]
    #[case(StreamCompletedReason::Canceled)]
    #[case(StreamCompletedReason::ToolUse)]
    #[tokio::test]
    async fn non_finished_turn_end_retains_buffer_for_next_turn(
        #[case] reason: StreamCompletedReason,
    ) {
        // Given a buffered citation and a non-final turn end.
        let session = SessionId::new();
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<CitationsReceived>().await;
        let mut state = actor(&harness).await;
        state.on_tool_call(
            &session,
            &call("c1", "web-fetch", r#"{"url":"https://example.com"}"#),
        );
        state.on_tool_result(&session, &result("c1", "ok", true));

        // When the turn ends without a final answer.
        state.on_turn_end(&session, reason).await;

        // Then the next successful turn still flushes the citation.
        state
            .on_turn_end(&session, StreamCompletedReason::Finished)
            .await;
        let flushed = await_recorded(&recorder, 1, Duration::from_secs(5)).await;
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].citations[0].url, "https://example.com");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn flush_clears_the_session_buffer() {
        // Given a flushed citation.
        let session = SessionId::new();
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<CitationsReceived>().await;
        let mut state = actor(&harness).await;
        state.on_tool_call(
            &session,
            &call("c1", "web-fetch", r#"{"url":"https://example.com"}"#),
        );
        state.on_tool_result(&session, &result("c1", "ok", true));
        state
            .on_turn_end(&session, StreamCompletedReason::Finished)
            .await;
        let first = await_recorded(&recorder, 1, Duration::from_secs(5)).await;
        assert_eq!(first.len(), 1);

        // When the next turn ends successfully with no new citations.
        state
            .on_turn_end(&session, StreamCompletedReason::Finished)
            .await;

        // Then nothing flushes (the buffer was cleared).
        let second = await_recorded(&recorder, 1, Duration::from_millis(200)).await;
        assert!(second.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sessions_are_isolated() {
        // Given citations buffered for two sessions.
        let a = SessionId::new();
        let b = SessionId::new();
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<CitationsReceived>().await;
        let mut state = actor(&harness).await;
        state.on_tool_call(
            &a,
            &call("c1", "web-fetch", r#"{"url":"https://a.example"}"#),
        );
        state.on_tool_call(
            &b,
            &call("c2", "web-fetch", r#"{"url":"https://b.example"}"#),
        );
        state.on_tool_result(&a, &result("c1", "ok", true));
        state.on_tool_result(&b, &result("c2", "ok", true));

        // When session A's turn ends.
        state.on_turn_end(&a, StreamCompletedReason::Finished).await;
        let flushed_a = await_recorded(&recorder, 1, Duration::from_secs(5)).await;

        // Then only A's citation flushed; B's is retained.
        assert_eq!(flushed_a.len(), 1);
        assert_eq!(flushed_a[0].session_id, a);
        assert_eq!(flushed_a[0].citations[0].url, "https://a.example");

        // And when B's turn ends, its citation flushes.
        state.on_turn_end(&b, StreamCompletedReason::Finished).await;
        let flushed_b = await_recorded(&recorder, 2, Duration::from_secs(5)).await;
        let b_flush = flushed_b.last().expect("B flush");
        assert_eq!(b_flush.session_id, b);
        assert_eq!(b_flush.citations[0].url, "https://b.example");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn empty_buffer_flush_pushes_nothing() {
        // Given no buffered citations.
        let session = SessionId::new();
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<CitationsReceived>().await;
        let mut state = actor(&harness).await;

        // When a final-answer turn ends.
        state
            .on_turn_end(&session, StreamCompletedReason::Finished)
            .await;

        // Then no flush payload is produced.
        let flushed = await_recorded(&recorder, 1, Duration::from_millis(200)).await;
        assert!(flushed.is_empty());
    }
}
