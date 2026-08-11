//! Parsing the `#[block(..)]` attribute and the struct it sits on (SDK §1.1).
//!
//! Everything here is *rejection*: what the grammar admits, and what it refuses with which
//! message. A macro's error messages are its user interface — a block author meets them
//! more often than they meet the documentation — so each one names the offending token and
//! says what was expected, rather than reporting that parsing failed.
//!
//! The names are validated against `eio-manifest`'s rules (ABI §11.1) at *expansion* time
//! rather than at build time, which is the whole point: ABI §11.1's patterns exist so that
//! one rule reaches every surface, and a block whose port name a host will refuse should
//! not compile in the first place.

use std::collections::BTreeSet;

use eio_manifest::{Capability as ManifestCapability, PORT_ERR_NAME, PropertyType};
use proc_macro2::Span;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Expr, ExprLit, Fields, ItemStruct, Lit, Meta, Token, Type};

/// A parsed `#[block(..)]` attribute plus the struct it annotates.
pub struct Block {
    /// The annotated struct, passed through unchanged.
    pub item: ItemStruct,
    /// `name = ".."` — the block's registry name (ABI §11.1).
    pub name: String,
    /// `version = ".."`, defaulting to the crate's own (ABI §11.1 requires SemVer).
    pub version: String,
    /// `description = ".."`, optional.
    pub description: Option<String>,
    /// `inputs(a, b)` — position is the port index (ABI §5.2).
    pub inputs: Vec<Port>,
    /// `outputs(a, b)` — position is the port index.
    pub outputs: Vec<Port>,
    /// `capabilities(state, timer)` — must equal the imported `eio:*` namespaces (ABI §4.3).
    pub capabilities: Vec<Capability>,
    /// `#[prop(..)]` fields, in declaration order — position is the `prop_id` (ABI §5.2).
    pub props: Vec<Prop>,
}

/// One declared port.
pub struct Port {
    /// The name as written, which is also the manifest's.
    pub name: String,
    /// The `PascalCase` variant name on the generated `In`/`Out` enum.
    pub variant: syn::Ident,
    /// Where it was written, so an error points at it.
    pub span: Span,
}

/// One declared capability (ABI §11.1's closed set).
pub struct Capability {
    /// The manifest's own type, so the set is never spelled twice.
    pub value: ManifestCapability,
    /// Where it was written.
    pub span: Span,
}

/// One `#[prop(..)]` field.
pub struct Prop {
    /// The field name, which is also the property name in the manifest.
    pub name: String,
    /// The field identifier, for generating the initializer.
    pub ident: syn::Ident,
    /// The field's declared type — `Prop<f64>` and so on.
    pub ty: Type,
    /// `ty = ".."` — the manifest's declared type (ABI §11.1's closed set).
    pub declared: PropertyType,
    /// `desc = ".."`, optional.
    pub description: Option<String>,
    /// `default = ".."` — an expression string (ABI §11.1).
    pub default: Option<String>,
    /// `required` — a bare flag.
    pub required: bool,
    /// Where the field was written.
    pub span: Span,
}

type Result<T> = syn::Result<T>;

/// Reads one of `eio-manifest`'s lowercase-serialized enums from a bare identifier.
///
/// Through `serde` rather than a `match`, so ABI §11.1's closed sets are never written
/// down twice: `Capability` and `PropertyType` already carry them, and a set spelled again
/// here would be free to fall behind a minor ABI bump that added to one.
fn closed_set<T>(ident: &syn::Ident, label: &str, all: &[T]) -> Result<T>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let text = ident.to_string();
    serde_json::from_value::<T>(serde_json::Value::String(text.clone())).map_err(|_| {
        let expected: Vec<String> = all.iter().map(spelling).collect();
        syn::Error::new(
            ident.span(),
            format!(
                "`{text}` is not a {label} (ABI §11.1); expected one of {}",
                expected.join(", ")
            ),
        )
    })
}

/// How `eio-manifest` spells a closed-set value in JSON, without hard-coding it.
fn spelling<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

impl Block {
    /// Parses `#[block(..)]` over `item`.
    pub fn parse(args: proc_macro2::TokenStream, item: ItemStruct) -> Result<Block> {
        let metas =
            syn::parse::Parser::parse2(Punctuated::<Meta, Token![,]>::parse_terminated, args)?;

        let mut name = None;
        let mut version = None;
        let mut description = None;
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        let mut capabilities = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();

        for meta in metas {
            let key = meta
                .path()
                .get_ident()
                .ok_or_else(|| syn::Error::new(meta.span(), "expected a bare argument name"))?
                .to_string();

            if !seen.insert(key.clone()) {
                // Not last-wins. ABI §11.1 refuses duplicate keys in a manifest for the
                // same reason: choosing one silently makes the document's meaning depend
                // on order.
                return Err(syn::Error::new(
                    meta.span(),
                    format!("`{key}` is given twice; each argument may appear at most once"),
                ));
            }

            match key.as_str() {
                "name" => name = Some(string_value(&meta, "name")?),
                "version" => version = Some(string_value(&meta, "version")?),
                "description" => description = Some(string_value(&meta, "description")?),
                "inputs" => inputs = ports(&meta, "inputs")?,
                "outputs" => outputs = ports(&meta, "outputs")?,
                "capabilities" => capabilities = caps(&meta)?,
                other => {
                    return Err(syn::Error::new(
                        meta.span(),
                        format!(
                            "unknown `#[block]` argument `{other}`; expected one of \
                             name, version, description, inputs, outputs, capabilities"
                        ),
                    ));
                }
            }
        }

        let name = name.ok_or_else(|| {
            syn::Error::new(
                item.ident.span(),
                "`#[block]` needs a `name = \"..\"` (ABI §11.1 makes it REQUIRED)",
            )
        })?;

        // The crate's own version, so a block author states it once in `Cargo.toml`. ABI
        // §11.1 requires SemVer, and cargo already requires it of `package.version`.
        let version = version.unwrap_or_else(|| {
            std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| String::from("0.0.0"))
        });

        let props = parse_props(&item)?;
        let block = Block {
            item,
            name,
            version,
            description,
            inputs,
            outputs,
            capabilities,
            props,
        };
        block.check()?;
        Ok(block)
    }

    /// ABI §11.1's rules, applied at expansion time.
    ///
    /// Every one of these is something a host would refuse at load. Refusing here means a
    /// block author learns it from `cargo build` instead of from a deploy.
    fn check(&self) -> Result<()> {
        for (label, ports) in [("input", &self.inputs), ("output", &self.outputs)] {
            let mut seen = BTreeSet::new();
            for port in ports {
                if port.name == PORT_ERR_NAME {
                    return Err(syn::Error::new(
                        port.span,
                        format!(
                            "`{PORT_ERR_NAME}` is a reserved port name (ABI §6.4, §11.1): every \
                             block has an error port by that name without declaring it, and a \
                             service file addresses it that way"
                        ),
                    ));
                }
                if !eio_manifest::is_port_name(&port.name) {
                    return Err(syn::Error::new(
                        port.span,
                        format!(
                            "`{}` is not a valid port name (ABI §11.1): lowercase alphanumeric, \
                             `_` and `-` inside, at most 64 bytes",
                            port.name
                        ),
                    ));
                }
                if !seen.insert(port.name.clone()) {
                    // Per-direction, because ABI §11.1 makes inputs and outputs separate
                    // namespaces: a block MAY have an input and an output sharing a name.
                    return Err(syn::Error::new(
                        port.span,
                        format!("duplicate {label} port `{}` (ABI §11.1)", port.name),
                    ));
                }
            }
        }

        let mut seen = BTreeSet::new();
        for capability in &self.capabilities {
            if !seen.insert(capability.value) {
                return Err(syn::Error::new(
                    capability.span,
                    format!(
                        "duplicate capability `{}` (ABI §11.1)",
                        spelling(&capability.value)
                    ),
                ));
            }
        }

        let mut seen = BTreeSet::new();
        for prop in &self.props {
            if !eio_manifest::is_port_name(&prop.name) {
                return Err(syn::Error::new(
                    prop.span,
                    format!(
                        "`{}` is not a valid property name (ABI §11.1): lowercase alphanumeric, \
                         `_` and `-` inside, at most 64 bytes",
                        prop.name
                    ),
                ));
            }
            if !seen.insert(prop.name.clone()) {
                return Err(syn::Error::new(
                    prop.span,
                    format!("duplicate property `{}` (ABI §11.1)", prop.name),
                ));
            }
        }

        if !eio_manifest::is_ref_name(&self.name) {
            return Err(syn::Error::new(
                self.item.ident.span(),
                format!(
                    "`{}` is not a valid block name (ABI §11.1): lowercase alphanumeric, \
                     `.`, `_` and `-` inside, at most 64 bytes",
                    self.name
                ),
            ));
        }

        Ok(())
    }
}

fn string_value(meta: &Meta, key: &str) -> Result<String> {
    let Meta::NameValue(pair) = meta else {
        return Err(syn::Error::new(
            meta.span(),
            format!("`{key}` takes a string: `{key} = \"..\"`"),
        ));
    };
    match &pair.value {
        Expr::Lit(ExprLit {
            lit: Lit::Str(text),
            ..
        }) => Ok(text.value()),
        other => Err(syn::Error::new(
            other.span(),
            format!("`{key}` takes a string literal"),
        )),
    }
}

/// `inputs(a, b)` — a list of bare identifiers, in port-index order.
fn ports(meta: &Meta, key: &str) -> Result<Vec<Port>> {
    let Meta::List(list) = meta else {
        return Err(syn::Error::new(
            meta.span(),
            format!("`{key}` takes a list: `{key}(one, two)`"),
        ));
    };
    let idents = list.parse_args_with(Punctuated::<syn::Ident, Token![,]>::parse_terminated)?;
    Ok(idents
        .into_iter()
        .map(|ident| Port {
            name: ident.to_string(),
            variant: pascal(&ident),
            span: ident.span(),
        })
        .collect())
}

fn caps(meta: &Meta) -> Result<Vec<Capability>> {
    let Meta::List(list) = meta else {
        return Err(syn::Error::new(
            meta.span(),
            "`capabilities` takes a list: `capabilities(state, timer)`, or `capabilities()`",
        ));
    };
    let idents = list.parse_args_with(Punctuated::<syn::Ident, Token![,]>::parse_terminated)?;
    idents
        .into_iter()
        .map(|ident| {
            Ok(Capability {
                value: closed_set(&ident, "capability", &ManifestCapability::ALL)?,
                span: ident.span(),
            })
        })
        .collect()
}

/// `snake_case` → `PascalCase`, for the generated port enum variants.
fn pascal(ident: &syn::Ident) -> syn::Ident {
    let mut out = String::new();
    let mut upper = true;
    for character in ident.to_string().chars() {
        if character == '_' || character == '-' {
            upper = true;
        } else if upper {
            out.extend(character.to_uppercase());
            upper = false;
        } else {
            out.push(character);
        }
    }
    syn::Ident::new(&out, ident.span())
}

/// Reads the `#[prop(..)]` fields, in declaration order (ABI §5.2: position is `prop_id`).
fn parse_props(item: &ItemStruct) -> Result<Vec<Prop>> {
    let Fields::Named(fields) = &item.fields else {
        return Err(syn::Error::new(
            item.fields.span(),
            "`#[block]` needs a struct with named fields; a property's name is its field name",
        ));
    };

    let mut props = Vec::new();
    for field in &fields.named {
        let Some(attr) = field.attrs.iter().find(|attr| attr.path().is_ident("prop")) else {
            // A field without `#[prop]` is the block's own state. It keeps no `prop_id`
            // and never reaches the manifest.
            continue;
        };
        let ident = field.ident.clone().expect("named fields have identifiers");

        let mut declared = None;
        let mut description = None;
        let mut default = None;
        let mut required = false;

        let metas = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
        for meta in metas {
            let key = meta
                .path()
                .get_ident()
                .ok_or_else(|| syn::Error::new(meta.span(), "expected a bare argument name"))?
                .to_string();
            match key.as_str() {
                "ty" => {
                    let text = string_value(&meta, "ty")?;
                    let ident = syn::Ident::new(&sanitize(&text), meta.span());
                    declared = Some(closed_set(&ident, "property type", &PropertyType::ALL)?);
                }
                "desc" => description = Some(string_value(&meta, "desc")?),
                "default" => default = Some(string_value(&meta, "default")?),
                "required" => required = true,
                other => {
                    return Err(syn::Error::new(
                        meta.span(),
                        format!(
                            "unknown `#[prop]` argument `{other}`; expected one of \
                             ty, desc, default, required"
                        ),
                    ));
                }
            }
        }

        let declared = declared.ok_or_else(|| {
            syn::Error::new(
                attr.span(),
                "`#[prop]` needs a `ty = \"..\"` (ABI §11.1 makes a property's type REQUIRED)",
            )
        })?;

        props.push(Prop {
            name: ident.to_string(),
            ident,
            ty: field.ty.clone(),
            declared,
            description,
            default,
            required,
            span: field.span(),
        });
    }
    Ok(props)
}

/// A `ty = ".."` string as an identifier, so [`closed_set`] can report it with a span.
///
/// A value that is not an identifier at all cannot be one of ABI §11.1's types, so
/// replacing the offending characters loses nothing a caller could act on — the error
/// still names what was written, because `closed_set` reports the identifier it was given.
fn sanitize(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect();
    if cleaned.is_empty() || cleaned.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}
