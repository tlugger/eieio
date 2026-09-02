//! `eio service`, end to end (SERVICE-SPEC §9.1).
//!
//! Every test runs the real binary over a real file, because every claim being made is about
//! what happens to somebody's file: that a minted id is a §2.1 id and lands in the text, that
//! a refused command leaves the bytes alone, that `validate` names each of §7's classes, and
//! that the exit code says which happened. None of that is checkable by calling a function.
//!
//! The fixture is deliberately untidy — a comment, an aligned `=`, a multi-line array — so
//! that "preserves comments and formatting" is asserted against something that has some.

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

/// Runs `eio service <args>` in `dir`.
///
/// `CARGO_BIN_EXE_eio` is the binary this test crate was built alongside, so no test here can
/// pass against a stale install.
fn eio(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_eio"))
        .arg("service")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("eio runs")
}

/// Runs a command that is expected to succeed, and answers with its stdout.
fn ok(dir: &Path, args: &[&str]) -> String {
    let output = eio(dir, args);
    assert!(
        output.status.success(),
        "eio service {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

/// Runs a command that is expected to be refused, and answers with its stderr.
fn refused(dir: &Path, args: &[&str]) -> String {
    let output = eio(dir, args);
    assert!(
        !output.status.success(),
        "eio service {args:?} was supposed to fail, and printed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stderr).expect("utf-8 stderr")
}

/// A service file a person wrote, with trivia worth preserving.
const HAND_WRITTEN: &str = r#"# the kitchen service
name = "kitchen"
autostart  = true

connections = [
  "b7k2.out -> f3m9.in",
]

[blocks.b7k2]
name  = "Thermometer"
block = "temp-sensor:1.0.0"

[blocks.f3m9]
block = "filter:1.2.0"
"#;

fn fixture(test: &str) -> PathBuf {
    let dir = scratch(test);
    std::fs::write(dir.join("kitchen.toml"), HAND_WRITTEN).expect("the fixture");
    dir
}

fn text(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).expect("the file")
}

#[test]
fn new_writes_a_file_whose_stem_is_its_name() {
    let dir = scratch("new");
    let out = ok(&dir, &["new", "kitchen", "--autostart"]);
    assert!(out.contains("kitchen.toml"), "{out}");

    // SERVICE §1: the stem is the name, so the caller names the service and the command
    // decides the filename — offering both would be offering a way to disagree with §1.
    assert_eq!(
        text(&dir, "kitchen.toml"),
        "name = \"kitchen\"\nautostart = true\n"
    );
    // A service with no blocks is valid: it runs nothing, which is what it says (§3). Stage 2
    // has nothing to check, which is not the same as having skipped something.
    let report = ok(&dir, &["validate", "kitchen.toml"]);
    assert!(report.contains("stage 1: ok"), "{report}");
    assert!(report.contains("stage 2: ok (no blocks)"), "{report}");
    assert!(!report.contains("not checked"), "{report}");

    let again = refused(&dir, &["new", "kitchen"]);
    assert!(again.contains("already exists"), "{again}");
    assert!(
        refused(&dir, &["new", "Kitchen"]).contains("SERVICE §3"),
        "a service name is held to the same pattern"
    );
}

#[test]
fn add_block_mints_an_id_and_leaves_the_rest_of_the_file_alone() {
    let dir = fixture("add-block");
    let out = ok(
        &dir,
        &[
            "add-block",
            "kitchen.toml",
            "--block",
            "publisher:1.0.0",
            "--name",
            "Alarm",
            "--prop",
            "topic=\"kitchen.cold\"",
        ],
    );

    // The id is printed because the next thing its author does is write a connection naming
    // it (SERVICE §9.1).
    let id = out.split_whitespace().next().expect("an id on stdout");
    assert!(eio_service::id::is_id(id), "{id:?} is not a §2.1 id");
    assert!(!HAND_WRITTEN.contains(id), "the id is new to the file");

    // The fixture, unchanged, with the new block after it — not `contains(id)`, which the
    // command just made true and which would pass however much else it had rewritten.
    let after = text(&dir, "kitchen.toml");
    assert!(
        after.starts_with(HAND_WRITTEN),
        "the whole fixture is still the head of the file:\n{after}"
    );
    // Everything the fixture said, still said the same way.
    for line in HAND_WRITTEN.lines() {
        assert!(after.contains(line), "the edit lost {line:?}:\n{after}");
    }
    assert!(after.contains(&format!("[blocks.{id}]")));
    assert!(after.contains("name = \"Alarm\""));
}

#[test]
fn a_supplied_id_is_held_to_the_same_rule() {
    let dir = fixture("supplied-id");
    ok(
        &dir,
        &[
            "add-block",
            "kitchen.toml",
            "--block",
            "x:1",
            "--id",
            "q4tv",
        ],
    );
    assert!(text(&dir, "kitchen.toml").contains("[blocks.q4tv]"));

    let refusal = refused(
        &dir,
        &[
            "add-block",
            "kitchen.toml",
            "--block",
            "x:1",
            "--id",
            "Q4TV",
        ],
    );
    assert!(refusal.contains("SERVICE §2.1"), "{refusal}");
}

#[test]
fn connect_and_disconnect_round_trip() {
    let dir = fixture("wiring");
    let before = text(&dir, "kitchen.toml");

    ok(&dir, &["connect", "kitchen.toml", "f3m9.out", "b7k2.in"]);
    assert!(text(&dir, "kitchen.toml").contains("f3m9.out -> b7k2.in"));

    ok(&dir, &["disconnect", "kitchen.toml", "f3m9.out", "b7k2.in"]);
    assert_eq!(
        text(&dir, "kitchen.toml"),
        before,
        "an edge added and removed leaves the file as it was"
    );
}

#[test]
fn a_refused_command_writes_nothing() {
    // SERVICE §9: an edit that would make the file invalid MUST fail and change nothing.
    let dir = fixture("refusals");
    let before = text(&dir, "kitchen.toml");

    let cases: [(&[&str], &str); 5] = [
        (
            &["connect", "kitchen.toml", "b7k2.out", "nope.in"],
            "defines no",
        ),
        (
            &["connect", "kitchen.toml", "b7k2.out", "f3m9.err"],
            "ABI §6.4",
        ),
        (
            &["connect", "kitchen.toml", "b7k2.out", "f3m9.in"],
            "already connects",
        ),
        (&["remove-block", "kitchen.toml", "nope"], "defines no"),
        // `unset-prop` on an absent property is deliberately NOT here. It used to be, and
        // eieio-m9s.10 moved it: SERVICE §9 puts clearing an OPTIONAL thing on the
        // end-state side of its removal line, so unsetting a property that was never set
        // succeeds and reports that it did nothing. What still refuses is an unknown
        // *instance* — the identified half of the same call — which the case below covers.
        (
            &["unset-prop", "kitchen.toml", "nope", "interval_ms"],
            "defines no",
        ),
    ];
    for (args, expected) in cases {
        let output = eio(&dir, args);
        let refusal = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?} succeeded");
        assert!(
            refusal.contains(expected),
            "{args:?} should have said {expected:?}, said:\n{refusal}"
        );
        assert_eq!(text(&dir, "kitchen.toml"), before, "{args:?} wrote");
        // Nothing on stdout, because nothing happened. A caller reading only stdout — which
        // the agent surface this binary exists for does — must not be told about an edit that
        // is not on disk.
        assert!(
            output.stdout.is_empty(),
            "{args:?} announced an edit it did not make: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn a_failed_add_block_does_not_print_an_id() {
    // The sharp edge of the rule above. `add-block` prints a minted id so its author can wire
    // it up next; an id printed for a block that was never written sends them to a `connect`
    // that cannot work. The refusal here comes from `check`, which runs *after* the block was
    // added in memory — so this only holds because output waits for the write.
    let dir = fixture("failed-add");
    let before = text(&dir, "kitchen.toml");
    let output = eio(
        &dir,
        &[
            "add-block",
            "kitchen.toml",
            "--block",
            "x:1",
            "--id",
            "zzzz",
            "--prop",
            "bogus=(nosuchfn 1)",
        ],
    );

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "printed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(text(&dir, "kitchen.toml"), before);
}

#[test]
fn removing_a_block_takes_its_connections_with_it() {
    // Cascading is not a convenience: a connection naming an instance the file does not
    // define is SERVICE §7's dangling-connection error, so the alternative is writing a file
    // that will not load.
    let dir = fixture("remove");
    let out = ok(&dir, &["remove-block", "kitchen.toml", "f3m9"]);
    assert!(out.contains("disconnected b7k2.out -> f3m9.in"), "{out}");

    let after = text(&dir, "kitchen.toml");
    assert!(!after.contains("f3m9"), "{after}");
    assert!(after.starts_with("# the kitchen service\n"), "{after}");
    assert!(ok(&dir, &["validate", "kitchen.toml"]).contains("stage 1: ok"));
}

#[test]
fn show_resolves_the_names_the_format_keeps_out_of_connections() {
    let dir = fixture("show");
    ok(
        &dir,
        &["set-prop", "kitchen.toml", "f3m9", "threshold", "18.0"],
    );
    let rendered = ok(&dir, &["show", "kitchen.toml"]);

    assert_eq!(
        rendered,
        "kitchen  (autostart)\n\
         \n\
         blocks\n  \
           b7k2  \"Thermometer\"  temp-sensor:1.0.0\n  \
           f3m9  -              filter:1.2.0\n      \
             threshold = 18.0\n\
         \n\
         connections\n  \
           b7k2 \"Thermometer\" .out  ->  f3m9 - .in\n",
        "actual:\n{rendered}"
    );
}

#[test]
fn show_puts_the_property_under_the_block_that_has_it() {
    // Guards the assertion above against reading right for the wrong reason: `set-prop`
    // targeted f3m9, so the property must appear under f3m9 and not under whichever block the
    // renderer happened to be printing.
    let dir = fixture("show-props");
    ok(
        &dir,
        &["set-prop", "kitchen.toml", "f3m9", "threshold", "18.0"],
    );
    let rendered = ok(&dir, &["show", "kitchen.toml"]);
    let f3m9 = rendered.find("f3m9  -").expect("f3m9's row");
    let property = rendered.find("threshold").expect("the property");
    assert!(property > f3m9, "the property is under f3m9:\n{rendered}");
}

#[test]
fn validate_names_each_of_service_7s_classes() {
    let dir = scratch("validate-classes");
    // One file per class, each wrong in exactly one way, so a message naming the wrong class
    // cannot pass by matching a neighbour's text.
    let cases: [(&str, &str, &str); 7] = [
        ("malformed", "name = \"a\"\n[blocks\n", "TOML parse error"),
        (
            "unknownfield",
            "name = \"a\"\nautostrat = true\n",
            "unknown field",
        ),
        (
            "badname",
            "name = \"Kitchen\"\n",
            "does not match ^[a-z0-9]",
        ),
        (
            "badid",
            "name = \"badid\"\n[blocks.Thermo]\nblock = \"x:1\"\n",
            "SERVICE §2.1",
        ),
        (
            "emptyref",
            "name = \"emptyref\"\n[blocks.a]\nblock = \"\"\n",
            "names no block",
        ),
        (
            "badsyntax",
            "name = \"badsyntax\"\nconnections = [\"a.out => b.in\"]\n[blocks.a]\nblock = \"x:1\"\n",
            "there is no `->`",
        ),
        (
            "dangling",
            "name = \"dangling\"\nconnections = [\"a.out -> nope.in\"]\n[blocks.a]\nblock = \"x:1\"\n",
            "which this service does not define",
        ),
    ];

    for (stem, body, expected) in cases {
        let file = format!("{stem}.toml");
        std::fs::write(dir.join(&file), body).expect("the fixture");
        let output = eio(&dir, &["validate", &file]);
        let printed = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.status.success(), "{file} validated:\n{printed}");
        assert!(
            printed.contains(expected),
            "{file} should have said {expected:?}, said:\n{printed}"
        );
    }
}

#[test]
fn validate_refuses_a_stem_that_disagrees_with_the_name() {
    // SERVICE §1. A file this command accepted and a node refuses would make it worth less
    // than reading the specification.
    let dir = fixture("stem");
    std::fs::rename(dir.join("kitchen.toml"), dir.join("pantry.toml")).expect("rename");

    let output = eio(&dir, &["validate", "pantry.toml"]);
    let printed = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(!output.status.success(), "{printed}");
    assert!(printed.contains("§1 requires them to match"), "{printed}");

    // And the same file under the right name passes, so the check is the stem and not the
    // file's contents.
    std::fs::rename(dir.join("pantry.toml"), dir.join("kitchen.toml")).expect("rename");
    assert!(ok(&dir, &["validate", "kitchen.toml"]).contains("stem:    ok"));
}

#[test]
fn stage_2_runs_only_over_the_manifests_it_was_given() {
    let dir = scratch("stage-2");
    let manifest = dir.join("gpio-echo.json");
    std::fs::write(
        &manifest,
        r#"{ "name": "gpio-echo", "version": "1.0.0", "abi": { "major": 1, "minor": 0 },
             "capabilities": ["gpio"],
             "inputs": [{ "name": "in" }], "outputs": [{ "name": "out" }],
             "properties": [], "targets": ["wasm32-unknown-unknown"], "aot": [] }"#,
    )
    .expect("a manifest");
    let flag = format!("gpio-echo:1.0.0={}", manifest.display());

    ok(&dir, &["new", "echoes"]);
    for id in ["e1", "e2"] {
        ok(
            &dir,
            &[
                "add-block",
                "echoes.toml",
                "--block",
                "gpio-echo:1.0.0",
                "--id",
                id,
            ],
        );
    }
    ok(&dir, &["connect", "echoes.toml", "e1.out", "e2.in"]);
    assert!(ok(&dir, &["validate", "echoes.toml", "--manifest", &flag]).contains("stage 2: ok"));

    // Without the manifest nothing is checkable, and saying "ok" would be reporting a stage
    // that did not run — the one way this command could lie.
    let unchecked = ok(&dir, &["validate", "echoes.toml"]);
    assert!(unchecked.contains("stage 2: not run"), "{unchecked}");
    assert!(
        unchecked.contains("not checked: gpio-echo:1.0.0"),
        "{unchecked}"
    );

    // A *partial* stage 2 says so in the headline. A caller scanning for `stage 2: ok` reads
    // that line and not the `not checked` ones under it, so "ok" alone would be the same lie
    // in a quieter voice.
    ok(
        &dir,
        &[
            "add-block",
            "echoes.toml",
            "--block",
            "other:2.0.0",
            "--id",
            "o1",
        ],
    );
    let partial = ok(&dir, &["validate", "echoes.toml", "--manifest", &flag]);
    assert!(
        partial.contains("stage 2: ok for 1 of 2 blocks"),
        "{partial}"
    );
    assert!(partial.contains("not checked: other:2.0.0"), "{partial}");
    ok(&dir, &["remove-block", "echoes.toml", "o1"]);

    // Both stage-2 classes, and each only visible because a manifest was supplied.
    ok(&dir, &["connect", "echoes.toml", "e1.nope", "e2.in"]);
    ok(&dir, &["set-prop", "echoes.toml", "e1", "bogus", "1"]);
    assert!(
        ok(&dir, &["validate", "echoes.toml"]).contains("stage 2: not run"),
        "stage 1 still passes: a port is not checkable from the file alone"
    );

    let output = eio(&dir, &["validate", "echoes.toml", "--manifest", &flag]);
    let printed = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(!output.status.success(), "{printed}");
    assert!(
        printed.contains("declares no output port \"nope\""),
        "{printed}"
    );
    assert!(
        printed.contains("declares no property \"bogus\""),
        "{printed}"
    );
}

#[test]
fn a_property_expression_may_contain_an_equals_sign() {
    // `--prop NAME=EXPR` splits at the first `=`, because the value is an expression and
    // `(= $a 1)` is one of them. Splitting anywhere else makes a comparison unwritable.
    let dir = fixture("equals");
    ok(
        &dir,
        &[
            "add-block",
            "kitchen.toml",
            "--block",
            "filter:1.2.0",
            "--id",
            "cmp1",
            "--prop",
            "keep=(= $mode 1)",
        ],
    );
    assert!(text(&dir, "kitchen.toml").contains("keep = \"(= $mode 1)\""));
}

#[test]
fn a_property_that_cannot_mean_anything_is_refused_before_it_is_written() {
    // EXPR §10's static analysis runs inside stage 1, so `check` catches this on the way out
    // and the file is never written (SERVICE §9).
    let dir = fixture("bad-expression");
    let before = text(&dir, "kitchen.toml");
    let refusal = refused(
        &dir,
        &[
            "set-prop",
            "kitchen.toml",
            "b7k2",
            "interval",
            "(nosuchfn 1)",
        ],
    );
    assert!(refusal.contains("not a valid service file"), "{refusal}");
    assert_eq!(text(&dir, "kitchen.toml"), before);
}

#[test]
fn unsetting_a_property_that_was_never_set_reports_rather_than_refuses() {
    // eieio-m9s.10, and the other half of `a_refused_command_writes_nothing`'s table: SERVICE
    // §9 puts clearing an OPTIONAL thing on the end-state side of its removal line, so this
    // succeeds. It is worth a positive test because the refusal table can only prove what
    // still refuses, and the interesting claim here is what a person is *told* — silence
    // would leave them unsure whether the command had understood them.
    let dir = fixture("unset-absent");
    let before = text(&dir, "kitchen.toml");

    let output = eio(&dir, &["unset-prop", "kitchen.toml", "b7k2", "nope"]);
    assert!(
        output.status.success(),
        "absent is an end state, not a refusal: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(
        said.contains("already unset"),
        "it has to say it did nothing, not just exit 0: {said}"
    );
    // And it wrote nothing, which is the same promise the refusal table checks.
    assert_eq!(text(&dir, "kitchen.toml"), before);
}
