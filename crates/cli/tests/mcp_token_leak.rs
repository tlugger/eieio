//! `eio mcp`'s auth requirement, end to end: a node token never reaches a tool's output.
//!
//! This drives the real `eio` *binary*, built with `mcp` as its subcommand, as a child process
//! over its real stdio transport (`rmcp::transport::TokioChildProcess`) — the same shape a real
//! MCP-speaking agent uses, not a call into this crate's own Rust functions. `XDG_CONFIG_HOME`
//! is pointed at a scratch `nodes.toml` the same way `tests/node_config.rs` already does, and
//! `HOME` is cleared for the same reason that file gives: a resolution bug should fail loudly
//! rather than land on a developer's or CI runner's real configuration.
//!
//! The plan this test implements says a grep of a command's output is "necessary and not
//! sufficient": also assert no `{:?}` of a token-carrying type can print it. That half lives
//! in `crates/cli/src/config.rs`'s own test module, beside the type it is about
//! (`no_debug_rendering_of_a_node_entry_can_print_its_token`) — this file is the other half,
//! proving the same guarantee holds when a token actually flows through a real tool call.

use std::path::{Path, PathBuf};

use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;

const TOKEN: &str = "s3cr3t-token-do-not-print-me";

/// Cargo's per-integration-test scratch directory, cleaned with `target/`.
fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clearing the scratch directory");
    }
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

/// A `nodes.toml` naming one node — `addr` unreachable on purpose, so every call this test
/// makes fails, and fails on the wire rather than on a real node's answer. The point is what
/// the failure says, not what it does.
fn write_nodes_toml(config_home: &Path, addr: &str) {
    let eieio = config_home.join("eieio");
    std::fs::create_dir_all(&eieio).expect("the config directory");
    std::fs::write(
        eieio.join("nodes.toml"),
        format!("[nodes.kitchen]\naddr = \"{addr}\"\ntoken = \"{TOKEN}\"\n"),
    )
    .expect("writing nodes.toml");
}

/// Spawns `eio mcp` with `XDG_CONFIG_HOME` set to `config_home` and connects a plain MCP
/// client to it over its real stdio transport.
async fn connect(config_home: &Path) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_eio"));
    command
        .arg("mcp")
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("HOME")
        .stderr(std::process::Stdio::null());
    let transport = TokioChildProcess::new(command).expect("spawning `eio mcp`");
    ().serve(transport)
        .await
        .expect("the MCP initialize handshake")
}

/// Calls `name` with `arguments` and returns whatever text the failure carries — the tool's
/// error message if the call failed as a protocol-level tool error, or a panic if it somehow
/// succeeded, since every call in this file names an unreachable node and success would mean
/// the fixture is wrong, not that the code is right.
async fn failing_call_text(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    arguments: serde_json::Value,
) -> String {
    let params = CallToolRequestParams::new(String::from(name))
        .with_arguments(arguments.as_object().expect("a JSON object").clone());
    match client.call_tool(params).await {
        Ok(result) => {
            panic!("`{name}` against an unreachable node was supposed to fail: {result:?}")
        }
        Err(error) => format!("{error:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_call_against_an_unreachable_node_never_prints_its_token() {
    let config_home = scratch("token-leak-unreachable");
    // Port 1 is a privileged port nothing listens on in a test sandbox — connection refused,
    // fast, every time, with no real node anywhere near this test.
    write_nodes_toml(&config_home, "http://127.0.0.1:1");
    let client = connect(&config_home).await;

    for (name, arguments) in [
        ("node_info", serde_json::json!({ "node": "kitchen" })),
        ("list_services", serde_json::json!({ "node": "kitchen" })),
        (
            "get_service",
            serde_json::json!({ "node": "kitchen", "service": "anything" }),
        ),
        (
            "put_service",
            serde_json::json!({
                "node": "kitchen",
                "service": "anything",
                "definition": "name = \"anything\"\n",
                "if_match": "*",
            }),
        ),
    ] {
        let text = failing_call_text(&client, name, arguments).await;
        assert!(
            !text.contains(TOKEN),
            "`{name}`'s error carried the bearer token: {text}"
        );
    }

    client.cancel().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_call_against_a_node_that_refuses_the_token_never_prints_it() {
    // `read_tap`/`read_logs` build their own connection rather than going through `Client`
    // (`mcp.rs`'s module doc explains why) and so exercise a different code path to the same
    // rule — this is the proof that path holds it too.
    let config_home = scratch("token-leak-read-tap");
    write_nodes_toml(&config_home, "http://127.0.0.1:1");
    let client = connect(&config_home).await;

    let text = failing_call_text(
        &client,
        "read_tap",
        serde_json::json!({ "node": "kitchen", "tap": "nonexistent" }),
    )
    .await;
    assert!(
        !text.contains(TOKEN),
        "`read_tap`'s error carried the bearer token: {text}"
    );

    let text = failing_call_text(
        &client,
        "read_logs",
        serde_json::json!({ "node": "kitchen" }),
    )
    .await;
    assert!(
        !text.contains(TOKEN),
        "`read_logs`'s error carried the bearer token: {text}"
    );

    client.cancel().await.ok();
}

/// `list_nodes` needs no token to fail usefully (naming an unconfigured node), and must not
/// need one to succeed usefully either: it never reports what a token *is*, only whether one
/// is configured (DAEMON §9.1, mirroring `eio node list`'s own rule).
#[tokio::test(flavor = "multi_thread")]
async fn list_nodes_reports_whether_a_token_is_configured_and_never_what_it_is() {
    let config_home = scratch("token-leak-list-nodes");
    write_nodes_toml(&config_home, "http://127.0.0.1:1");
    let client = connect(&config_home).await;

    let result = client
        .call_tool(CallToolRequestParams::new("list_nodes"))
        .await
        .expect("list_nodes needs no node argument and no network");
    let rendered = format!("{result:?}");
    assert!(rendered.contains("kitchen"), "{rendered}");
    assert!(rendered.contains("token_configured"), "{rendered}");
    assert!(
        !rendered.contains(TOKEN),
        "list_nodes printed the bearer token: {rendered}"
    );

    client.cancel().await.ok();
}
