//! `eio nodes export` / `eio nodes import` — moving a set of configured nodes between two
//! `nodes.toml` files (eieio-m9s.6, `src/nodes.rs`'s module doc).
//!
//! Same posture as `tests/node_config.rs`: every test here runs the real binary with
//! `XDG_CONFIG_HOME` pointed at a scratch directory that starts out empty, `HOME` cleared, and
//! no test reaches the network or a real daemon — everything exercised only ever touches
//! `nodes.toml` and the export file this suite writes into its own scratch directory. No test
//! here points at the real `$HOME`/`~/.config/eieio`.

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

/// Runs `eio <args>` with `XDG_CONFIG_HOME` set to `config_home`, which need not exist yet.
/// `HOME` is cleared for the same reason `tests/node_config.rs` clears it: a resolution bug
/// should fail loudly on a nonexistent `$HOME/.config` rather than land on a real machine.
fn eio(config_home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_eio"))
        .args(args)
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("HOME")
        .output()
        .expect("eio runs")
}

fn ok(config_home: &Path, args: &[&str]) -> String {
    let output = eio(config_home, args);
    assert!(
        output.status.success(),
        "eio {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

fn refused(config_home: &Path, args: &[&str]) -> String {
    let output = eio(config_home, args);
    assert!(
        !output.status.success(),
        "eio {args:?} was supposed to fail, and printed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stderr).expect("utf-8 stderr")
}

const KITCHEN_TOKEN: &str = "s3cr3t-token-do-not-print-me-kitchen";
const GARAGE_TOKEN: &str = "s3cr3t-token-do-not-print-me-garage";

fn mode_of(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .expect("the export file's metadata")
            .permissions()
            .mode()
            & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0o600
    }
}

#[test]
fn round_trip_preserves_names_addresses_and_tokens() {
    let source = scratch("roundtrip-source");
    let dest = scratch("roundtrip-dest");
    let export_file = scratch("roundtrip-artifact").join("export.json");

    ok(
        &source,
        &[
            "node",
            "add",
            "pi-kitchen",
            "--addr",
            "http://10.0.0.5:7777",
            "--token",
            KITCHEN_TOKEN,
        ],
    );
    ok(
        &source,
        &[
            "node",
            "add",
            "pi-garage",
            "--addr",
            "http://10.0.0.9:7777",
            "--token",
            GARAGE_TOKEN,
        ],
    );

    let export_out = ok(
        &source,
        &[
            "nodes",
            "export",
            "--out",
            export_file.to_str().expect("utf-8 path"),
        ],
    );
    assert!(export_out.contains('2'), "{export_out}");

    ok(
        &dest,
        &["nodes", "import", export_file.to_str().expect("utf-8 path")],
    );

    // Names and addresses round-trip through `eio node list`.
    let listing = ok(&dest, &["node", "list"]);
    assert!(listing.contains("pi-kitchen"), "{listing}");
    assert!(listing.contains("http://10.0.0.5:7777"), "{listing}");
    assert!(listing.contains("pi-garage"), "{listing}");
    assert!(listing.contains("http://10.0.0.9:7777"), "{listing}");
    assert!(!listing.contains(KITCHEN_TOKEN), "{listing}");
    assert!(!listing.contains(GARAGE_TOKEN), "{listing}");

    // The tokens themselves round-trip byte for byte into the destination's `nodes.toml` — a
    // credential "still works" if and only if what is on disk is exactly what the node was
    // issued, so this is the round trip's proof for tokens the way `listing` above is for
    // names and addresses (no live daemon is dialled anywhere in this suite).
    let dest_nodes_toml =
        std::fs::read_to_string(dest.join("eieio").join("nodes.toml")).expect("dest nodes.toml");
    assert!(dest_nodes_toml.contains(KITCHEN_TOKEN), "{dest_nodes_toml}");
    assert!(dest_nodes_toml.contains(GARAGE_TOKEN), "{dest_nodes_toml}");
}

#[test]
fn export_defaults_beside_nodes_toml_never_stdout() {
    let config_home = scratch("export-default-destination");
    ok(
        &config_home,
        &[
            "node",
            "add",
            "pi-kitchen",
            "--addr",
            "http://10.0.0.5:7777",
            "--token",
            KITCHEN_TOKEN,
        ],
    );

    let stdout = ok(&config_home, &["nodes", "export"]);
    assert!(
        !stdout.contains(KITCHEN_TOKEN),
        "eio nodes export printed the bearer token to stdout: {stdout}"
    );

    let expected = config_home.join("eieio").join("nodes-export.json");
    assert!(
        expected.is_file(),
        "expected the default export at {}: {stdout}",
        expected.display()
    );
    let text = std::fs::read_to_string(&expected).expect("reading the default export");
    assert!(text.contains(KITCHEN_TOKEN), "{text}");
    assert!(text.contains("eieio.nodes/v1"), "{text}");
}

#[test]
fn export_file_is_created_0600() {
    let config_home = scratch("export-permissions");
    ok(
        &config_home,
        &[
            "node",
            "add",
            "pi-kitchen",
            "--addr",
            "http://10.0.0.5:7777",
        ],
    );
    let export_file = config_home.join("export.json");
    ok(
        &config_home,
        &[
            "nodes",
            "export",
            "--out",
            export_file.to_str().expect("utf-8 path"),
        ],
    );
    #[cfg(unix)]
    {
        let mode = mode_of(&export_file);
        assert_eq!(mode, 0o600, "the export file must be 0600, was {mode:o}");
    }
}

#[test]
fn export_never_prints_a_token_to_stdout_or_stderr() {
    let config_home = scratch("export-no-leak");
    ok(
        &config_home,
        &[
            "node",
            "add",
            "pi-kitchen",
            "--addr",
            "http://10.0.0.5:7777",
            "--token",
            KITCHEN_TOKEN,
        ],
    );
    let export_file = config_home.join("export.json");
    let output = eio(
        &config_home,
        &[
            "nodes",
            "export",
            "--out",
            export_file.to_str().expect("utf-8 path"),
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(KITCHEN_TOKEN),
        "eio nodes export printed the token on stdout: {stdout}"
    );
    assert!(
        !stderr.contains(KITCHEN_TOKEN),
        "eio nodes export printed the token on stderr: {stderr}"
    );
    // The token is genuinely in the file — this is a proof about the command's *output*, not
    // that the token went missing entirely.
    let text = std::fs::read_to_string(&export_file).expect("reading the export");
    assert!(text.contains(KITCHEN_TOKEN), "{text}");
}

#[test]
fn import_skips_a_colliding_name_without_force_and_never_overwrites_its_token() {
    let config_home = scratch("import-collision-skip");
    let old_token = "old-token-still-the-working-one";
    let new_token = "new-token-from-a-stale-export";

    ok(
        &config_home,
        &[
            "node",
            "add",
            "pi-kitchen",
            "--addr",
            "http://10.0.0.5:7777",
            "--token",
            old_token,
        ],
    );

    let export_file = config_home.join("stale-export.json");
    std::fs::write(
        &export_file,
        format!(
            r#"{{"format":"eieio.nodes/v1","nodes":[{{"name":"pi-kitchen","address":"http://10.0.0.5:7777","token":"{new_token}"}}]}}"#
        ),
    )
    .expect("writing the stale export fixture");

    let report = ok(
        &config_home,
        &["nodes", "import", export_file.to_str().expect("utf-8 path")],
    );
    assert!(report.contains("skipped"), "{report}");
    assert!(report.contains("pi-kitchen"), "{report}");
    assert!(report.contains("--force"), "{report}");

    let nodes_toml = std::fs::read_to_string(config_home.join("eieio").join("nodes.toml"))
        .expect("reading nodes.toml");
    assert!(
        nodes_toml.contains(old_token),
        "the working token must survive an unforced import: {nodes_toml}"
    );
    assert!(
        !nodes_toml.contains(new_token),
        "an unforced import must not silently overwrite a working token: {nodes_toml}"
    );
}

#[test]
fn import_with_force_overwrites_a_colliding_token() {
    let config_home = scratch("import-collision-force");
    let old_token = "old-token-being-replaced";
    let new_token = "new-token-after-reprovisioning";

    ok(
        &config_home,
        &[
            "node",
            "add",
            "pi-kitchen",
            "--addr",
            "http://10.0.0.5:7777",
            "--token",
            old_token,
        ],
    );

    let export_file = config_home.join("fresh-export.json");
    std::fs::write(
        &export_file,
        format!(
            r#"{{"format":"eieio.nodes/v1","nodes":[{{"name":"pi-kitchen","address":"http://10.0.0.5:7777","token":"{new_token}"}}]}}"#
        ),
    )
    .expect("writing the fresh export fixture");

    let report = ok(
        &config_home,
        &[
            "nodes",
            "import",
            "--force",
            export_file.to_str().expect("utf-8 path"),
        ],
    );
    assert!(report.contains("updated"), "{report}");
    assert!(report.contains("pi-kitchen"), "{report}");

    let nodes_toml = std::fs::read_to_string(config_home.join("eieio").join("nodes.toml"))
        .expect("reading nodes.toml");
    assert!(nodes_toml.contains(new_token), "{nodes_toml}");
    assert!(!nodes_toml.contains(old_token), "{nodes_toml}");
}

#[test]
fn import_adds_new_names_alongside_an_untouched_collision() {
    let config_home = scratch("import-mixed");
    let kitchen_token = "kitchen-token-untouched";

    ok(
        &config_home,
        &[
            "node",
            "add",
            "pi-kitchen",
            "--addr",
            "http://10.0.0.5:7777",
            "--token",
            kitchen_token,
        ],
    );

    let export_file = config_home.join("mixed-export.json");
    std::fs::write(
        &export_file,
        format!(
            r#"{{"format":"eieio.nodes/v1","nodes":[
                {{"name":"pi-kitchen","address":"http://10.0.0.5:7777","token":"a-different-token"}},
                {{"name":"pi-garage","address":"http://10.0.0.9:7777","token":"{GARAGE_TOKEN}"}}
            ]}}"#
        ),
    )
    .expect("writing the mixed export fixture");

    let report = ok(
        &config_home,
        &["nodes", "import", export_file.to_str().expect("utf-8 path")],
    );
    assert!(report.contains("added"), "{report}");
    assert!(report.contains("pi-garage"), "{report}");
    assert!(report.contains("skipped"), "{report}");
    assert!(report.contains("pi-kitchen"), "{report}");

    let listing = ok(&config_home, &["node", "list"]);
    assert!(listing.contains("pi-kitchen"), "{listing}");
    assert!(listing.contains("pi-garage"), "{listing}");
}

#[test]
fn import_rejects_a_file_with_an_unrecognized_format_marker() {
    let config_home = scratch("import-bad-format");
    let export_file = config_home.join("bad-export.json");
    std::fs::write(&export_file, r#"{"format":"something-else/v9","nodes":[]}"#)
        .expect("writing the malformed export fixture");

    let refusal = refused(
        &config_home,
        &["nodes", "import", export_file.to_str().expect("utf-8 path")],
    );
    assert!(
        refusal.contains("not a recognized nodes export"),
        "{refusal}"
    );
}

#[test]
fn import_of_a_nonexistent_file_is_refused_before_touching_nodes_toml() {
    let config_home = scratch("import-missing-file");
    let refusal = refused(
        &config_home,
        &["nodes", "import", "/nonexistent/export.json"],
    );
    assert!(refusal.contains("/nonexistent/export.json"), "{refusal}");
    assert!(
        !config_home.join("eieio").join("nodes.toml").exists(),
        "a failed import must not create nodes.toml"
    );
}

// ─── `class` in the export/import round trip (eieio-x7g.9) ───
//
// `ExportedNode` gained `class` (`src/nodes.rs`'s module doc, rewritten by eieio-x7g.5's
// integration fix: `nodes.toml` no longer never holds one). The four tests below are the ones
// the plan for this bead calls out as missing: that a leaf's `class` survives into the export,
// that importing it back restores an effective `leaf` (not just the text `"leaf"`), that a
// pre-`class` export — the shape a v1 export had before the field existed — still imports as
// `daemon`, and that `--force` on a name already configured as `leaf` cannot lose that class to
// an export that forgot to carry it.

/// Parses `nodes.toml` and returns the `[nodes.<name>]` table, structurally rather than by
/// scanning the file's raw text — the same choice `node_class.rs`'s `node_table` makes and for
/// the same reason: a substring search for `"class"` can't tell "absent" from "present in a
/// comment or another entry", where a parsed table can.
fn node_table(config_home: &Path, name: &str) -> toml::Value {
    let text = std::fs::read_to_string(config_home.join("eieio").join("nodes.toml"))
        .expect("reading nodes.toml");
    let doc: toml::Value = toml::from_str(&text).expect("nodes.toml parses as TOML");
    doc.get("nodes")
        .and_then(|nodes| nodes.get(name))
        .unwrap_or_else(|| panic!("no [nodes.{name}] table in:\n{text}"))
        .clone()
}

/// Parses an export file and returns the entry named `name` from its `nodes` array,
/// structurally (`serde_json::Value`) rather than by scanning the file's raw text, for the same
/// reason `node_table` above parses rather than greps.
fn exported_node(export_path: &Path, name: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(export_path).expect("reading the export file");
    let doc: serde_json::Value = serde_json::from_str(&text).expect("export parses as JSON");
    doc["nodes"]
        .as_array()
        .expect("nodes is an array")
        .iter()
        .find(|node| node["name"] == name)
        .unwrap_or_else(|| panic!("no entry named {name} in:\n{text}"))
        .clone()
}

#[test]
fn export_carries_class_for_a_leaf_entry_and_omits_it_for_a_daemon_one() {
    let config_home = scratch("export-carries-leaf-class");
    ok(
        &config_home,
        &[
            "node",
            "add",
            "porch-sensor",
            "--addr",
            "http://10.0.0.7:7777",
            "--class",
            "leaf",
        ],
    );
    ok(
        &config_home,
        &[
            "node",
            "add",
            "kitchen",
            "--addr",
            "http://10.0.0.5:7777",
            "--class",
            "daemon",
        ],
    );

    let export_file = config_home.join("export.json");
    ok(
        &config_home,
        &[
            "nodes",
            "export",
            "--out",
            export_file.to_str().expect("utf-8 path"),
        ],
    );

    let leaf = exported_node(&export_file, "porch-sensor");
    assert_eq!(
        leaf.get("class").and_then(serde_json::Value::as_str),
        Some("leaf"),
        "a leaf entry's class must be carried into the export: {leaf}"
    );

    let daemon = exported_node(&export_file, "kitchen");
    assert!(
        daemon.get("class").is_none(),
        "a daemon entry's class must be omitted from the export, just as `nodes.toml` omits \
         it: {daemon}"
    );
}

#[test]
fn import_of_an_exported_leaf_restores_an_effective_leaf_not_just_the_text() {
    let source = scratch("import-leaf-source");
    let dest = scratch("import-leaf-dest");
    let export_file = scratch("import-leaf-artifact").join("export.json");

    ok(
        &source,
        &[
            "node",
            "add",
            "porch-sensor",
            "--addr",
            "http://10.0.0.7:7777",
            "--class",
            "leaf",
        ],
    );
    ok(
        &source,
        &[
            "nodes",
            "export",
            "--out",
            export_file.to_str().expect("utf-8 path"),
        ],
    );

    ok(
        &dest,
        &["nodes", "import", export_file.to_str().expect("utf-8 path")],
    );

    // Present in the file...
    let table = node_table(&dest, "porch-sensor");
    assert_eq!(
        table.get("class").and_then(toml::Value::as_str),
        Some("leaf"),
        "{table:?}"
    );
    // ...and effective, not merely textual: the guard refuses a reaching command against it,
    // the same proof `node_class.rs` uses for a hand-written leaf entry.
    let refusal = refused(&dest, &["--node", "porch-sensor", "node", "info"]);
    assert!(refusal.contains("leaf"), "{refusal}");
}

#[test]
fn import_of_a_pre_class_export_imports_as_daemon() {
    // Hand-written, not derived from a fresh export with `class` deleted: this is what a v1
    // export written *before* eieio-x7g.5 actually looked like — the field did not exist in
    // `ExportedNode` at all — so the fixture has to be independent of whatever the current
    // export format happens to produce, or a future format change could make this test pass
    // for the wrong reason.
    let config_home = scratch("import-pre-class-export");
    let export_file = config_home.join("pre-class-export.json");
    std::fs::write(
        &export_file,
        r#"{"format":"eieio.nodes/v1","nodes":[{"name":"kitchen","address":"http://10.0.0.5:7777"}]}"#,
    )
    .expect("writing the pre-class export fixture");

    ok(
        &config_home,
        &["nodes", "import", export_file.to_str().expect("utf-8 path")],
    );

    let table = node_table(&config_home, "kitchen");
    assert!(
        !table
            .as_table()
            .expect("a node entry is a TOML table")
            .contains_key("class"),
        "a pre-class export must import as daemon (no class key), not gain one: {table:?}"
    );
    // Effective, not just textual: no token was carried either (this fixture predates that
    // too), so a reaching command fails on the deterministic, network-free "no token
    // configured" check, never on "leaf" — proving the entry is genuinely daemon-class.
    let refusal = refused(&config_home, &["--node", "kitchen", "node", "info"]);
    assert!(refusal.contains("no token configured"), "{refusal}");
    assert!(!refusal.contains("leaf"), "{refusal}");
}

#[test]
fn import_force_on_a_leaf_collision_preserves_the_class_the_import_carries() {
    // What should `import --force` do when a name already configured as `leaf` collides with
    // an import? The plan for this bead is explicit that the defensible claim is narrower than
    // "the class always survives force": the import is what the operator chose to apply, so an
    // import that explicitly says `daemon` for that name changing it to `daemon` would be a
    // legitimate, deliberate downgrade, not a bug. What must never happen is the class
    // *silently* reverting to `daemon` because the export format forgot to carry it — which is
    // exactly the bug eieio-x7g.5's integration fix closed (`Config::add` rebuilds the entry
    // from scratch, so a `class` missing from `ExportedNode` — or dropped on the way into
    // `config.add` — would flip a working leaf back to daemon with no operator choice involved
    // at all).
    //
    // So this test exercises the case that actually proves the fix: the existing entry is
    // `leaf`, the import explicitly re-asserts `class":"leaf"` while also changing the address
    // (an operator re-provisioning the same device), and `--force` must apply the address
    // change while the class comes through as `leaf` — not the zero-value `daemon` a broken
    // `class` field or a `config.add` call that dropped it on the floor would produce. Combined
    // with `export_carries_class_for_a_leaf_entry_and_omits_it_for_a_daemon_one` above (which
    // proves a leaf's class is never merely *omitted* from a fresh export), this rules out the
    // silent-loss failure mode without asserting an opinion about the deliberate-downgrade case,
    // which this test does not exercise.
    let config_home = scratch("import-force-leaf-collision");
    ok(
        &config_home,
        &[
            "node",
            "add",
            "pi-porch",
            "--addr",
            "http://10.0.0.7:7777",
            "--class",
            "leaf",
        ],
    );

    let export_file = config_home.join("reprovisioned-export.json");
    std::fs::write(
        &export_file,
        r#"{"format":"eieio.nodes/v1","nodes":[{"name":"pi-porch","address":"http://10.0.0.7:9999","class":"leaf"}]}"#,
    )
    .expect("writing the reprovisioned export fixture");

    let report = ok(
        &config_home,
        &[
            "nodes",
            "import",
            "--force",
            export_file.to_str().expect("utf-8 path"),
        ],
    );
    assert!(report.contains("updated"), "{report}");
    assert!(report.contains("pi-porch"), "{report}");

    let table = node_table(&config_home, "pi-porch");
    assert_eq!(
        table.get("addr").and_then(toml::Value::as_str),
        Some("http://10.0.0.7:9999"),
        "force must still apply the address change: {table:?}"
    );
    assert_eq!(
        table.get("class").and_then(toml::Value::as_str),
        Some("leaf"),
        "force must not lose the class an explicit leaf-carrying import reasserted: {table:?}"
    );

    // Effective, not just textual: still refused as a leaf, at the new address.
    let refusal = refused(&config_home, &["--node", "pi-porch", "node", "info"]);
    assert!(refusal.contains("leaf"), "{refusal}");
}
