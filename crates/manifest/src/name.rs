//! The name and version rules of ABI-SPEC §11.1.
//!
//! Each rule appears twice: as a regex constant, and as a validator. The regex is
//! not decoration — it is what `manifest.schema.json` publishes as `pattern`, and
//! what the SDK, `cargo eio`, and the Designer's forms validate against, so that
//! every surface enforces one rule instead of its own approximation. The validators
//! here and those regexes MUST accept exactly the same strings; `tests/name.rs`
//! holds the table that keeps them honest.
//!
//! Regexes rather than a regex *engine*: matching a 64-byte name against a fixed
//! pattern is a handful of character-class tests, and pulling a regex crate into a
//! `no_std` leaf build to do it would be a poor trade.

/// Longest name this crate accepts, in bytes (ABI §11.1).
///
/// Bytes, not characters: every accepted name is ASCII, so the two coincide, and
/// the bound stays meaningful for a host budgeting memory rather than glyphs.
pub const MAX_NAME_BYTES: usize = 64;

/// Pattern for a block `name` and for `targets`/`aot` entries (ABI §11.1).
///
/// Admits `.` because these are registry-reference and target-triple components
/// (SCOPE §3.6): `wasm32-unknown-unknown` and `example.com` shaped names both have
/// to fit.
pub const REF_NAME_PATTERN: &str = "^[a-z0-9]([a-z0-9._-]*[a-z0-9])?$";

/// Pattern for port and property names (ABI §11.1).
///
/// Identical to [`REF_NAME_PATTERN`] minus `.`, because service files address
/// connections as `from.port -> to.port` and carry property names as TOML bare
/// keys (DAEMON §2) — a dot is ambiguous in both.
pub const PORT_NAME_PATTERN: &str = "^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$";

/// Pattern for `version`: Semantic Versioning 2.0.0, as published by semver.org.
///
/// Reproduced here so `manifest.schema.json` can carry it verbatim. [`is_version`]
/// is the implementation and accepts exactly this language.
pub const VERSION_PATTERN: &str = concat!(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)",
    r"(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)",
    r"(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?",
    r"(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$",
);

/// Whether `name` satisfies [`REF_NAME_PATTERN`] and [`MAX_NAME_BYTES`].
pub fn is_ref_name(name: &str) -> bool {
    is_name(name, true)
}

/// Whether `name` satisfies [`PORT_NAME_PATTERN`] and [`MAX_NAME_BYTES`].
pub fn is_port_name(name: &str) -> bool {
    is_name(name, false)
}

/// The shared shape of both name patterns: lowercase alphanumeric at each end,
/// separators only in between.
///
/// Leading and trailing separators are excluded so that a name is never ambiguous
/// when concatenated — `a.` followed by a port name would read as two dots.
fn is_name(name: &str, allow_dot: bool) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return false;
    }

    let separator = |b: u8| b == b'_' || b == b'-' || (allow_dot && b == b'.');
    let alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();

    let bytes = name.as_bytes();
    if !alnum(bytes[0]) || !alnum(bytes[bytes.len() - 1]) {
        return false;
    }
    bytes.iter().all(|&b| alnum(b) || separator(b))
}

/// Whether `version` is a Semantic Versioning 2.0.0 string, per
/// [`VERSION_PATTERN`].
///
/// Ordering and precedence are not this crate's concern: a manifest declares one
/// version, and comparing two of them is the registry's and the block manager's
/// job (DAEMON §4).
pub fn is_version(version: &str) -> bool {
    // Build metadata first: it is the only part that may contain a `+`, and it
    // imposes the weakest rules, so peeling it off leaves the strict remainder.
    let (rest, build) = match version.split_once('+') {
        Some((rest, build)) => (rest, Some(build)),
        None => (version, None),
    };
    if let Some(build) = build
        && !dot_separated(build, is_build_identifier)
    {
        return false;
    }

    // The version core contains no `-`, so the first one starts the pre-release.
    let (core, pre) = match rest.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (rest, None),
    };
    if let Some(pre) = pre
        && !dot_separated(pre, is_pre_release_identifier)
    {
        return false;
    }

    let mut parts = core.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(major), Some(minor), Some(patch), None)
            if is_numeric_identifier(major)
                && is_numeric_identifier(minor)
                && is_numeric_identifier(patch)
    )
}

/// Whether `text` is a non-empty `.`-separated list whose every element satisfies
/// `element`. An empty element (`1.0.0-a..b`) fails, since `split` yields it.
fn dot_separated(text: &str, element: fn(&str) -> bool) -> bool {
    !text.is_empty() && text.split('.').all(element)
}

/// A semver numeric identifier: digits, no leading zero unless the whole thing is
/// `0`.
fn is_numeric_identifier(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'))
}

/// A semver pre-release identifier: alphanumerics and hyphens, and if it is
/// all-digits it must also be a valid numeric identifier (no leading zeros).
fn is_pre_release_identifier(text: &str) -> bool {
    if !is_build_identifier(text) {
        return false;
    }
    if text.bytes().all(|b| b.is_ascii_digit()) {
        return is_numeric_identifier(text);
    }
    true
}

/// A semver build identifier: non-empty, ASCII alphanumerics and hyphens. Leading
/// zeros are explicitly allowed here (`1.0.0+001`).
fn is_build_identifier(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
