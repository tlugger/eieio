//! The template `cargo eio new` writes (SDK-SPEC §5.1).
//!
//! The files are `include_str!`-embedded rather than fetched, generated from a schema, or
//! rendered by a template engine: a block author's first experience of the platform should
//! not depend on a network, and a template a reviewer cannot read in the repository is one
//! nobody reviews. Substitution is four names and two dependency lines, which is why it is
//! a `replace` and not a dependency.

use std::path::Path;

/// The names one block name yields, each in the form its file needs.
///
/// Kept together because they are one decision: `my-block` is the block's registry name
/// (ABI §11.1), the cargo package name, `my_block` as a crate path, and `MyBlock` as a type.
/// Deriving them at each use site is how three of them end up disagreeing.
#[derive(Debug)]
pub struct Names {
    /// The block's name, as ABI §11.1 constrains it, and the cargo package name.
    pub name: String,
    /// The name cargo gives the library target and the emitted `.wasm`.
    pub lib: String,
    /// The block struct's type name.
    pub type_name: String,
}

impl Names {
    /// Derives every form from the block's name.
    ///
    /// The caller has already checked the name against ABI §11.1 — see [`crate::new`].
    pub fn new(name: &str) -> Names {
        Names {
            name: name.to_string(),
            lib: name.replace('-', "_"),
            type_name: pascal_case(name),
        }
    }
}

/// `my-block` → `MyBlock`.
fn pascal_case(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// One file of the template: where it goes, and what goes in it.
pub struct File {
    /// Where it lands, relative to the block repo's root.
    pub path: &'static str,
    /// Its contents, before substitution.
    pub source: &'static str,
}

/// Every file `cargo eio new` writes (SDK §5.1).
///
/// The source of `.gitignore` is `gitignore.in` for a mundane reason: a directory of dotfiles
/// is a directory whose contents `ls` does not show, and a template file nobody sees is a
/// template file nobody maintains. Where it *lands* is [`File::path`] like every other, and
/// `.cargo/config.toml` is `cargo-config.toml.in` for the same reason.
pub const FILES: [File; 8] = [
    File {
        path: "Cargo.toml",
        source: include_str!("../template/Cargo.toml.in"),
    },
    File {
        path: "src/lib.rs",
        source: include_str!("../template/lib.rs.in"),
    },
    File {
        path: "tests/native.rs",
        source: include_str!("../template/native.rs.in"),
    },
    File {
        path: "conformance/lifecycle.json",
        source: include_str!("../template/lifecycle.json.in"),
    },
    File {
        path: ".github/workflows/ci.yml",
        source: include_str!("../template/ci.yml.in"),
    },
    File {
        path: "README.md",
        source: include_str!("../template/README.md.in"),
    },
    File {
        path: ".gitignore",
        source: include_str!("../template/gitignore.in"),
    },
    File {
        path: ".cargo/config.toml",
        source: include_str!("../template/cargo-config.toml.in"),
    },
];

/// Renders one template file.
///
/// `sdk` and `test_host` are whole dependency lines rather than versions or paths, because
/// the two forms differ in shape and not only in value: `eio-sdk = "0.1.0"` against
/// `eio-sdk = { path = "..." }` (SDK §5.1).
pub fn render(source: &str, names: &Names, sdk: &str, test_host: &str) -> String {
    source
        .replace("{{name}}", &names.name)
        .replace("{{lib}}", &names.lib)
        .replace("{{struct}}", &names.type_name)
        .replace("{{sdk_dep}}", sdk)
        .replace("{{test_host_dep}}", test_host)
}

/// A `name = { path = "..." }` dependency line, with the path spelled for TOML.
///
/// Backslashes and quotes are escaped rather than assumed absent: a Windows checkout path is
/// full of the former, and a path that broke the manifest it was written into would be a
/// confusing way to learn that.
pub fn path_dependency(name: &str, path: &Path) -> String {
    let path = path
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("{name} = {{ path = \"{path}\" }}")
}
