//! Reading a suite off disk (ABI-SPEC §13.1).
//!
//! Scenarios are data, so loading them is a file read and a JSON parse — and the paths inside
//! one are relative to the scenario file, so a suite can be copied or vendored without
//! rewriting them. A `.wat` module is assembled here rather than checked in as bytes: a
//! reviewer has to be able to see what a fixture exercises, since the fixture is the thing a
//! host failure will be blamed on.

use std::path::{Path, PathBuf};

use crate::host::Host;
use crate::report::Summary;
use crate::run::Loaded;

/// Where this crate's own suite lives.
///
/// Absolute, from `CARGO_MANIFEST_DIR`, so another crate's test can run the same files
/// without knowing where it was invoked from — which is what `crates/daemon`'s conformance
/// test does.
pub fn scenarios_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios")
}

/// Runs **this repository's own** suite against `host`, golden blocks built first.
///
/// The one entry point every host binding here uses, because the alternative was five call
/// sites each remembering to build the fixtures before naming them (ABI §13.2's golden
/// blocks are `eio-sdk` crates under `examples/blocks/`, not bytes checked in). A host
/// implemented elsewhere calls [`run_dir`] with its own directory and builds nothing.
pub fn run_own<H: Host>(host: &mut H) -> Result<Summary, String> {
    crate::golden::build();
    run_dir(&scenarios_dir(), host)
}

/// Reads one scenario and the module it names.
pub fn load(path: &Path) -> Result<Loaded, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let scenario: crate::Scenario =
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;

    let dir = path.parent().unwrap_or(Path::new("."));
    let wasm = read_module(&dir.join(&scenario.module))?;
    let registry = match &scenario.manifest {
        Some(relative) => {
            let path = dir.join(relative);
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            Some(
                eio_manifest::parse(&text)
                    .map_err(|error| format!("{}: {error}", path.display()))?,
            )
        }
        None => None,
    };
    Ok(Loaded {
        scenario,
        wasm,
        registry,
    })
}

/// Reads a `.wasm`, or assembles a `.wat`.
fn read_module(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if path.extension().is_some_and(|extension| extension == "wat") {
        let text =
            String::from_utf8(bytes).map_err(|error| format!("{}: {error}", path.display()))?;
        return wat::parse_str(&text).map_err(|error| format!("{}: {error}", path.display()));
    }
    Ok(bytes)
}

/// Every scenario in `dir`, in filename order, run against `host`.
///
/// Filename order rather than directory order: a suite has to run the same way twice, and a
/// filesystem's order is not a promise.
pub fn run_dir<H: Host>(dir: &Path, host: &mut H) -> Result<Summary, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|error| format!("{}: {error}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();
    if paths.is_empty() {
        return Err(format!("{} holds no scenarios", dir.display()));
    }

    let mut summary = Summary::default();
    for path in paths {
        let loaded = load(&path)?;
        summary.reports.push(crate::run(&loaded, host));
    }
    Ok(summary)
}
