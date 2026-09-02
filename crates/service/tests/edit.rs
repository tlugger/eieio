//! A preserving edit changes what it was asked to change, and nothing else (SERVICE-SPEC §9).
//!
//! Every test here is a variation on one assertion: take a file that a person clearly wrote by
//! hand — aligned `=`, comments in awkward places, a multi-line array, a `[ui]` table — edit it,
//! and check that the diff is the edit. That is DESIGNER §4's hard requirement, and it is not
//! provable by reading the implementation, because the property belongs to the parser
//! underneath it.

use eio_service::edit::{Document, EditError, WriteError};

/// A file with every kind of trivia a preserving edit has to carry through.
///
/// Deliberately not tidy: two spaces before an `=`, a comment inside the array, a comment
/// trailing a value, single quotes on one string, and a `[ui]` table with a nested inline
/// table. Each of them is something a value-tree round trip would silently destroy.
const HAND_WRITTEN: &str = r#"# the kitchen service, edited by a person
name = "kitchen"
autostart  = true

connections = [
  # the sensor feeds the filter
  "b7k2.out -> f3m9.in",
  'f3m9.err -> k1p8.in',
]

[blocks.b7k2]
name  = "Thermometer"   # by the window
block = "ghcr.io/tlugger/temp-sensor:1.0.0"

[blocks.b7k2.props]
interval_ms = "5000"

[blocks.f3m9]
block = "filter:1.2.0"

[blocks.f3m9.props]
reading   = "(float $temp)"
threshold = "18.0"

[blocks.k1p8]
block = "publisher:1.0.0"

[ui]
viewport = { x = 0, y = 0, zoom = 1.0 }

[ui.blocks.b7k2]
x = 148
y = 234
"#;

/// The lines an edit added, removed or changed — the diff, as a person would read it.
///
/// Line-based and order-insensitive on purpose: what these tests assert is that *nothing else*
/// moved, and a test that also pinned where the new lines went would fail on a formatting
/// choice rather than on a regression.
fn touched(before: &str, after: &str) -> (Vec<String>, Vec<String>) {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let removed = old
        .iter()
        .filter(|line| !new.contains(line))
        .map(|line| String::from(*line))
        .collect();
    let added = new
        .iter()
        .filter(|line| !old.contains(line))
        .map(|line| String::from(*line))
        .collect();
    (removed, added)
}

/// Opens the fixture, applies `edit`, and returns what changed — asserting throughout that the
/// result is still a service file the reader accepts.
fn edited(edit: impl FnOnce(&mut Document)) -> (String, Vec<String>, Vec<String>) {
    let mut doc = Document::parse(HAND_WRITTEN).expect("the fixture is a valid service file");
    edit(&mut doc);
    let after = doc.render();
    doc.check()
        .expect("still a valid service file after the edit");
    let (removed, added) = touched(HAND_WRITTEN, &after);
    (after, removed, added)
}

#[test]
fn the_fixture_survives_a_read_modify_write_with_no_edit() {
    let doc = Document::parse(HAND_WRITTEN).expect("valid");
    assert_eq!(
        doc.render(),
        HAND_WRITTEN,
        "an edit-free round trip is a no-op"
    );
}

#[test]
fn adding_a_block_touches_only_the_block() {
    let (after, removed, added) = edited(|doc| {
        doc.add_block("m4v7", Some("Logger"), "logger:2.0.0")
            .expect("a fresh id");
    });
    assert!(removed.is_empty(), "nothing was lost: {removed:?}");
    assert_eq!(
        added,
        [
            "[blocks.m4v7]",
            "name = \"Logger\"",
            "block = \"logger:2.0.0\""
        ],
        "and only the block arrived"
    );
    // SERVICE §5: a new table header must not land above a top-level key.
    assert!(after.find("connections").unwrap() < after.find("[blocks.m4v7]").unwrap());
}

#[test]
fn a_property_edit_leaves_its_neighbours_aligned() {
    let (_, removed, added) = edited(|doc| {
        doc.set_prop("f3m9", "threshold", "20.0")
            .expect("f3m9 has props");
    });
    assert_eq!(removed, ["threshold = \"18.0\""]);
    assert_eq!(added, ["threshold = \"20.0\""]);
    // `reading   = ...`'s three spaces are somebody's alignment, and are not this crate's to
    // normalise just because the line below it changed.
}

#[test]
fn a_new_property_joins_an_existing_props_table() {
    let (after, removed, added) = edited(|doc| {
        doc.set_prop("k1p8", "topic", "\"kitchen.cold\"")
            .expect("k1p8 exists");
    });
    assert!(removed.is_empty(), "{removed:?}");
    // SERVICE §4's example spells this `topic = "\"kitchen.cold\""`; a value written by this
    // crate comes out single-quoted instead, because a string property's value is a *quoted*
    // expression and a TOML literal string is the spelling that needs no escapes. The two are
    // the same string, which is what `check` below asserts — and a crate that forced the
    // escaped spelling would be owning an escaper to make a file look like an example.
    assert_eq!(added, ["[blocks.k1p8.props]", "topic = '\"kitchen.cold\"'"]);
    assert_eq!(
        doc_prop(&after, "k1p8", "topic"),
        "\"kitchen.cold\"",
        "and it reads back as the expression that was written"
    );
    assert!(after.contains("[ui]"), "and `[ui]` is still there");
}

/// What the *reader* makes of a property, which is the only thing its spelling has to preserve.
fn doc_prop(text: &str, id: &str, property: &str) -> String {
    let parsed = eio_service::parse(text).expect("valid");
    parsed.service.blocks[id].props[property].clone()
}

#[test]
fn a_connection_is_appended_in_the_arrays_own_style() {
    let (_, removed, added) = edited(|doc| {
        doc.connect("f3m9.out", "k1p8.in").expect("both exist");
    });
    assert!(removed.is_empty(), "{removed:?}");
    assert_eq!(
        added,
        ["  \"f3m9.out -> k1p8.in\","],
        "the two-space indent came from the array, not from a default"
    );
}

#[test]
fn removing_a_block_takes_its_connections_and_leaves_its_layout() {
    let (after, removed, added) = edited(|doc| {
        let dropped = doc.remove_block("f3m9").expect("f3m9 exists");
        assert_eq!(
            dropped,
            ["b7k2.out -> f3m9.in", "f3m9.err -> k1p8.in"],
            "both edges naming it, reported so a caller can say so"
        );
    });
    assert!(added.is_empty(), "{added:?}");
    assert_eq!(
        removed,
        [
            // The comment sat above the edge it describes and goes with it. A comment inside an
            // array is trivia belonging to the element beneath it, so keeping it would leave a
            // sentence about a connection this file no longer has.
            "  # the sensor feeds the filter",
            "  \"b7k2.out -> f3m9.in\",",
            "  'f3m9.err -> k1p8.in',",
            "[blocks.f3m9]",
            "block = \"filter:1.2.0\"",
            "[blocks.f3m9.props]",
            "reading   = \"(float $temp)\"",
            "threshold = \"18.0\"",
        ]
    );
    // SERVICE §6: a stale annotation is inert, and tidying it would be this crate deciding
    // that `[ui]`'s keys are block ids.
    assert!(after.contains("[ui.blocks.b7k2]"));
    // The file's own leading comment is untouched: nothing outside the edit moved.
    assert!(after.starts_with("# the kitchen service, edited by a person\n"));
}

#[test]
fn a_ui_annotation_is_written_without_a_schema_for_it() {
    let (after, removed, added) = edited(|doc| {
        doc.set_ui(&["blocks", "f3m9"], "{ x = 300, y = 96 }")
            .expect("a TOML value");
    });
    assert!(removed.is_empty(), "{removed:?}");
    // `[ui.blocks]` is implicit in the fixture — it exists only through `[ui.blocks.b7k2]` —
    // and materialises here because a key now sits directly in it. That is TOML's doing and
    // not a reformatting: the header is part of expressing the new value.
    assert_eq!(added, ["[ui.blocks]", "f3m9 = { x = 300, y = 96 }"]);
    assert!(after.contains("viewport = { x = 0, y = 0, zoom = 1.0 }"));
}

#[test]
fn removing_a_ui_annotation_that_is_not_there_is_not_an_error() {
    let (_, removed, added) = edited(|doc| {
        doc.remove_ui(&["blocks", "nobody"])
            .expect("absent is fine");
        doc.remove_ui(&["nothing", "here"]).expect("so is the path");
    });
    assert!(removed.is_empty() && added.is_empty());
}

#[test]
fn renaming_a_block_touches_only_its_name_line() {
    // SERVICE §9's new bullet: the whole point of `set_name` existing is that renaming a
    // block leaves its id, connections, properties and `[ui]` alone — the failure mode a
    // remove-and-re-add would otherwise cause silently.
    let (after, removed, added) = edited(|doc| {
        doc.set_name("b7k2", "Kitchen Thermometer")
            .expect("b7k2 has a name already");
    });
    assert_eq!(removed, ["name  = \"Thermometer\"   # by the window"]);
    assert_eq!(
        added,
        ["name  = \"Kitchen Thermometer\"   # by the window"],
        "the two-space alignment and the trailing comment both survive"
    );
    assert!(after.contains("[blocks.b7k2]"), "the id is untouched");
    assert!(
        after.contains("\"b7k2.out -> f3m9.in\""),
        "connections naming it are untouched"
    );
    assert!(
        after.contains("interval_ms = \"5000\""),
        "its properties are untouched"
    );
    assert!(
        after.contains("[ui.blocks.b7k2]\nx = 148\ny = 234"),
        "its [ui] entry is untouched"
    );
}

#[test]
fn setting_a_name_on_a_block_that_has_none_adds_the_key() {
    // f3m9 has a `block` line and no `name` (SERVICE §9: "setting a name on a block that has
    // none adds the key").
    let (after, removed, added) = edited(|doc| {
        doc.set_name("f3m9", "Filter").expect("f3m9 exists");
    });
    assert!(removed.is_empty(), "{removed:?}");
    assert_eq!(added, ["name = \"Filter\""]);
    // The new key lands above `[blocks.f3m9.props]` — a scalar key of a table always must,
    // in TOML, or the parser would read it as belonging to the sub-table instead.
    assert!(after.find("name = \"Filter\"").unwrap() < after.find("[blocks.f3m9.props]").unwrap());
}

#[test]
fn clearing_a_name_removes_the_key_rather_than_emptying_it() {
    let (after, removed, added) = edited(|doc| {
        doc.remove_name("b7k2").expect("b7k2 has a name");
    });
    assert_eq!(removed, ["name  = \"Thermometer\"   # by the window"]);
    assert!(added.is_empty(), "{added:?}");
    let parsed = eio_service::parse(&after).expect("still valid");
    assert_eq!(
        parsed.service.blocks["b7k2"].name, None,
        "absent, not an empty string"
    );
}

#[test]
fn clearing_a_name_that_is_already_absent_is_not_an_error() {
    let (_, removed, added) = edited(|doc| {
        doc.remove_name("f3m9")
            .expect("f3m9 has no name; clearing one it doesn't have is a no-op");
    });
    assert!(removed.is_empty() && added.is_empty());
}

#[test]
fn set_name_and_remove_name_refuse_an_unknown_id() {
    let mut doc = Document::parse(HAND_WRITTEN).expect("valid");
    assert_eq!(
        doc.set_name("nope", "x"),
        Err(EditError::NoSuchInstance {
            id: String::from("nope")
        })
    );
    assert_eq!(
        doc.remove_name("nope"),
        Err(EditError::NoSuchInstance {
            id: String::from("nope")
        })
    );
    assert_eq!(doc.render(), HAND_WRITTEN, "and nothing was written");
}

#[test]
fn autostart_is_set_in_place() {
    let (_, removed, added) = edited(|doc| doc.set_autostart(false));
    assert_eq!(removed, ["autostart  = true"]);
    // Two spaces before the `=`, still. The whitespace belongs to the key rather than to the
    // value, so changing the value leaves somebody's alignment where they put it.
    assert_eq!(added, ["autostart  = false"]);
}

#[test]
fn disconnect_matches_what_the_edge_means_not_how_it_was_spelled() {
    let text = "name = \"k\"\nconnections = [\"a.out->b.in\"]\n\n[blocks.a]\nblock = \"x:1\"\n\n[blocks.b]\nblock = \"y:1\"\n";
    let mut doc = Document::parse(text).expect("valid");
    // SERVICE §5 permits any amount of whitespace around the arrow, so a textual search for
    // the canonical spelling would miss this edge entirely.
    doc.disconnect("a.out", "b.in").expect("the same edge");
    assert!(!doc.render().contains("a.out"));
    assert_eq!(
        doc.disconnect("a.out", "b.in"),
        Err(EditError::NoSuchConnection {
            edge: String::from("a.out -> b.in")
        })
    );
}

#[test]
fn connecting_a_second_time_is_a_duplicate_whatever_the_spelling() {
    let mut doc = Document::parse(HAND_WRITTEN).expect("valid");
    assert_eq!(
        doc.connect("b7k2.out", "f3m9.in"),
        Err(EditError::DuplicateConnection {
            edge: String::from("b7k2.out -> f3m9.in")
        }),
        "SERVICE §5: the same edge twice would deliver each batch twice"
    );
    assert_eq!(doc.render(), HAND_WRITTEN, "and nothing was written");
}

/// SERVICE §5's hazard, and the reason `connections_or_insert` needs no defensive code.
///
/// A top-level key written below a table header belongs to that table. This pins that
/// `toml_edit` renders a root key-value above every sub-table whatever order it was inserted
/// in — a property of the library, so it is asserted rather than assumed.
#[test]
fn a_created_connections_array_lands_above_the_first_table_header() {
    let mut doc = Document::parse(
        "name = \"k\"\n\n[blocks.a]\nblock = \"x:1\"\n\n[blocks.b]\nblock = \"y:1\"\n",
    )
    .expect("valid");
    doc.connect("a.out", "b.in").expect("both exist");

    let text = doc.render();
    assert!(
        text.find("connections").unwrap() < text.find("[blocks.a]").unwrap(),
        "it would otherwise be a key of `blocks.b`:\n{text}"
    );
    let parsed = doc.check().expect("and it reads back as the service's");
    assert_eq!(parsed.connections.len(), 1);
}

#[test]
fn a_new_file_is_a_valid_service_with_no_blocks() {
    let doc = Document::create("kitchen").expect("a valid name");
    assert_eq!(doc.render(), "name = \"kitchen\"\n");
    // SERVICE §3: a service with no blocks runs nothing, which is what it says.
    doc.check().expect("valid");
    assert_eq!(
        Document::create("Kitchen").err(),
        Some(EditError::BadServiceName {
            name: String::from("Kitchen")
        })
    );
}

#[test]
fn a_refused_edit_changes_nothing() {
    let mut doc = Document::parse(HAND_WRITTEN).expect("valid");
    let refusals: Vec<EditError> = vec![
        doc.add_block("B7K2", None, "x:1").unwrap_err(),
        doc.add_block("b7k2", None, "x:1").unwrap_err(),
        doc.add_block("new1", None, "   ").unwrap_err(),
        doc.remove_block("nope").unwrap_err(),
        doc.set_prop("nope", "a", "1").unwrap_err(),
        doc.set_prop("b7k2", "Interval", "1").unwrap_err(),
        doc.remove_prop("b7k2", "nope").unwrap_err(),
        doc.connect("b7k2.out", "nope.in").unwrap_err(),
        doc.connect("b7k2.out", "f3m9.err").unwrap_err(),
        doc.connect("b7k2out", "f3m9.in").unwrap_err(),
        doc.connect("b7k2.out", "f3m9.IN").unwrap_err(),
        doc.set_ui(&["blocks", "b7k2"], "{ x = ").unwrap_err(),
        doc.set_ui(&[], "1").unwrap_err(),
    ];

    assert_eq!(
        refusals,
        [
            EditError::BadId {
                id: String::from("B7K2")
            },
            EditError::DuplicateInstance {
                id: String::from("b7k2")
            },
            EditError::EmptyBlockRef,
            EditError::NoSuchInstance {
                id: String::from("nope")
            },
            EditError::NoSuchInstance {
                id: String::from("nope")
            },
            EditError::BadName {
                name: String::from("Interval")
            },
            EditError::NoSuchProperty {
                id: String::from("b7k2"),
                property: String::from("nope"),
            },
            EditError::NoSuchInstance {
                id: String::from("nope")
            },
            EditError::ErrorPortDestination,
            EditError::BadTerminal {
                terminal: String::from("b7k2out"),
                error: eio_service::ConnectionError::NoPort {
                    side: "source",
                    span: eio_service::Span::new(0, 7),
                },
            },
            EditError::BadTerminal {
                terminal: String::from("f3m9.IN"),
                error: eio_service::ConnectionError::BadPort {
                    side: "destination",
                    span: eio_service::Span::new(17, 2),
                },
            },
            EditError::BadUiValue {
                detail: doc_ui_error(),
            },
            EditError::BadUiPath,
        ]
    );

    // SERVICE §9: an edit that would make the file invalid fails and changes nothing.
    assert_eq!(doc.render(), HAND_WRITTEN);
}

/// Whatever `toml_edit` says about `"{ x = "`. Pinned by construction rather than by a literal,
/// because the message is the parser's and not this crate's contract.
fn doc_ui_error() -> String {
    "{ x = "
        .parse::<toml_edit::Value>()
        .expect_err("not a value")
        .to_string()
}

#[test]
fn an_invalid_file_cannot_be_opened_for_editing() {
    // Stage 1 on the way in: a caller that cannot parse a file cannot be told which of its
    // edits failed, and the list it gets is the reader's list.
    let errors = Document::parse("name = \"Kitchen\"\n").expect_err("a bad service name");
    assert_eq!(errors.len(), 1);
    assert!(matches!(errors[0], eio_service::Error::ServiceName { .. }));
}

#[test]
fn an_id_is_minted_around_what_the_file_already_uses() {
    let doc = Document::parse(HAND_WRITTEN).expect("valid");
    // `b7k2` is taken, so the first four bytes are skipped and the next chunk answers.
    let random = [11, 55, 18, 1, 7, 7, 7, 7];
    let taken = doc.mint_id(&random[..4]).expect("some id");
    assert_ne!(taken, "b7k2");
    assert!(eio_service::id::is_id(&taken));
}

// `Document::write` (SERVICE §9): the crate's own atomic write, which the CLI and — per
// DESIGNER §4 — the Designer's backend both call rather than each carrying a temp-file-plus-
// rename of their own. These tests are the ones that make "atomic" a checked property rather
// than a claim: a happy path alone would not distinguish this from `std::fs::write`.

/// A fresh directory under the OS temp dir, cleared first so a leftover from a killed run of
/// this binary cannot make a later run pass by accident.
fn scratch(test: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("eio-service-edit-write-{test}"));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clearing the scratch directory");
    }
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

#[test]
fn write_replaces_the_file_with_the_checked_render() {
    let dir = scratch("happy-path");
    let path = dir.join("kitchen.toml");
    std::fs::write(&path, "name = \"stale\"\n").expect("something already there");

    let mut doc = Document::parse(HAND_WRITTEN).expect("valid");
    doc.set_autostart(false);
    doc.write(&path).expect("a valid edit");

    assert_eq!(
        std::fs::read_to_string(&path).expect("written"),
        doc.render()
    );
    // And no temporary was left beside it once the rename succeeded.
    let leftover: Vec<_> = std::fs::read_dir(&dir)
        .expect("the scratch directory")
        .map(|entry| entry.expect("a directory entry").file_name())
        .collect();
    assert_eq!(leftover, [std::ffi::OsString::from("kitchen.toml")]);
}

#[test]
fn write_refuses_an_invalid_document_and_the_file_on_disk_is_untouched() {
    // SERVICE §9: "An edit that would make the file invalid MUST fail and change nothing" —
    // `check` alone proves the in-memory document is untouched; this proves the file is too.
    let dir = scratch("invalid");
    let path = dir.join("kitchen.toml");
    std::fs::write(&path, HAND_WRITTEN).expect("the original");

    let mut doc = Document::parse(HAND_WRITTEN).expect("valid");
    doc.set_prop("b7k2", "interval_ms", "(nosuchfn 1)")
        .expect("the property exists; the expression is nonsense");

    let error = doc.write(&path).expect_err("check refuses it");
    assert!(matches!(error, WriteError::Invalid(_)), "{error}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("still there"),
        HAND_WRITTEN,
        "an edit `check` rejects must not reach disk under any name for `write`"
    );
}

/// The test that proves atomicity rather than assuming it: a write that cannot finish must
/// leave `path` exactly as it was, not truncated and not partially replaced.
///
/// A temp-file-plus-rename implementation truncates nothing *if* every step after the temporary
/// file starts filling either finishes or is never reached — which a happy-path test cannot
/// distinguish from `std::fs::write(path, text)` truncating `path` directly. This forces the
/// first step, the temporary write, to fail by putting a directory exactly where the temporary
/// needs to go, so `write` cannot finish, and then checks what a crash at that point would also
/// have left behind: the original, byte for byte.
#[test]
fn a_write_that_cannot_finish_leaves_the_original_file_intact() {
    let dir = scratch("write-fails");
    let path = dir.join("kitchen.toml");
    std::fs::write(&path, HAND_WRITTEN).expect("the original");

    // `write_atomically`'s naming scheme, pinned here because this test only proves anything if
    // it blocks the exact path the implementation uses.
    let temporary = dir.join(".kitchen.toml.eio-tmp");
    std::fs::create_dir(&temporary).expect("occupying the temporary's path");

    let doc = Document::parse(HAND_WRITTEN).expect("valid");
    let error = doc
        .write(&path)
        .expect_err("writing into a directory fails");
    assert!(matches!(error, WriteError::Temporary { .. }), "{error}");

    assert_eq!(
        std::fs::read_to_string(&path).expect("still there, still a file"),
        HAND_WRITTEN,
        "the original must survive a write that never got past the temporary"
    );
}

/// The other half: a write whose temporary succeeds but whose rename cannot complete must still
/// leave whatever was at `path` untouched, and must not leave the temporary behind either.
#[test]
fn a_rename_that_cannot_finish_leaves_the_original_in_place_and_cleans_up() {
    let dir = scratch("rename-fails");
    // `path` names a directory rather than a file, so the rename onto it fails on every
    // platform this targets — the closest a test gets to "the process died between the write
    // and the rename" without actually killing the process.
    let path = dir.join("kitchen.toml");
    std::fs::create_dir(&path).expect("a directory sits where the file would go");

    let doc = Document::parse(HAND_WRITTEN).expect("valid");
    let error = doc
        .write(&path)
        .expect_err("renaming onto a directory fails");
    assert!(matches!(error, WriteError::Rename { .. }), "{error}");

    assert!(
        path.is_dir(),
        "what was at `path` must be exactly what was there before"
    );
    let temporary = dir.join(".kitchen.toml.eio-tmp");
    assert!(
        !temporary.exists(),
        "a failed rename must not leave its temporary behind"
    );
}
