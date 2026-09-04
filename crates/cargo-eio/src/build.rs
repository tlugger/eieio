//! `cargo eio build` — the guest build, and the validation that makes it worth running
//! (SDK-SPEC §5.2).

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, anyhow, bail};
use clap::Args;
use eio_manifest::PORTABLE_TARGET;
use serde::Deserialize;

/// SDK §5.2's profile, passed as cargo `--config` overrides.
///
/// `--config` and not a suggestion in the block's own manifest: config-level profile settings
/// override the manifest's, so a block cannot ship with `panic = "unwind"` by editing a file.
/// SDK §4's rule is not one a block author may opt out of on their own machine.
///
/// No `-C target-feature` of any kind: ABI §4.3's accepted set is exactly what rustc emits by
/// default, and the flag earlier drafts required here was measured to do nothing.
const PROFILE: [&str; 4] = [
    "profile.release.panic=\"abort\"",
    "profile.release.opt-level=\"z\"",
    "profile.release.lto=true",
    "profile.release.strip=true",
];

/// SDK §5.2's shadow-stack default, as a `build.rustflags` cargo `--config` override.
///
/// `wasm-ld` defaults the shadow stack to 1 MiB, which is not a size anybody chose for a
/// block: it is a desktop linker's default, and it is what made every one of ABI §13.2's
/// five golden blocks declare a **minimum linear memory of 17 pages, 1088 KiB** — three and
/// a half times the whole of LEAF §4.2's v1 target chip. 16 KiB takes all five to one page
/// with no source change and no measurable size difference.
///
/// `build.rustflags` and not `target.<triple>.rustflags` is the whole of the override story,
/// and it is deliberate: cargo resolves rustflags from the *first* source that has any, in
/// the order `RUSTFLAGS` env → `target.<triple>.rustflags` → `build.rustflags`. So a block
/// that genuinely needs a deeper stack raises it in its own `.cargo/config.toml` under
/// `[target.wasm32-unknown-unknown]` — which outranks this even though this arrives on the
/// command line, and which a plain `cargo build --release --target wasm32-unknown-unknown`
/// honours identically. That is the property SDK §5.1 asks of the template's
/// `[profile.release]`, kept for the one setting a `Cargo.toml` cannot carry.
const SHADOW_STACK: &str = "build.rustflags=[\"-C\", \"link-arg=-zstack-size=16384\"]";

/// The manifest file `build` writes beside the module (ABI §11, §4.4).
const MANIFEST_JSON: &str = "manifest.json";

/// `cargo eio build`'s arguments (SDK-SPEC §5.2).
#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Path to the block's `Cargo.toml`. Defaults to cargo's own search from here.
    #[arg(long, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,
}

/// What a build produced: the block repo's root, the module's own bytes and path, and its
/// validated manifest.
///
/// `publish` (§5) needs all four to package an OCI artifact, and derives none of them a
/// second way: the wasm bytes it hashes into a layer digest are the exact bytes this
/// validated, and the name and version it tags a push with are the exact ones `manifest.json`
/// was written from.
pub struct Built {
    /// The directory holding the `Cargo.toml` cargo compiled.
    ///
    /// From cargo's own artifact message rather than from `--manifest-path` or the working
    /// directory, which are two guesses at the same question; `cargo eio test` resolves
    /// `conformance/` against it.
    pub root: PathBuf,
    /// Where the built `.wasm` sits on disk.
    pub wasm_path: PathBuf,
    /// The module's bytes, already read once so a caller does not read them a second time.
    pub wasm: Vec<u8>,
    /// The manifest `validate_unaided` read out of the module — the same one written to
    /// `manifest.json` beside it.
    pub manifest: eio_manifest::Manifest,
}

/// Builds the block, validates the module, and writes its manifest.
pub fn run(args: &BuildArgs) -> anyhow::Result<Built> {
    let artifact = compile(args)?;

    let wasm =
        fs::read(&artifact.wasm).with_context(|| format!("reading {}", artifact.wasm.display()))?;

    // The same checks a host runs at load (ABI §4), through the same implementation: exports
    // and their signatures, imports within the declared capabilities, capability-paired
    // callbacks, the embedded manifest. This is what makes a module that builds a module a
    // node accepts — and a build-time approximation of it would be a second, drifting rule.
    //
    // `_unaided` because this command compiles nothing (§4.3): it prints `Built` and stops,
    // so anything the loader stays quiet about here is something nobody ever looks at. The
    // deploy path keeps the silence, because there an engine reads the module next and says
    // which proposal it objected to.
    let manifest = eio_manifest::validate_unaided(&wasm, None)
        .map_err(|error| anyhow!("{}: {error}", artifact.wasm.display()))?;

    let manifest_json = artifact.wasm.with_file_name(MANIFEST_JSON);
    fs::write(&manifest_json, manifest.to_json_pretty() + "\n")
        .with_context(|| format!("writing {}", manifest_json.display()))?;

    println!(
        "Built {} v{} ({} bytes)",
        manifest.name,
        manifest.version,
        wasm.len()
    );
    println!("    module   {}", artifact.wasm.display());
    println!("    manifest {}", manifest_json.display());
    // The declared minimum linear memory, printed on every build rather than checked
    // against a ceiling. `cargo eio build` produces ABI §11.1's *portable* module, which
    // both tiers run and which a daemon has no reason to size against a chip; the ceiling
    // is LEAF §4.2's, applied where a per-instance page budget is actually known. What
    // belongs here is the number — a block author who has raised the shadow stack, or whose
    // `RUSTFLAGS` displaced §5.2's default, sees the cost of it in the same breath as the
    // module's size instead of at somebody else's firmware build.
    //
    // A second walk of the module for one integer, which a build command can afford: the
    // alternative is threading it out of a validation whose subject is the manifest.
    if let Some(pages) = eio_manifest::Module::read(&wasm)
        .ok()
        .and_then(|module| module.min_pages)
    {
        println!(
            "    memory   {pages} page(s), {} KiB minimum",
            pages.saturating_mul(64)
        );
    }

    Ok(Built {
        root: artifact.root,
        wasm_path: artifact.wasm,
        wasm,
        manifest,
    })
}

/// Where cargo put the one `cdylib` it built.
struct Artifact {
    root: PathBuf,
    wasm: PathBuf,
}

/// Runs the build and finds its module.
fn compile(args: &BuildArgs) -> anyhow::Result<Artifact> {
    let mut command = cargo("build", args.manifest_path.as_deref());
    command
        .arg("--release")
        // ABI §1.1's guest target, named by the crate that owns what a manifest means:
        // `targets` MUST contain it (§11.1), so a second spelling here could disagree with
        // the manifest the same build validates against.
        .args(["--target", PORTABLE_TARGET])
        // Machine-readable artifacts, human-readable diagnostics: `json-render-diagnostics`
        // puts the rendered errors on stderr, where they reach the author unchanged, and
        // leaves stdout to say which files were produced.
        .arg("--message-format=json-render-diagnostics")
        .stdout(Stdio::piped());
    for setting in PROFILE {
        command.args(["--config", setting]);
    }
    command.args(["--config", SHADOW_STACK]);

    let output = command
        .spawn()
        .with_context(|| "running cargo build")?
        .wait_with_output()?;
    if !output.status.success() {
        // Cargo has already rendered why on stderr; repeating it here would only bury it.
        bail!("cargo build failed");
    }

    let mut artifacts = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(message) = serde_json::from_str::<Message>(line) else {
            continue;
        };
        if message.reason != "compiler-artifact" {
            continue;
        }
        let (Some(target), Some(manifest_path)) = (message.target, message.manifest_path) else {
            continue;
        };
        if !target.crate_types.iter().any(|kind| kind == "cdylib") {
            continue;
        }
        for filename in message.filenames {
            if Path::new(&filename)
                .extension()
                .is_some_and(|ext| ext == "wasm")
            {
                artifacts.push(Artifact {
                    root: Path::new(&manifest_path)
                        .parent()
                        .unwrap_or(Path::new("."))
                        .to_path_buf(),
                    wasm: PathBuf::from(&filename),
                });
            }
        }
    }

    match artifacts.len() {
        // A block is a cdylib: the `.wasm` is what a host loads, and an rlib-only crate has
        // produced nothing to load (SDK §5.1).
        0 => bail!(
            "the build produced no `.wasm`; a block's manifest needs `[lib] crate-type = [\"cdylib\", \"rlib\"]`"
        ),
        1 => Ok(artifacts.remove(0)),
        // One block per module (SDK §1) — the generated exports are `#[unsafe(no_mangle)]`
        // and the manifest section has a fixed name, so two would describe one module twice.
        _ => bail!(
            "the build produced {} `.wasm` modules: {}",
            artifacts.len(),
            artifacts
                .iter()
                .map(|artifact| artifact.wasm.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// A `cargo <subcommand>`, pointed at the block's manifest when one was named.
///
/// The cargo that invoked *this* subcommand, so a `+toolchain` or a rustup shim is honoured.
/// One function rather than two call sites, because `build` and `test` both have to say how a
/// block's manifest reaches cargo and there is no reason for them to say it differently.
pub fn cargo(subcommand: &str, manifest_path: Option<&Path>) -> Command {
    let program = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = Command::new(program);
    command.arg(subcommand);
    if let Some(path) = manifest_path {
        command.arg("--manifest-path").arg(path);
    }
    command
}

/// One line of cargo's `--message-format=json` stream, in the parts this reads.
#[derive(Debug, Deserialize)]
struct Message {
    reason: String,
    manifest_path: Option<String>,
    target: Option<Target>,
    #[serde(default)]
    filenames: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Target {
    crate_types: Vec<String>,
}
