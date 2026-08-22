//! Editing a service file without disturbing it (SERVICE-SPEC §9).
//!
//! # Why this is a separate parser
//!
//! [`crate::parse`] reads a service file into a value tree, which is what stage 1 validates and
//! what the daemon runs. A value tree is also exactly the wrong thing to write a file back
//! from: it has dropped every comment, every choice of alignment and every decision about
//! whether an array is one line or five. Rendering one would mean a Designer drag reformatting
//! a file a human wrote, and SERVICE §2's whole case for hand-editing being first class rests
//! on that not happening.
//!
//! So this module keeps a second representation — `toml_edit`'s, which holds the trivia — and
//! edits it in place. The diff of a file before and after an edit shows the edit and nothing
//! else, which is the contract SERVICE §9 states and DESIGNER §4 depends on.
//!
//! # The reader is still the authority
//!
//! Two TOML implementations in one crate is two chances to disagree about what a service file
//! is. They cannot, because this one never gets the last word: [`Document::check`] runs the
//! rendered text back through [`crate::parse`], so the writer is a text transformer that has to
//! satisfy the reader rather than a second opinion beside it. Every mutator below is tested
//! against that.
//!
//! # What a *new* value looks like
//!
//! Preservation is about text that is already there. A value this module writes for the first
//! time gets the underlying parser's spelling, which is TOML's shortest correct one: a string
//! property whose expression contains double quotes comes out `topic = '"kitchen.cold"'` rather
//! than SERVICE §4's escaped `topic = "\"kitchen.cold\""`. They are the same string, and the
//! reader says so. Forcing the escaped form would mean this crate owning an escaper so that a
//! generated file resembles an example, which is a worse trade than an unfamiliar quote.
//!
//! # Who edits
//!
//! The `eio service` CLI, the Designer through its backend, and an agent through either. **Not
//! the daemon** — DAEMON §9.3's `PUT` stores the bytes a client composed and edits none of
//! them, which is how SERVICE §2's "a host MUST NOT write to a service file" stays true of a
//! node while this module is true of everything else.
//!
//! ```
//! use eio_service::edit::Document;
//!
//! let mut doc = Document::parse(
//!     "# the kitchen\nname = \"kitchen\"\n\n[blocks.b7k2]\nblock = \"temp-sensor:1.0.0\"\n",
//! )
//! .expect("it parses");
//!
//! doc.add_block("f3m9", Some("Too cold?"), "filter:1.2.0").unwrap();
//! doc.connect("b7k2.out", "f3m9.in").unwrap();
//!
//! let text = doc.render();
//! assert!(text.starts_with("# the kitchen\n"), "the comment survived");
//! // SERVICE §5: a top-level key stays above the first table header.
//! assert!(text.find("connections").unwrap() < text.find("[blocks.").unwrap());
//! doc.check().expect("still a valid service file");
//! ```

use core::fmt;
use std::io;
use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, Item, Table, value};

use crate::connection::Connection;
use crate::error::Error;
use crate::parse::Parsed;
use crate::{id, parse};

/// What an edit can be refused for (SERVICE §9).
///
/// Distinct variants for the same reason [`Error`]'s are: a caller is told *which* rule it
/// broke, and a CLI or a canvas can point at the thing rather than print a sentence. These are
/// the editor's own preconditions — an edit that gets past them still has to satisfy
/// [`Document::check`], which is the format's word rather than this module's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// A service name that does not satisfy SERVICE §3.
    BadServiceName {
        /// What was asked for.
        name: String,
    },
    /// An instance id that does not satisfy SERVICE §2.1.
    BadId {
        /// What was asked for.
        id: String,
    },
    /// A port or property name that does not satisfy ABI §11.1.
    BadName {
        /// What was asked for.
        name: String,
    },
    /// A `block` reference that is empty (SERVICE §4).
    EmptyBlockRef,
    /// An id the file already defines. TOML would reject the duplicate key; this says so first.
    DuplicateInstance {
        /// The id.
        id: String,
    },
    /// An id the file does not define.
    NoSuchInstance {
        /// The id.
        id: String,
    },
    /// A property the instance does not configure.
    NoSuchProperty {
        /// The instance.
        id: String,
        /// The property.
        property: String,
    },
    /// A connection the file does not contain.
    NoSuchConnection {
        /// As it was asked for.
        edge: String,
    },
    /// The same edge twice (SERVICE §5).
    DuplicateConnection {
        /// As it was asked for.
        edge: String,
    },
    /// A terminal that is not `<id>.<port>` (SERVICE §5).
    BadTerminal {
        /// What was asked for.
        terminal: String,
        /// What was wrong with it.
        error: crate::ConnectionError,
    },
    /// `err` as a destination. It is an output port (ABI §6.4).
    ErrorPortDestination,
    /// A `[ui]` value that is not a TOML value (SERVICE §6).
    BadUiValue {
        /// What the TOML parser said.
        detail: String,
    },
    /// A `[ui]` path with no segments, or one that is not a bare key.
    BadUiPath,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::BadServiceName { name } => write!(
                f,
                "the service name {name:?} does not match {} (SERVICE §3)",
                id::ID_PATTERN
            ),
            EditError::BadId { id: bad } => write!(
                f,
                "the block instance id {bad:?} does not match {} (SERVICE §2.1)",
                id::ID_PATTERN
            ),
            EditError::BadName { name } => write!(
                f,
                "the name {name:?} does not match {} (ABI §11.1)",
                id::ID_PATTERN
            ),
            EditError::EmptyBlockRef => write!(f, "a block instance names no block (SERVICE §4)"),
            EditError::DuplicateInstance { id: taken } => {
                write!(f, "this service already defines {taken:?}")
            }
            EditError::NoSuchInstance { id: missing } => {
                write!(f, "this service defines no {missing:?}")
            }
            EditError::NoSuchProperty { id: on, property } => {
                write!(f, "{on:?} configures no property {property:?}")
            }
            EditError::NoSuchConnection { edge } => {
                write!(f, "this service has no connection {edge:?}")
            }
            EditError::DuplicateConnection { edge } => {
                write!(f, "this service already connects {edge:?}")
            }
            EditError::BadTerminal { terminal, error } => write!(f, "{terminal:?}: {error}"),
            EditError::ErrorPortDestination => write!(
                f,
                "`err` is an output port and cannot be a destination (ABI §6.4)"
            ),
            EditError::BadUiValue { detail } => write!(f, "not a TOML value: {detail}"),
            EditError::BadUiPath => write!(f, "a `[ui]` path is one or more bare keys"),
        }
    }
}

impl std::error::Error for EditError {}

/// Why [`Document::write`] refused, or could not finish (SERVICE §9).
///
/// One editor, one place this can fail from — which is the point: a caller that reimplemented
/// temp-file-plus-rename beside this crate would be free to get any of `NoFileName`, `Temporary`
/// or `Rename` wrong in a way this type cannot be.
#[derive(Debug)]
pub enum WriteError {
    /// What [`Document::render`] would have produced is not a valid service file. Carries
    /// [`Document::check`]'s own errors; nothing was written.
    ///
    /// SERVICE §9's rule is unconditional — "An edit that would make the file invalid MUST fail
    /// and change nothing" — so `write` runs `check` itself rather than trusting a caller to
    /// have run it already.
    Invalid(Vec<Error>),
    /// `path` names no file (it is `/`, or a bare prefix), so there is nowhere to put a
    /// temporary beside it. The original, if any, is untouched.
    NoFileName {
        /// The path that had none.
        path: PathBuf,
    },
    /// The temporary file could not be written. The original at `path`, if any, is untouched.
    Temporary {
        /// The temporary file's own path — not `path`, which was never opened.
        path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },
    /// The temporary file was written but could not be renamed into place. The original at
    /// `path` is untouched; the temporary is removed on a best-effort basis rather than left
    /// beside it.
    Rename {
        /// The path the write was replacing.
        path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Invalid(errors) => {
                write!(f, "not a valid service file")?;
                for error in errors {
                    write!(f, "\n  {error}")?;
                }
                Ok(())
            }
            WriteError::NoFileName { path } => write!(f, "{} names no file", path.display()),
            WriteError::Temporary { path, source } => {
                write!(f, "writing {}: {source}", path.display())
            }
            WriteError::Rename { path, source } => {
                write!(f, "replacing {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for WriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WriteError::Temporary { source, .. } | WriteError::Rename { source, .. } => {
                Some(source)
            }
            WriteError::Invalid(_) | WriteError::NoFileName { .. } => None,
        }
    }
}

/// A service file, open for editing (SERVICE §9).
///
/// Holds the text's own formatting, so every mutator below changes what it was asked to change
/// and leaves the rest of the file byte for byte as it found it — comments, key order,
/// alignment, blank lines, quoting, and `[ui]` (SERVICE §6).
#[derive(Debug, Clone)]
pub struct Document {
    doc: DocumentMut,
}

impl Document {
    /// Opens `text` for editing, if it is a valid service file.
    ///
    /// Stage 1 on the way in, because an editor is not a way to make an invalid file worse: a
    /// caller that cannot parse a file cannot meaningfully be told which of its edits failed.
    pub fn parse(text: &str) -> Result<Document, Vec<Error>> {
        // The reader first and on its own, so that a file this module would refuse and a file
        // the format refuses produce the same list.
        parse(text)?;
        match text.parse::<DocumentMut>() {
            Ok(doc) => Ok(Document { doc }),
            // Unreachable in practice — `parse` above ran the same text through a TOML parser
            // already — but the two are different parsers and this crate does not get to
            // assume they agree. Reported as the reader would report it.
            Err(error) => Err(vec![Error::Toml(error.to_string())]),
        }
    }

    /// A new service file with a name and nothing else.
    ///
    /// Which is a valid service: SERVICE §3 says a service with no blocks runs nothing, and
    /// that is what it says.
    pub fn create(name: &str) -> Result<Document, EditError> {
        if !id::is_id(name) {
            return Err(EditError::BadServiceName {
                name: String::from(name),
            });
        }
        let mut doc = DocumentMut::new();
        doc["name"] = value(name);
        Ok(Document { doc })
    }

    /// The file, as it would be written.
    pub fn render(&self) -> String {
        self.doc.to_string()
    }

    /// Runs SERVICE §7 stage 1 over what [`render`](Self::render) would produce.
    ///
    /// What a writer calls before it puts bytes on disk. Stage 1 needs nothing but the text, so
    /// there is no reason not to — and it is what makes SERVICE §9's "an edit that would make
    /// the file invalid MUST fail and change nothing" a checked property rather than a claim
    /// about this module's care.
    pub fn check(&self) -> Result<Parsed, Vec<Error>> {
        parse(&self.render())
    }

    /// Writes the file (SERVICE §9): checked, then replaced rather than truncated in place.
    ///
    /// Runs [`check`](Self::check) first, then writes the rendered text to a temporary file
    /// beside `path` and renames it into place. A reader of `path` — including a person who has
    /// it open in an editor — never observes a partially written file: the write is
    /// all-or-nothing, on every platform this runs on, and a write that fails at any step
    /// leaves `path` exactly as it was.
    ///
    /// This is the one place a writer gets SERVICE §9's guarantees from. The CLI and the
    /// Designer's backend (DESIGNER §4) both call this rather than each carrying their own
    /// temp-file-plus-rename, which is what keeps "the write is atomic" one implementation
    /// instead of two that can quietly disagree.
    ///
    /// # Why `check` runs unconditionally
    ///
    /// SERVICE §9 states the rule with no exception: "An edit that would make the file invalid
    /// MUST fail and change nothing." A caller that wants to inspect a possibly-invalid edit
    /// without committing it to disk already has [`render`](Self::render) and
    /// [`check`](Self::check) to do that with; `write` is the operation that puts the editor's
    /// guarantee on disk, so it is the one place that guarantee is not optional. Skipping the
    /// check for a caller that wanted to save a draft would mean a file `check` rejects can
    /// still reach disk under some other name for "write", which is exactly the footgun §9
    /// exists to close — a person's git checkout, or a node's autostart config, holding a
    /// service file that is invalid on its face.
    pub fn write(&self, path: &Path) -> Result<(), WriteError> {
        self.check().map_err(WriteError::Invalid)?;
        write_atomically(path, &self.render())
    }

    /// Mints an id this file does not already use ([`crate::id::generate`]).
    ///
    /// The randomness is the caller's, for the reason `generate` gives: a service file's
    /// contents must not depend on which binary was linked against this crate.
    pub fn mint_id(&self, random: &[u8]) -> Option<String> {
        id::generate(random, |candidate| self.block(candidate).is_some())
    }

    /// Whether the file defines `id`, and the table it is in.
    fn block(&self, id: &str) -> Option<&Item> {
        self.doc.get("blocks")?.as_table_like()?.get(id)
    }

    /// Adds a block instance (SERVICE §4).
    ///
    /// `name` is a label and MAY be omitted or repeated (SERVICE §2); `block` is the registry
    /// reference, whose grammar is the registry's and not this format's — all that is checked
    /// here is that there is one.
    pub fn add_block(
        &mut self,
        id: &str,
        name: Option<&str>,
        block: &str,
    ) -> Result<(), EditError> {
        if !crate::id::is_id(id) {
            return Err(EditError::BadId {
                id: String::from(id),
            });
        }
        if block.trim().is_empty() {
            return Err(EditError::EmptyBlockRef);
        }
        if self.block(id).is_some() {
            return Err(EditError::DuplicateInstance {
                id: String::from(id),
            });
        }

        let mut table = Table::new();
        // SERVICE §4's order: the label a person reads first, then what it runs.
        if let Some(name) = name {
            table["name"] = value(name);
        }
        table["block"] = value(block);
        blocks_mut(&mut self.doc)[id] = Item::Table(table);
        Ok(())
    }

    /// Removes a block instance and every connection that names it.
    ///
    /// The cascade is not a convenience: a connection naming an instance the file does not
    /// define is SERVICE §7's dangling-connection error, so the alternative is writing a file
    /// that will not load. Returns what was removed, so a caller can say so.
    ///
    /// `[ui]` is left alone. SERVICE §6 makes a stale annotation inert, and tidying it would be
    /// this crate deciding that `[ui]`'s keys are block ids — a schema §6 says the format does
    /// not have.
    pub fn remove_block(&mut self, id: &str) -> Result<Vec<String>, EditError> {
        if self.block(id).is_none() {
            return Err(EditError::NoSuchInstance {
                id: String::from(id),
            });
        }
        blocks_mut(&mut self.doc)
            .as_table_like_mut()
            .expect("blocks is a table")
            .remove(id);

        let mut removed = Vec::new();
        if let Some(array) = self.connections_mut() {
            array.retain(|entry| {
                // A non-string entry is not a connection this crate can read, and `check` will
                // say so about the whole file — removing it here would be this function
                // repairing something it was not asked about.
                let Some(text) = entry.as_str() else {
                    return true;
                };
                let touches = Connection::parse(text)
                    .is_ok_and(|edge| edge.from.instance == id || edge.to.instance == id);
                if touches {
                    removed.push(String::from(text));
                }
                !touches
            });
        }
        Ok(removed)
    }

    /// Sets a property to an expression (SERVICE §4, ABI §11).
    ///
    /// The value is an expression *string* — every property is an expression and a literal is a
    /// trivial one — and it is not parsed here: [`check`](Self::check) runs EXPR §10's static
    /// analysis over the whole file, which is one place that happens instead of two.
    pub fn set_prop(
        &mut self,
        id: &str,
        property: &str,
        expression: &str,
    ) -> Result<(), EditError> {
        if !eio_manifest::is_port_name(property) {
            return Err(EditError::BadName {
                name: String::from(property),
            });
        }
        let instance = self.instance_mut(id)?;
        instance
            .entry("props")
            .or_insert_with(|| Item::Table(Table::new()))[property] = value(expression);
        Ok(())
    }

    /// Removes a configured property, leaving the block to take its manifest default.
    pub fn remove_prop(&mut self, id: &str, property: &str) -> Result<(), EditError> {
        let instance = self.instance_mut(id)?;
        let gone = instance
            .get_mut("props")
            .and_then(Item::as_table_like_mut)
            .and_then(|props| props.remove(property))
            .is_some();
        if !gone {
            return Err(EditError::NoSuchProperty {
                id: String::from(id),
                property: String::from(property),
            });
        }
        Ok(())
    }

    /// Whether the service starts at boot (DAEMON §3).
    pub fn set_autostart(&mut self, autostart: bool) {
        self.doc["autostart"] = value(autostart);
    }

    /// Connects an output terminal to an input terminal (SERVICE §5).
    ///
    /// Each is `"<id>.<port>"`. Both are checked against the grammar the file's own connections
    /// are read with, so an edge this writes is one [`crate::parse`] will read back.
    pub fn connect(&mut self, from: &str, to: &str) -> Result<(), EditError> {
        let (edge, parsed) = self.edge(from, to)?;
        if parsed.to.port == eio_manifest::PORT_ERR_NAME {
            return Err(EditError::ErrorPortDestination);
        }
        for terminal in [&parsed.from, &parsed.to] {
            if self.block(&terminal.instance).is_none() {
                return Err(EditError::NoSuchInstance {
                    id: terminal.instance.clone(),
                });
            }
        }
        if self.find_connection(&parsed).is_some() {
            return Err(EditError::DuplicateConnection { edge });
        }

        let array = self.connections_or_insert();
        // The new entry wears the formatting the array already has. A multi-line array's
        // elements each carry their leading newline and indent as decor, so the indent is read
        // off the last one — copying its whole prefix would copy the comment above it too.
        let indent = array
            .len()
            .checked_sub(1)
            .and_then(|last| array.get(last))
            .and_then(|last| last.decor().prefix()?.as_str())
            .and_then(|prefix| prefix.rsplit_once('\n'))
            .map(|(_, indent)| format!("\n{indent}"));
        array.push(edge);
        if let Some(indent) = indent {
            let last = array.len() - 1;
            array
                .get_mut(last)
                .expect("just pushed")
                .decor_mut()
                .set_prefix(indent);
        }
        Ok(())
    }

    /// Removes an edge, whatever whitespace the file happened to write it with.
    pub fn disconnect(&mut self, from: &str, to: &str) -> Result<(), EditError> {
        let (edge, parsed) = self.edge(from, to)?;
        let Some(index) = self.find_connection(&parsed) else {
            return Err(EditError::NoSuchConnection { edge });
        };
        self.connections_mut()
            .expect("a connection was found in it")
            .remove(index);
        Ok(())
    }

    /// Sets an annotation under `[ui]`, creating the path (SERVICE §6, DESIGNER §4).
    ///
    /// `fragment` is a TOML value — `"{ x = 148, y = 234 }"`, `"1.5"`, `"\"a note\""` — because
    /// `[ui]` has no schema here and never will. Taking a value this crate had a type for would
    /// be this crate having an opinion about the Designer's layout format, which §6 forbids the
    /// daemon and this module has no better claim to.
    pub fn set_ui(&mut self, path: &[&str], fragment: &str) -> Result<(), EditError> {
        let (last, parents) = path.split_last().ok_or(EditError::BadUiPath)?;
        if path.iter().any(|segment| !is_bare_key(segment)) {
            return Err(EditError::BadUiPath);
        }
        let parsed =
            fragment
                .parse::<toml_edit::Value>()
                .map_err(|error| EditError::BadUiValue {
                    detail: error.to_string(),
                })?;

        let mut table = self
            .doc
            .entry("ui")
            .or_insert_with(implicit_table)
            .as_table_like_mut()
            .ok_or(EditError::BadUiPath)?;
        for segment in parents {
            table = table
                .entry(segment)
                .or_insert_with(implicit_table)
                .as_table_like_mut()
                .ok_or(EditError::BadUiPath)?;
        }
        table.insert(last, value(parsed));
        Ok(())
    }

    /// Removes an annotation. Absent is not an error: a caller clearing a layout entry for a
    /// block that never had one has got what it asked for.
    pub fn remove_ui(&mut self, path: &[&str]) -> Result<(), EditError> {
        let (last, parents) = path.split_last().ok_or(EditError::BadUiPath)?;
        let mut table = match self.doc.get_mut("ui").and_then(Item::as_table_like_mut) {
            Some(table) => table,
            None => return Ok(()),
        };
        for segment in parents {
            table = match table.get_mut(segment).and_then(Item::as_table_like_mut) {
                Some(table) => table,
                None => return Ok(()),
            };
        }
        table.remove(last);
        Ok(())
    }

    /// `id`'s table, or why there is not one.
    fn instance_mut(&mut self, id: &str) -> Result<&mut dyn toml_edit::TableLike, EditError> {
        self.doc
            .get_mut("blocks")
            .and_then(Item::as_table_like_mut)
            .and_then(|blocks| blocks.get_mut(id))
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| EditError::NoSuchInstance {
                id: String::from(id),
            })
    }

    /// The `connections` array, if the file has one.
    fn connections_mut(&mut self) -> Option<&mut Array> {
        self.doc.get_mut("connections")?.as_array_mut()
    }

    /// The `connections` array, creating it if the file has none.
    ///
    /// SERVICE §5 requires it above the first table header, and `toml_edit` renders a root
    /// key-value above every sub-table whatever order it was inserted in — so this is correct
    /// by construction rather than by an insertion this function is careful about. Pinned by a
    /// test, because it is a property of the library rather than of this code.
    fn connections_or_insert(&mut self) -> &mut Array {
        self.doc
            .entry("connections")
            .or_insert_with(|| value(Array::new()))
            .as_array_mut()
            .expect("connections is an array: `check` refuses a document where it is not")
    }

    /// Renders and parses `<from> -> <to>`, so a written edge and a read one use one grammar.
    fn edge(&self, from: &str, to: &str) -> Result<(String, Connection), EditError> {
        let edge = format!("{from} -> {to}");
        match Connection::parse(&edge) {
            Ok(parsed) => Ok((edge, parsed)),
            // The failure is reported against the half that caused it rather than against the
            // string this function invented, which a caller never wrote and cannot fix.
            Err(error) => {
                let terminal = match Connection::parse(&format!("{from} -> x.y")) {
                    Err(_) => from,
                    Ok(_) => to,
                };
                Err(EditError::BadTerminal {
                    terminal: String::from(terminal),
                    error,
                })
            }
        }
    }

    /// Where `edge` sits in `connections`, comparing what the strings *mean*.
    ///
    /// SERVICE §5 lets any amount of whitespace sit around the arrow, so `"a.out->b.in"` and
    /// `"a.out  ->  b.in"` are the same edge and a textual search would miss one of them.
    fn find_connection(&self, edge: &Connection) -> Option<usize> {
        self.doc
            .get("connections")?
            .as_array()?
            .iter()
            .position(|entry| {
                entry
                    .as_str()
                    .and_then(|text| Connection::parse(text).ok())
                    .is_some_and(|other| {
                        (
                            &other.from.instance,
                            &other.from.port,
                            &other.to.instance,
                            &other.to.port,
                        ) == (
                            &edge.from.instance,
                            &edge.from.port,
                            &edge.to.instance,
                            &edge.to.port,
                        )
                    })
            })
    }
}

/// Replaces `path`, rather than truncating and rewriting it (SERVICE §9.1).
///
/// The file often belongs to a person who has it open in an editor, and a service file
/// half-written by a process that died partway through is worse than a write that failed
/// outright. A temporary file beside it and a rename makes the change all-or-nothing: a rename
/// is atomic on every filesystem this targets, and the temporary sits in the *same* directory
/// as `path` because a rename across filesystems is a copy, not an atom.
fn write_atomically(path: &Path, text: &str) -> Result<(), WriteError> {
    let name = path.file_name().ok_or_else(|| WriteError::NoFileName {
        path: path.to_path_buf(),
    })?;
    // A leading `.` and a suffix `toml_edit`'s own writer would never choose, so this can never
    // collide with a real service file sitting in the same directory.
    let temporary = path.with_file_name(format!(".{}.eio-tmp", name.to_string_lossy()));

    std::fs::write(&temporary, text).map_err(|source| WriteError::Temporary {
        path: temporary.clone(),
        source,
    })?;
    std::fs::rename(&temporary, path).map_err(|source| {
        // Best-effort: the rename already failed, and a stray temporary is a smaller problem
        // than reporting the wrong one because cleanup itself failed.
        let _ = std::fs::remove_file(&temporary);
        WriteError::Rename {
            path: path.to_path_buf(),
            source,
        }
    })
}

/// The `blocks` table, created implicit so a file with instances has no bare `[blocks]` header.
fn blocks_mut(doc: &mut DocumentMut) -> &mut Item {
    doc.entry("blocks").or_insert_with(implicit_table)
}

/// A table that renders only through its children — `[blocks.b7k2]`, never `[blocks]`.
fn implicit_table() -> Item {
    let mut table = Table::new();
    table.set_implicit(true);
    Item::Table(table)
}

/// Whether `key` can be written as a TOML bare key.
///
/// The `[ui]` path is spelled with bare keys because that is what the Designer's convention
/// uses (block ids, §6) and because a quoted key here would be this module choosing an escaping
/// on the caller's behalf.
fn is_bare_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}
