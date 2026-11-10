//! End-to-end tests for the builtin overgraph MCP server.
//!
//! Spawns the real `jinn` binary as `jinn mcp overgraph --db <dir>`
//! and drives it with the real [`McpClient`] stdio transport — the exact
//! path production uses when the `[mcp.overgraph]` entry enables. Asserts
//! the tools are advertised over the wire and that graph data persists
//! across child processes on the same database directory.

use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

use jinn_mcp::client::{McpClient, ServerCommand};
use rmcp::model::ContentBlock;
use serde_json::json;

/// Resolves the built `jinn` binary for integration tests.
#[expect(
    clippy::disallowed_types,
    reason = "cargo's CARGO_BIN_EXE plumbing, consumed only as a std::process::Command program"
)]
fn jinn_bin() -> std::path::PathBuf {
    std::env::var_os("CARGO_BIN_EXE_jinn")
        .map(std::path::PathBuf::from)
        .expect("CARGO_BIN_EXE_jinn not set — integration test must run via cargo test")
}

/// Spawns `jinn mcp overgraph --db <dir>` and completes the MCP
/// handshake over its stdio. No `--db-path` is passed: the shipped
/// config spawns the child bare, and MCP subcommands are exempt from
/// the debug-build `--db-path` guard (the child never opens the session
/// store).
async fn connect(bin: &std::path::Path, db: &MarkedPathBuf<Absolute>) -> McpClient {
    let command = ServerCommand {
        program: bin.to_string_lossy().into_owned(),
        args: vec![
            "mcp".to_owned(),
            "overgraph".to_owned(),
            "--db".to_owned(),
            db.to_string_lossy().into_owned(),
        ],
        env: Vec::new(),
    };
    McpClient::connect(&command)
        .await
        .expect("child handshake succeeds")
}

/// Extracts a text content block from a tool result.
fn text_of(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[rstest::rstest]
#[tokio::test]
async fn served_child_advertises_the_graph_tools() {
    // Given the real jinn binary serving overgraph on a fresh db dir.
    let bin = jinn_bin();
    let db = jinn_testutil::tempdir();
    let mut client = connect(&bin, &db.path).await;

    // When listing tools.
    let tools = client.list_tools().await.expect("tools/list");

    // Then the six graph tools come over the wire.
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(
        names,
        [
            "graph_query",
            "graph_upsert_node",
            "graph_upsert_edge",
            "graph_vector_search",
            "graph_ingest_file",
            "graph_docs"
        ]
    );

    // And the client shuts down cleanly (child exits).
    client.shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
async fn served_child_returns_the_whole_vendored_reference() {
    // Given the real jinn binary serving overgraph on a fresh db dir.
    let bin = jinn_bin();
    let db = jinn_testutil::tempdir();
    let mut client = connect(&bin, &db.path).await;

    // When calling the docs tool with no arguments.
    let docs_result = client
        .call_tool(
            "graph_docs",
            Some(json!({}).as_object().expect("obj").clone()),
        )
        .await
        .expect("docs call succeeds");

    // Then the whole vendored reference comes back, untruncated.
    assert!(
        !docs_result.is_error.unwrap_or(false),
        "{}",
        text_of(&docs_result)
    );
    let docs_text = text_of(&docs_result);
    assert!(
        docs_text.contains("elementKey"),
        "GQL reference served: {docs_text}"
    );

    // And the client shuts down cleanly (child exits).
    client.shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
async fn served_child_upserts_a_node() {
    // Given the real jinn binary serving overgraph on a fresh db dir.
    let bin = jinn_bin();
    let db = jinn_testutil::tempdir();
    let mut client = connect(&bin, &db.path).await;

    // When calling the upsert tool.
    let result = client
        .call_tool(
            "graph_upsert_node",
            Some(
                json!({"label": "Person", "key": "alice"})
                    .as_object()
                    .expect("obj")
                    .clone(),
            ),
        )
        .await
        .expect("call succeeds");

    // Then it reports success with a node id.
    assert!(!result.is_error.unwrap_or(false), "{}", text_of(&result));
    assert!(text_of(&result).contains("node_id"), "{}", text_of(&result));

    // And the client shuts down cleanly (child exits).
    client.shutdown().await;
}

#[rstest::rstest]
#[tokio::test]
async fn graph_data_persists_across_child_processes() {
    // Given a first child that upserted a node and exited.
    let bin = jinn_bin();
    let db = jinn_testutil::tempdir();
    {
        let mut first = connect(&bin, &db.path).await;
        first
            .call_tool(
                "graph_upsert_node",
                Some(
                    json!({"label": "Person", "key": "alice", "props": {"age": 31}})
                        .as_object()
                        .expect("obj")
                        .clone(),
                ),
            )
            .await
            .expect("upsert");
        first.shutdown().await;
    }

    // When a second child opens the same db dir and queries.
    let mut second = connect(&bin, &db.path).await;
    let result = second
        .call_tool(
            "graph_query",
            Some(
                json!({"query": "MATCH (n:Person) RETURN n"})
                    .as_object()
                    .expect("obj")
                    .clone(),
            ),
        )
        .await
        .expect("query");

    // Then the first child's node is still there.
    assert!(!result.is_error.unwrap_or(false), "{}", text_of(&result));
    let text = text_of(&result);
    assert!(text.contains("alice"), "persisted node found: {text}");
    assert!(text.contains("31"), "persisted property found: {text}");

    // And the client shuts down cleanly.
    second.shutdown().await;
}
