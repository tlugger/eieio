//! `eio mcp`'s drift test: every tool DAEMON-SPEC §9 describes still exists, names no operation
//! the daemon does not serve, and still says what the document says (`mcp.rs`'s module doc,
//! "Tool derivation").
//!
//! Mirrors `tests/openapi_surface.rs`'s shape exactly, for the same reason: it calls
//! `eio_daemon::api::openapi::Document::openapi()` in-process — the same dev-dependency, the
//! same lib-target trick, no socket, no subprocess — rather than inventing a second mechanism
//! (the plan this test implements says so explicitly: "extend or mirror that pattern; do not
//! invent a second mechanism").
//!
//! Two things are checked, and they fail for different reasons:
//! - **Existence**: every `(METHOD, path)` in `eio_cli::client::ENDPOINTS` has exactly one MCP
//!   tool mapped to it below, and no tool is mapped to a pair the client does not address.
//!   `list_nodes` has no DAEMON operation behind it (`nodes.toml` is this CLI's own file) and is
//!   named as the one exception, rather than the comparison happening to not notice it.
//! - **Wording**: each mapped tool's `Tool::description` — derived from its own doc comment,
//!   `mcp.rs`'s module doc explains how — agrees with that operation's OpenAPI
//!   `summary`/`description`, once whitespace is normalized (the two doc-comment-to-text
//!   pipelines disagree about blank lines between paragraphs; that is not the drift this test
//!   exists to catch, so it is normalized away rather than asserted on).

use std::collections::BTreeMap;

use eio_daemon::api::openapi::Document;
use utoipa::OpenApi as _;

/// One `#[tool(name = "...")]` from `mcp.rs`, and the `(METHOD, path)` from
/// `eio_cli::client::ENDPOINTS` it is this crate's answer to.
const TOOL_ENDPOINTS: &[(&str, &str, &str)] = &[
    ("node_info", "GET", "/node"),
    ("list_blocks", "GET", "/blocks"),
    ("pull_block", "POST", "/blocks/pull"),
    ("list_services", "GET", "/services"),
    ("get_service", "GET", "/services/{service}"),
    ("put_service", "PUT", "/services/{service}"),
    ("delete_service", "DELETE", "/services/{service}"),
    ("service_errors", "GET", "/services/{service}/errors"),
    ("start_service", "POST", "/services/{service}/start"),
    ("stop_service", "POST", "/services/{service}/stop"),
    ("reload_service", "POST", "/services/{service}/reload"),
    (
        "instance_state",
        "GET",
        "/services/{service}/state/{instance}",
    ),
    ("list_orphans", "GET", "/state/orphans"),
    ("reclaim_orphan", "DELETE", "/state/orphans/{namespace}"),
    ("create_tap", "POST", "/taps"),
    ("list_taps", "GET", "/taps"),
    ("delete_tap", "DELETE", "/taps/{tap}"),
    ("read_tap", "GET", "/taps/{tap}/stream"),
    ("read_logs", "GET", "/logs/stream"),
];

/// The one tool with no DAEMON operation behind it (`mcp.rs`'s module doc, "Tool derivation").
const LOCAL_ONLY_TOOLS: &[&str] = &["list_nodes"];

/// Collapses whitespace so the comparison is about words, not about which of two
/// doc-comment-to-text pipelines keeps a blank line between paragraphs.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every `(METHOD, path)` pair the live document describes, alongside its `summary` and
/// `description` joined the way a person reading both together would read them.
fn daemon_operation_docs() -> BTreeMap<(String, String), String> {
    let document = Document::openapi();
    let mut docs = BTreeMap::new();
    for (path, item) in document.paths.paths {
        for (method, operation) in [
            ("GET", &item.get),
            ("PUT", &item.put),
            ("POST", &item.post),
            ("DELETE", &item.delete),
        ] {
            if let Some(operation) = operation {
                let mut text = operation.summary.clone().unwrap_or_default();
                if let Some(description) = &operation.description {
                    text.push(' ');
                    text.push_str(description);
                }
                docs.insert((String::from(method), path.clone()), text);
            }
        }
    }
    docs
}

/// Every tool this server registers, by name, with its description.
fn tool_descriptions() -> BTreeMap<String, String> {
    eio_cli::mcp::McpServer::tool_router()
        .list_all()
        .into_iter()
        .map(|tool| {
            (
                String::from(tool.name),
                tool.description.map(String::from).unwrap_or_default(),
            )
        })
        .collect()
}

#[test]
fn every_daemon_operation_the_cli_addresses_has_exactly_one_mcp_tool() {
    let client_endpoints: std::collections::BTreeSet<(String, String)> = eio_cli::client::ENDPOINTS
        .iter()
        .map(|(method, path)| (String::from(*method), String::from(*path)))
        .collect();
    let mapped_endpoints: std::collections::BTreeSet<(String, String)> = TOOL_ENDPOINTS
        .iter()
        .map(|(_, method, path)| (String::from(*method), String::from(*path)))
        .collect();

    let unmapped: Vec<_> = client_endpoints.difference(&mapped_endpoints).collect();
    assert!(
        unmapped.is_empty(),
        "eio_cli::client::ENDPOINTS names an operation with no MCP tool mapped to it in this \
         test's TOOL_ENDPOINTS (and, if it is genuinely missing, no #[tool] in mcp.rs either): \
         {unmapped:?}"
    );

    let invented: Vec<_> = mapped_endpoints.difference(&client_endpoints).collect();
    assert!(
        invented.is_empty(),
        "this test's TOOL_ENDPOINTS names an operation eio_cli::client::ENDPOINTS does not \
         address: {invented:?}"
    );

    // No two tools claim the same daemon operation, and no tool name appears twice.
    let mut seen_endpoints = std::collections::BTreeSet::new();
    let mut seen_names = std::collections::BTreeSet::new();
    for (name, method, path) in TOOL_ENDPOINTS {
        assert!(
            seen_endpoints.insert((*method, *path)),
            "{method} {path} is mapped to more than one tool"
        );
        assert!(
            seen_names.insert(*name),
            "`{name}` is mapped more than once"
        );
    }
}

#[test]
fn every_mapped_tool_actually_exists_and_no_tool_is_unaccounted_for() {
    let tools = tool_descriptions();
    let mapped_names: std::collections::BTreeSet<&str> =
        TOOL_ENDPOINTS.iter().map(|(name, _, _)| *name).collect();
    let local_names: std::collections::BTreeSet<&str> = LOCAL_ONLY_TOOLS.iter().copied().collect();

    for name in &mapped_names {
        assert!(
            tools.contains_key(*name),
            "`{name}` is mapped to a DAEMON operation in this test but mcp.rs registers no \
             such tool"
        );
    }
    for name in tools.keys() {
        assert!(
            mapped_names.contains(name.as_str()) || local_names.contains(name.as_str()),
            "`{name}` is a registered MCP tool with no DAEMON operation mapped to it in this \
             test and it is not listed in LOCAL_ONLY_TOOLS — either map it or add it there \
             explicitly, but do not let it go unaccounted for"
        );
    }
}

#[test]
fn every_mapped_tools_description_says_what_the_daemons_own_operation_doc_says() {
    let daemon_docs = daemon_operation_docs();
    let tools = tool_descriptions();

    for (name, method, path) in TOOL_ENDPOINTS {
        let expected = daemon_docs
            .get(&(String::from(*method), String::from(*path)))
            .unwrap_or_else(|| panic!("no live daemon operation for {method} {path}"));
        let actual = tools
            .get(*name)
            .unwrap_or_else(|| panic!("no tool named `{name}`"));
        // Containment, not equality: `read_tap`/`read_logs` legitimately say more than their
        // DAEMON operation does (they are bounded; the SSE endpoint behind them is not), and
        // that is a real difference in what the tool does, not drift. What must not happen is
        // the shared part — everything the daemon's own doc comment says — going missing or
        // getting reworded on its way into this file; an addition is welcome, a rewording or an
        // omission is exactly what this test exists to catch.
        assert!(
            normalize(actual).contains(&normalize(expected)),
            "\n`{name}`'s doc comment in mcp.rs has drifted from {method} {path}'s own doc \
             comment in crates/daemon/src/api/*.rs (compared via the live OpenAPI document) — \
             the tool's description no longer contains the daemon's text verbatim:\n\
             \n  tool says:   {actual}\n  daemon says: {expected}\n"
        );
    }
}
