//! Instance ids: what one may look like, and how tooling mints one (SERVICE-SPEC §2).

/// SERVICE §2.1's pattern, published as a regex so every surface enforces the same rule.
///
/// **It is ABI §11.1's port-and-property pattern, re-exported rather than restated.** SERVICE
/// §2.1 does not say an id is *shaped like* a port name, it says it is the same rule for the
/// same two reasons — both are TOML bare keys, and both exclude `.` because `id.port` is how
/// a connection addresses a terminal. A second copy of the string would be a second thing to
/// keep in step, which is precisely what ABI §11.1 publishes a regex to avoid.
///
/// The consequence is deliberate: an id and a port name cannot drift apart, and the
/// connection grammar — which puts one on each side of a dot — stays parseable by
/// construction.
pub use eio_manifest::PORT_NAME_PATTERN as ID_PATTERN;

/// The longest an id may be, in bytes (SERVICE §2.1). ABI §11.1's bound, for the same reason.
pub use eio_manifest::MAX_NAME_BYTES as MAX_ID_BYTES;

/// How many characters [`generate`] emits.
///
/// Four, from a 32-symbol alphabet: about a million ids, which is enough that a service with
/// a hundred blocks collides with probability under a percent — and the generator checks the
/// file anyway, so a collision costs a retry rather than a bug. Short because an id is
/// written by hand in every connection that touches the block, and read by nobody.
pub const GENERATED_LEN: usize = 4;

/// The alphabet [`generate`] draws from.
///
/// Crockford-shaped: no `i`, `l`, `o` or `u`. The first three are read back wrong from a
/// screen and the fourth is dropped so a generated id cannot spell an unfortunate word.
const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Whether `id` satisfies SERVICE §2.1.
///
/// `eio-manifest`'s validator, under the name this crate's readers are looking for. That
/// crate already proves its hand-written check equivalent to [`ID_PATTERN`] with a real
/// regex engine, and re-proving it here would test the same function twice — what this
/// crate's tests add is that the *published schema* carries the same string.
pub use eio_manifest::is_port_name as is_id;

/// Mints an id from `random`, avoiding anything in `taken`.
///
/// Takes its randomness rather than sourcing it: this crate is read by the daemon, the CLI
/// and the Designer's backend, and a function that reached for an RNG would make a service
/// file's contents depend on which of them was linked. The caller passes bytes; whether they
/// came from `getrandom` or from a test's fixed array is the caller's business.
///
/// Returns `None` when `random` runs out before an unused id is found — a caller that hands
/// over 64 bytes gets 16 attempts, and a service with enough blocks to exhaust that has
/// other problems.
pub fn generate<'a>(random: &[u8], taken: impl Fn(&str) -> bool + 'a) -> Option<String> {
    for chunk in random.chunks_exact(GENERATED_LEN) {
        let id: String = chunk
            .iter()
            .map(|byte| ALPHABET[*byte as usize % ALPHABET.len()] as char)
            .collect();
        // A generated id starting with a digit is still a valid id (§2.1 admits one), so the
        // only thing to check is whether it is free.
        if !taken(&id) {
            return Some(id);
        }
    }
    None
}
