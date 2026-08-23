//! `eio node` — `~/.config/eieio/nodes.toml` end to end (eieio-yck.1's DESIGN).
//!
//! Every test here runs the real binary, with `XDG_CONFIG_HOME` pointed at a scratch directory
//! that starts out *not containing* `eieio/` — never at the real `$HOME`, so this suite cannot
//! touch a developer's or CI runner's actual configuration. `HOME` is also cleared, so a bug
//! that ignored `XDG_CONFIG_HOME` would fail loudly (a write to a nonexistent `$HOME/.config`)
//! rather than silently landing on the machine's real config through a fallback this test did
//! not intend to exercise.
//!
//! No test here reaches the network or a real daemon: everything exercised — `add`, `list`,
//! `remove`, `set-default` — only ever touches `nodes.toml`. The one command that would dial
//! out, `eio node info`, is exercised only far enough to prove it fails *before* dialing
//! anything, on node resolution, which is as far as a network-free test can honestly go.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Cargo's per-integration-test scratch directory, cleaned with `target/`.
fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clearing the scratch directory");
    }
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

/// Runs `eio node <args>` with `XDG_CONFIG_HOME` set to `config_home`, which need not exist yet
/// — that is exactly the case `eio node add` has to handle on a fresh machine.
///
/// `HOME` is cleared rather than left alone: `nodes.toml`'s resolution falls back to
/// `$HOME/.config` only when `XDG_CONFIG_HOME` is unset, and a real `$HOME` still present in
/// the child's environment would make a resolution bug land on the real machine instead of
/// failing the test.
fn eio_node(config_home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_eio"))
        .arg("node")
        .args(args)
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("HOME")
        .output()
        .expect("eio runs")
}

fn ok(config_home: &Path, args: &[&str]) -> String {
    let output = eio_node(config_home, args);
    assert!(
        output.status.success(),
        "eio node {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

fn refused(config_home: &Path, args: &[&str]) -> String {
    let output = eio_node(config_home, args);
    assert!(
        !output.status.success(),
        "eio node {args:?} was supposed to fail, and printed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stderr).expect("utf-8 stderr")
}

const TOKEN: &str = "s3cr3t-token-do-not-print-me";

#[test]
fn add_creates_a_0600_file_in_a_directory_that_did_not_exist() {
    let config_home = scratch("add-fresh");
    let nodes_toml = config_home.join("eieio").join("nodes.toml");
    assert!(!nodes_toml.exists(), "the fixture starts with nothing");

    ok(
        &config_home,
        &[
            "add",
            "pi-kitchen",
            "--addr",
            "http://10.0.0.5:7777",
            "--token",
            TOKEN,
            "--default",
        ],
    );

    assert!(nodes_toml.is_file(), "nodes.toml was not created");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&nodes_toml)
            .expect("nodes.toml's metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "nodes.toml must be 0600, was {mode:o}");
    }

    let text = std::fs::read_to_string(&nodes_toml).expect("reading nodes.toml");
    assert!(text.contains("pi-kitchen"), "{text}");
    assert!(text.contains("http://10.0.0.5:7777"), "{text}");
    assert!(
        text.contains(TOKEN),
        "the file itself does carry the token: {text}"
    );
    assert!(text.contains("default"), "{text}");
}

#[test]
fn list_never_prints_the_token() {
    let config_home = scratch("list-redacts");
    ok(
        &config_home,
        &[
            "add",
            "pi-kitchen",
            "--addr",
            "http://10.0.0.5:7777",
            "--token",
            TOKEN,
        ],
    );
    let listing = ok(&config_home, &["list"]);
    assert!(listing.contains("pi-kitchen"), "{listing}");
    assert!(listing.contains("http://10.0.0.5:7777"), "{listing}");
    assert!(
        !listing.contains(TOKEN),
        "eio node list printed the bearer token: {listing}"
    );
    // It should still say *whether* a token is set, just never say what it is.
    assert!(listing.contains("token set"), "{listing}");
}

#[test]
fn a_node_named_with_no_token_is_reported_as_such_and_still_never_leaks_one() {
    let config_home = scratch("no-token");
    ok(
        &config_home,
        &["add", "pi-garage", "--addr", "http://10.0.0.9:7777"],
    );
    let listing = ok(&config_home, &["list"]);
    assert!(listing.contains("no token"), "{listing}");
}

#[test]
fn set_default_and_remove_round_trip() {
    let config_home = scratch("default-remove");
    ok(
        &config_home,
        &["add", "pi-kitchen", "--addr", "http://10.0.0.5:7777"],
    );
    ok(
        &config_home,
        &["add", "pi-garage", "--addr", "http://10.0.0.9:7777"],
    );
    ok(&config_home, &["set-default", "pi-garage"]);
    let listing = ok(&config_home, &["list"]);
    let garage_line = listing
        .lines()
        .find(|line| line.contains("pi-garage"))
        .expect("pi-garage's line");
    assert!(garage_line.starts_with('*'), "{listing}");

    // Removing the default clears it rather than leaving a dangling name (eieio-yck.1's DESIGN
    // note on `Config::remove`).
    ok(&config_home, &["remove", "pi-garage"]);
    let after = ok(&config_home, &["list"]);
    assert!(!after.contains("pi-garage"), "{after}");
    let refusal = refused(&config_home, &["set-default", "pi-garage"]);
    assert!(refusal.contains("pi-kitchen"), "{refusal}");
}

#[test]
fn resolving_with_no_node_and_no_default_names_the_configured_nodes() {
    let config_home = scratch("resolve-ambiguous");
    ok(
        &config_home,
        &["add", "pi-kitchen", "--addr", "http://10.0.0.5:7777"],
    );
    ok(
        &config_home,
        &["add", "pi-garage", "--addr", "http://10.0.0.9:7777"],
    );
    // `info` fails on resolution, before it would ever dial out — no default is set, so this
    // never reaches the network.
    let refusal = refused(&config_home, &["info"]);
    assert!(refusal.contains("pi-kitchen"), "{refusal}");
    assert!(refusal.contains("pi-garage"), "{refusal}");
}

#[test]
fn resolving_on_a_machine_with_no_nodes_says_so() {
    let config_home = scratch("resolve-empty");
    let refusal = refused(&config_home, &["info"]);
    assert!(refusal.contains("no nodes configured"), "{refusal}");
    assert!(refusal.contains("eio node add"), "{refusal}");
}

#[test]
fn removing_an_unknown_node_is_refused() {
    let config_home = scratch("remove-unknown");
    ok(
        &config_home,
        &["add", "pi-kitchen", "--addr", "http://10.0.0.5:7777"],
    );
    let refusal = refused(&config_home, &["remove", "nope"]);
    assert!(refusal.contains("nope"), "{refusal}");
    assert!(refusal.contains("pi-kitchen"), "{refusal}");
}
