//! `eio service` — authoring a service file (SERVICE-SPEC §9.1).
//!
//! Every mutating command has the same shape, and it is SERVICE §9's: read the file, apply
//! exactly what was asked, re-run §7 stage 1, and only then write. A command that would leave
//! the file invalid changes nothing and says which rule it broke — see [`edit`], which is the
//! one place that sequence is written down.
//!
//! Nothing here contacts a node. `validate`'s stage 2 takes manifests as an argument because
//! §7 makes them an input rather than something a stage fetches, which is what lets this
//! command work against a local `cargo eio build` with no registry reachable.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use eio_service::edit::Document;

/// What `eio service` can do (SERVICE §9.1).
#[derive(Debug, Subcommand)]
pub enum Service {
    /// Write a new service file with a name and nothing else.
    New(New),
    /// Add a block instance, minting its id (SERVICE §2).
    AddBlock(AddBlock),
    /// Remove a block instance and the connections that name it.
    RemoveBlock(RemoveBlock),
    /// Connect an output terminal to an input terminal.
    Connect(Wire),
    /// Remove a connection.
    Disconnect(Wire),
    /// Set a property to an expression (ABI §11).
    SetProp(SetProp),
    /// Remove a configured property, leaving the block to take its manifest default. Succeeds
    /// whether or not the property was set (SERVICE §9), and says which happened.
    UnsetProp(UnsetProp),
    /// Change a block instance's label. Its id, connections, properties and `[ui]` are
    /// untouched — SERVICE §9 requires that, because remove-and-re-add would change the id
    /// and DAEMON §10 keys the state store by id.
    SetName(SetName),
    /// Clear a block instance's label. `name` is OPTIONAL (SERVICE §6), so this removes the
    /// key rather than emptying it, and succeeds whether or not there was one.
    UnsetName(UnsetName),
    /// Set whether the daemon starts this service at boot (DAEMON §3).
    SetAutostart(SetAutostart),
    /// Render the graph with names resolved.
    Show(FileArg),
    /// Run SERVICE §7's two stages and report what each found.
    Validate(Validate),
}

/// A command that names a service file and nothing else.
#[derive(Debug, Args)]
pub struct FileArg {
    /// The service file.
    file: PathBuf,
}

/// `eio service new`'s arguments.
#[derive(Debug, Args)]
pub struct New {
    /// The service's name. It is also the file's stem (SERVICE §1).
    name: String,
    /// Where to write it. Defaults to the working directory.
    #[arg(long, default_value = ".")]
    dir: PathBuf,
    /// Start this service at boot (DAEMON §3).
    #[arg(long)]
    autostart: bool,
}

/// `eio service add-block`'s arguments.
#[derive(Debug, Args)]
pub struct AddBlock {
    /// The service file.
    file: PathBuf,
    /// The block reference the block manager resolves (SCOPE §3.6).
    #[arg(long)]
    block: String,
    /// A label for people. Optional, repeatable across instances, and meaningless to a host.
    #[arg(long)]
    name: Option<String>,
    /// The instance id. Minted if not given, which is the usual case (SERVICE §2).
    #[arg(long)]
    id: Option<String>,
    /// A property, as `name=expression`. Repeatable.
    #[arg(long = "prop", value_name = "NAME=EXPR")]
    props: Vec<String>,
}

/// `eio service remove-block`'s arguments.
#[derive(Debug, Args)]
pub struct RemoveBlock {
    /// The service file.
    file: PathBuf,
    /// The instance to remove.
    id: String,
}

/// A connection's two terminals: `eio service connect`'s and `disconnect`'s arguments.
#[derive(Debug, Args)]
pub struct Wire {
    /// The service file.
    file: PathBuf,
    /// The source terminal, as `id.port`. An output.
    from: String,
    /// The destination terminal, as `id.port`. An input.
    to: String,
}

/// `eio service set-prop`'s arguments.
#[derive(Debug, Args)]
pub struct SetProp {
    /// The service file.
    file: PathBuf,
    /// The instance.
    id: String,
    /// The property.
    property: String,
    /// The expression. A literal is a trivial expression (ABI §11).
    expression: String,
}

/// `eio service set-name`'s arguments.
#[derive(Debug, Args)]
pub struct SetName {
    /// The service file.
    file: PathBuf,
    /// The instance.
    id: String,
    /// The label. A label only: nothing resolves by it (SERVICE §2).
    label: String,
}

/// `eio service unset-name`'s arguments.
#[derive(Debug, Args)]
pub struct UnsetName {
    /// The service file.
    file: PathBuf,
    /// The instance.
    id: String,
}

/// `eio service unset-prop`'s arguments.
#[derive(Debug, Args)]
pub struct UnsetProp {
    /// The service file.
    file: PathBuf,
    /// The instance.
    id: String,
    /// The property.
    property: String,
}

/// `eio service set-autostart`'s arguments.
#[derive(Debug, Args)]
pub struct SetAutostart {
    /// The service file.
    file: PathBuf,
    /// Whether to start at boot.
    #[arg(value_parser = clap::value_parser!(bool))]
    autostart: bool,
}

/// `eio service validate`'s arguments.
#[derive(Debug, Args)]
pub struct Validate {
    /// The service file.
    file: PathBuf,
    /// A manifest for stage 2, as `block-ref=path` (SERVICE §7). Repeatable.
    ///
    /// Keyed by the reference the file writes, so two instances of one block share one
    /// manifest. `cargo eio build` writes the file this reads.
    #[arg(long = "manifest", value_name = "REF=PATH")]
    manifests: Vec<String>,
}

/// Runs one command.
pub fn run(command: Service) -> Result<()> {
    match command {
        Service::New(args) => new(args),
        Service::AddBlock(args) => add_block(args),
        Service::RemoveBlock(args) => edit(&args.file, |doc| {
            let mut said: Vec<String> = doc
                .remove_block(&args.id)?
                .iter()
                .map(|edge| format!("disconnected {edge}"))
                .collect();
            said.push(format!("removed {}", args.id));
            Ok(said)
        }),
        Service::Connect(args) => edit(&args.file, |doc| {
            doc.connect(&args.from, &args.to)?;
            Ok(vec![format!("connected {} -> {}", args.from, args.to)])
        }),
        Service::Disconnect(args) => edit(&args.file, |doc| {
            doc.disconnect(&args.from, &args.to)?;
            Ok(vec![format!("disconnected {} -> {}", args.from, args.to)])
        }),
        Service::SetProp(args) => edit(&args.file, |doc| {
            doc.set_prop(&args.id, &args.property, &args.expression)?;
            Ok(vec![format!(
                "{}.{} = {}",
                args.id, args.property, args.expression
            )])
        }),
        Service::UnsetProp(args) => edit(&args.file, |doc| {
            // SERVICE §9: unsetting a property is the OPTIONAL side of the removal rule, so an
            // already-unset property is not a refusal — it is a report of which happened, the
            // same way `remove_name` and `remove_ui` need none because they say only one thing.
            let removed = doc.remove_prop(&args.id, &args.property)?;
            Ok(vec![if removed {
                format!("unset {}.{}", args.id, args.property)
            } else {
                format!(
                    "{}.{} was already unset; nothing to do",
                    args.id, args.property
                )
            }])
        }),
        Service::SetName(args) => edit(&args.file, |doc| {
            doc.set_name(&args.id, &args.label)?;
            Ok(vec![format!("{} named {:?}", args.id, args.label)])
        }),
        Service::UnsetName(args) => edit(&args.file, |doc| {
            doc.remove_name(&args.id)?;
            Ok(vec![format!("{} has no label", args.id)])
        }),
        Service::SetAutostart(args) => edit(&args.file, |doc| {
            doc.set_autostart(args.autostart);
            Ok(vec![format!("autostart = {}", args.autostart)])
        }),
        Service::Show(args) => {
            let parsed = parse_file(&args.file)?;
            print!("{}", crate::show::render(&parsed));
            Ok(())
        }
        Service::Validate(args) => validate(args),
    }
}

/// Writes a new service file (SERVICE §9.1).
fn new(args: New) -> Result<()> {
    // The name before the filesystem. It is a property of the argument, so it is answerable
    // without consulting a disk — and checking existence first would answer `Kitchen` with
    // "already exists" on a case-insensitive filesystem, which names the wrong problem and
    // sends its author looking for a file they did not write.
    let mut document = Document::create(&args.name)?;
    if args.autostart {
        document.set_autostart(true);
    }

    // §1: the stem is the name, so the caller names the service and this decides the filename.
    // Offering both would be offering a way to disagree with §1 on the way in.
    let path = args.dir.join(format!("{}.toml", args.name));
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    save(&path, &document)?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Adds a block instance, minting its id unless one was supplied (SERVICE §2).
fn add_block(args: AddBlock) -> Result<()> {
    edit(&args.file, |document| {
        let id = match &args.id {
            Some(id) => id.clone(),
            // Enough bytes for sixteen attempts. `generate` takes its randomness from here
            // rather than sourcing it, so that a service file's contents do not depend on
            // which binary was linked against `eio-service`.
            None => {
                let mut random = [0_u8; 64];
                getrandom::fill(&mut random).context("no randomness to mint an id from")?;
                document
                    .mint_id(&random)
                    .context("could not mint an unused id; supply one with --id")?
            }
        };

        document.add_block(&id, args.name.as_deref(), &args.block)?;
        for property in &args.props {
            let (name, expression) = split_once(property, "--prop")?;
            document.set_prop(&id, name, expression)?;
        }

        // Reported because the next thing its author does is write a connection naming it —
        // which is exactly why `edit` holds this back until the block is on disk.
        Ok(vec![format!("{id}  {}", args.block)])
    })
}

/// SERVICE §7's two stages, and §1's stem rule, reported class by class.
fn validate(args: Validate) -> Result<()> {
    let text = read_text(&args.file)?;

    let parsed = match eio_service::parse(&text) {
        Ok(parsed) => {
            println!("stage 1: ok");
            Some(parsed)
        }
        Err(errors) => {
            report("stage 1", errors.iter().map(ToString::to_string));
            None
        }
    };

    // §1's rule sits beside stage 1 rather than inside it: it is checkable from the file
    // alone, but it needs the file's *path*, which `parse` deliberately does not take.
    let stem = args.file.file_stem().and_then(|stem| stem.to_str());
    let stem_disagrees = parsed
        .as_ref()
        .is_some_and(|parsed| Some(parsed.service.name.as_str()) != stem);
    if let Some(parsed) = parsed.as_ref() {
        if stem_disagrees {
            println!(
                "stem:    {:?} declares name {:?}; §1 requires them to match",
                stem.unwrap_or_default(),
                parsed.service.name
            );
        } else {
            println!("stem:    ok");
        }
    }

    let Some(parsed) = parsed else {
        println!("stage 2: not run (stage 1 failed)");
        bail!("{} is not a valid service file", args.file.display());
    };

    let manifests = load_manifests(&args.manifests)?;
    let errors = eio_service::validate(&parsed, |id| {
        let instance = parsed.service.instance(id)?;
        manifests
            .iter()
            .find(|(reference, _)| reference == &instance.block)
            .map(|(_, manifest)| manifest.clone())
    });

    // Deduped, because a manifest is supplied per *reference* and two instances of one block
    // are one thing to check — which is also what makes the count below the right comparison.
    let mut references: Vec<&str> = parsed
        .service
        .blocks
        .values()
        .map(|instance| instance.block.as_str())
        .collect();
    references.sort_unstable();
    references.dedup();

    // A block nobody supplied a manifest for was not checked, and saying so is what keeps a
    // partial stage 2 from reading as a complete one.
    let unchecked: Vec<&str> = references
        .iter()
        .copied()
        .filter(|reference| !manifests.iter().any(|(named, _)| named == reference))
        .collect();

    // The headline says how much of stage 2 ran, because §9.1's rule is about not letting a
    // partial stage read as a complete one — and a caller scanning for `stage 2: ok` reads the
    // headline, not the `not checked` lines under it.
    let checked = references.len() - unchecked.len();
    if !errors.is_empty() {
        report("stage 2", errors.iter().map(ToString::to_string));
    } else if references.is_empty() {
        // A service with no blocks is valid and has nothing to check (§3). Saying "not run"
        // would report a skip that did not happen.
        println!("stage 2: ok (no blocks)");
    } else if checked == 0 {
        println!("stage 2: not run (no manifest supplied for any block)");
    } else if unchecked.is_empty() {
        println!("stage 2: ok");
    } else {
        println!("stage 2: ok for {checked} of {} blocks", references.len());
    }
    for reference in &unchecked {
        println!("not checked: {reference} (no manifest supplied)");
    }

    if !errors.is_empty() || stem_disagrees {
        bail!("{} did not validate", args.file.display());
    }
    Ok(())
}

/// Reads every `REF=PATH` into the manifest it names.
fn load_manifests(arguments: &[String]) -> Result<Vec<(String, eio_manifest::Manifest)>> {
    arguments
        .iter()
        .map(|argument| {
            let (reference, path) = split_once(argument, "--manifest")?;
            let json = std::fs::read_to_string(path)
                .with_context(|| format!("reading the manifest for {reference} at {path}"))?;
            // The same parser a node validates with (ABI §11): a manifest this accepts and a
            // node rejects would make the command worth less than not running it.
            let manifest =
                eio_manifest::parse(&json).map_err(|error| anyhow::anyhow!("{path}: {error}"))?;
            Ok((String::from(reference), manifest))
        })
        .collect()
}

/// Prints a heading and every line under it.
fn report(stage: &str, errors: impl ExactSizeIterator<Item = String>) {
    let count = errors.len();
    println!(
        "{stage}: {count} error{}",
        if count == 1 { "" } else { "s" }
    );
    for error in errors {
        println!("  {error}");
    }
}

/// SERVICE §9's sequence, in the one place it is written down.
///
/// Read, apply, re-run stage 1, write — and on any refusal, return before anything is written.
/// Every mutating command goes through here, so "an edit that would make the file invalid MUST
/// fail and change nothing" is a property of this function rather than of each caller's care.
///
/// **`apply` returns what to say rather than saying it**, and that is not a style choice. The
/// refusal can come *after* the edit was applied in memory — `check` runs last — so a command
/// that printed on its way through would announce an edit that is not on disk. `add-block` is
/// the case that makes it matter: it prints a minted id precisely so its author can wire it up
/// next, and an id printed for a block that was never written sends them to a `connect` that
/// cannot work. Nothing reaches stdout until the bytes have reached the file.
fn edit(path: &Path, apply: impl FnOnce(&mut Document) -> Result<Vec<String>>) -> Result<()> {
    let text = read_text(path)?;
    let mut document = Document::parse(&text).map_err(|errors| invalid(path, &errors))?;
    let said = apply(&mut document)?;
    save(path, &document)?;
    for line in said {
        println!("{line}");
    }
    Ok(())
}

/// Reads a service file and runs stage 1 over it, for the commands that only look.
fn parse_file(path: &Path) -> Result<eio_service::Parsed> {
    let text = read_text(path)?;
    eio_service::parse(&text).map_err(|errors| invalid(path, &errors))
}

fn read_text(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

/// Re-runs stage 1 over what the editor rendered, then replaces the file — both `eio-service`'s
/// job now (SERVICE §9), so this command's atomicity is the crate's rather than a copy of it.
fn save(path: &Path, document: &Document) -> Result<()> {
    document.write(path).map_err(|error| match error {
        // The same wording `edit` and `parse_file` refuse with below, so a caller sees one
        // message for "not a valid service file" whichever command produced it.
        eio_service::edit::WriteError::Invalid(errors) => invalid(path, &errors),
        other => anyhow::anyhow!("{other}"),
    })
}

/// Every stage-1 error, one per line, under the file that carries them.
fn invalid(path: &Path, errors: &[eio_service::Error]) -> anyhow::Error {
    let lines: Vec<String> = errors.iter().map(|error| format!("  {error}")).collect();
    anyhow::anyhow!(
        "{} is not a valid service file\n{}",
        path.display(),
        lines.join("\n")
    )
}

/// Splits `NAME=VALUE` at the **first** `=`.
///
/// The first and not the last, because the value is often an expression and `(= $a 1)` is one
/// of them. Splitting anywhere else would make a comparison unwritable from the command line.
fn split_once<'a>(argument: &'a str, flag: &str) -> Result<(&'a str, &'a str)> {
    argument
        .split_once('=')
        .with_context(|| format!("{flag} takes `NAME=VALUE`, and {argument:?} has no `=`"))
}
