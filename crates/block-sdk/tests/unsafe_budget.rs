//! SDK §4's unsafe budget, enforced rather than described.
//!
//! > The entire `unsafe` surface, enumerated for audit: allocator export glue,
//! > `(ptr,len) ↔ &[u8]` conversions at each export entry and host-fn call site, and the
//! > panic handler. Nothing else. Target: every `unsafe` block carries a `// SAFETY:`
//! > comment citing the ABI section that justifies it.
//!
//! "Enumerated for audit" is only true if something audits it. `clippy::undocumented_unsafe_blocks`
//! (on, workspace-wide, with `-D warnings`) catches a missing comment; it does not catch an
//! `unsafe` appearing in a *new file*, which is how a budget stops being a budget. This
//! test is the second half: the inventory itself.
//!
//! Both checks read the source as text, deliberately. The property is about what a
//! reviewer opening the crate would find, and a macro-aware check could pass while the
//! files a human reads say something else.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The files SDK §4's enumeration permits `unsafe` in.
///
/// Shorter than the spec's list of three, and that is a finding rather than an oversight:
///
/// - **Allocator export glue** — `allocator.rs`. Four blocks.
/// - **`(ptr, len)` ↔ `&[u8]` conversions** — `raw.rs`, and only the `unsafe extern` block
///   that declares the imports. The conversions themselves turned out to need no `unsafe`
///   at all: taking a slice's address is safe, and every import is declared `safe fn`
///   because ABI §7.0 and §8 specify the host side as total over its arguments.
/// - **The panic handler** — `panic.rs`, which contains no `unsafe`:
///   `core::arch::wasm32::unreachable()` is a safe function.
///
/// A file added here is a decision to widen the budget, which SDK §4 says is a spec
/// question. Adding one without amending the spec is the thing this list exists to make
/// visible in review.
const PERMITTED: &[&str] = &["allocator.rs", "raw.rs"];

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).expect("src/ is readable") {
        let path = entry.expect("a readable entry").path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files.sort();
    files
}

/// Whether `line` opens an `unsafe` block or declares an `unsafe` item.
///
/// `#[unsafe(no_mangle)]` is excluded: the edition-2024 attribute syntax spells a
/// *declaration* of an export, not a use of unsafe code, and there is nothing about it for
/// a `// SAFETY:` comment to justify.
fn is_unsafe_site(line: &str) -> bool {
    let line = line.trim();
    if line.starts_with("//") || line.starts_with("#[unsafe(") {
        return false;
    }
    line.contains("unsafe {") || line.contains("unsafe fn") || line.contains("unsafe extern")
}

/// The comment block immediately above `index`, as one lowercase string.
///
/// Walks back over `//` and `///` lines and attributes, so an `unsafe fn` whose safety
/// contract is a `/// # Safety` doc section is found as readily as a `// SAFETY:` comment
/// above a block.
fn preceding_comment(lines: &[&str], index: usize) -> String {
    let mut collected = Vec::new();
    for line in lines[..index].iter().rev() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            collected.push(trimmed.to_lowercase());
        } else if trimmed.starts_with("#[") || trimmed.starts_with("#![") || trimmed.is_empty() {
            // Attributes and blank lines sit between a doc block and its item.
            continue;
        } else {
            break;
        }
    }
    collected.join(" ")
}

/// Whether a file's path is inside a `#[cfg(test)]` module at `index`.
///
/// Crude on purpose: it asks only whether a `#[cfg(test)]` appeared earlier in the file.
/// Every test module in this crate is the last item in its file, which makes that exact
/// here and keeps the check something a reader can confirm at a glance.
fn in_test_module(lines: &[&str], index: usize) -> bool {
    lines[..index]
        .iter()
        .any(|line| line.trim() == "#[cfg(test)]")
}

/// One `unsafe` site found in the source, with the comment block above it.
struct Site {
    file: String,
    line: usize,
    text: String,
    comment: String,
    in_test: bool,
}

impl core::fmt::Display for Site {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.text)
    }
}

/// Every `unsafe` site under `src/`.
///
/// One walk, because the two checks below differ only in which predicate they apply to it
/// — scanning the tree twice would be the same code with one substring changed.
fn sites() -> Vec<Site> {
    let mut sites = Vec::new();
    for path in rust_files(&src_dir()) {
        let source = std::fs::read_to_string(&path).expect("a readable source file");
        let lines: Vec<&str> = source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !is_unsafe_site(line) {
                continue;
            }
            sites.push(Site {
                file: path
                    .file_name()
                    .expect("a file name")
                    .to_string_lossy()
                    .into_owned(),
                line: index + 1,
                text: line.trim().to_string(),
                comment: preceding_comment(&lines, index),
                in_test: in_test_module(&lines, index),
            });
        }
    }
    assert!(
        !sites.is_empty(),
        "no `unsafe` found at all — the detector is broken, not the crate clean"
    );
    sites
}

fn report(offenders: Vec<&Site>) -> String {
    offenders
        .iter()
        .map(|site| site.to_string())
        .collect::<Vec<_>>()
        .join("\n  ")
}

#[test]
fn every_unsafe_carries_a_safety_comment() {
    let sites = sites();
    let missing: Vec<&Site> = sites
        .iter()
        .filter(|site| !site.comment.contains("safety:") && !site.comment.contains("# safety"))
        .collect();

    assert!(
        missing.is_empty(),
        "SDK §4: every `unsafe` carries a `// SAFETY:` comment. Missing:\n  {}",
        report(missing)
    );
}

#[test]
fn every_shipped_unsafe_cites_the_abi_section_that_justifies_it() {
    // SDK §4's actual wording, and the part clippy cannot check. Applied to shipped code
    // only: inside a `#[cfg(test)]` module the justification is usually local — "this
    // pointer came from the `allocate` two lines up" — which is a *better* warrant than a
    // spec citation would be, and demanding a section number there would buy a ritual.
    let sites = sites();
    let uncited: Vec<&Site> = sites
        .iter()
        // "ABI §7.0", "ABI §9.1" — the section mark is what makes it a citation rather
        // than a mention.
        .filter(|site| !site.in_test && !site.comment.contains("abi §"))
        .collect();

    assert!(
        uncited.is_empty(),
        "SDK §4: every shipped `unsafe` cites the ABI section justifying it. Uncited:\n  {}",
        report(uncited)
    );
}

#[test]
fn unsafe_appears_only_in_the_files_sdk_4_enumerates() {
    let permitted: BTreeSet<&str> = PERMITTED.iter().copied().collect();
    let mut found = BTreeSet::new();

    for path in rust_files(&src_dir()) {
        let source = std::fs::read_to_string(&path).expect("a readable source file");
        if source.lines().any(is_unsafe_site) {
            found.insert(
                path.file_name()
                    .expect("a file name")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }

    let unexpected: Vec<&String> = found
        .iter()
        .filter(|name| !permitted.contains(name.as_str()))
        .collect();

    assert!(
        unexpected.is_empty(),
        "SDK §4 enumerates the whole unsafe surface, and {unexpected:?} is not in it. \
         Widening the budget is a spec question (SDK §4, ABI §14), not a local call: \
         amend the spec and `PERMITTED` together, or find a safe shape."
    );
}

#[test]
fn the_permitted_list_has_no_stale_entries() {
    // A budget that lists files which no longer contain `unsafe` is a budget nobody has
    // read lately, and it silently pre-authorises the next one to land there.
    let mut stale = Vec::new();
    for name in PERMITTED {
        let path = src_dir().join(name);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("{name} is listed in PERMITTED but does not exist"));
        if !source.lines().any(is_unsafe_site) {
            stale.push(*name);
        }
    }
    assert!(
        stale.is_empty(),
        "these no longer contain `unsafe` and should leave PERMITTED: {stale:?}"
    );
}

#[test]
fn the_detector_recognises_what_it_claims_to() {
    // The checks above are only worth their runtime if `is_unsafe_site` actually fires.
    // Proving it here means a refactor that broke the detector fails this test rather than
    // silently reporting a clean budget.
    assert!(is_unsafe_site("    unsafe { alloc::alloc::alloc(layout) }"));
    assert!(is_unsafe_site(
        "pub(crate) unsafe fn release(ptr: *mut u8) {"
    ));
    assert!(is_unsafe_site("    unsafe extern \"C\" {"));

    // And does not fire on the things that are not uses of unsafe code.
    assert!(!is_unsafe_site("#[unsafe(no_mangle)]"));
    assert!(!is_unsafe_site("// SAFETY: unsafe { } in a comment"));
    assert!(!is_unsafe_site("/// mentions unsafe fn in a doc comment"));
    assert!(!is_unsafe_site("let x = 1;"));
}
