//! A permissive CBOR **structure** reader, for one purpose only: letting
//! `cbor_vectors.rs` check that a §6.3.1 rejecting vector is well-formed CBOR consuming
//! the whole input, so that the rule it names is actually the only thing wrong with it
//! (eieio-7d8.30).
//!
//! `crates/expr/tests/support/vector_format.rs` warns against a second CBOR
//! implementation living in this repo, and that warning is right for a second *value*
//! decoder: two decoders can disagree about what bytes mean — is this float `+0.0` or
//! `-0.0`, are these map keys sorted — and a corpus checked against two disagreeing
//! decoders is a corpus that has quietly picked a spec of its own. This reader has no
//! such surface. It does not decode integers to a value, does not compare floats, does
//! not look at map key order or uniqueness, and does not enforce a single one of
//! §6.3.1's canonical rules — if it did, it would reject the very vectors it exists to
//! check. All it knows is RFC 8949 §3's grammar of heads, lengths, and indefinite-length
//! closes: "is this one complete, syntactically legal CBOR item with nothing left over".
//! That question has one answer regardless of which conforming decoder is asked, so a
//! second implementation of it carries none of the semantic-disagreement risk the value
//! warning is about.
//!
//! Rules 10 and 11 are deliberately outside what this checks: they are rules about
//! ill-formed and hostile-length bytes, so a vector written for them is *supposed* to
//! fail a well-formedness check, and `cbor_vectors.rs` does not run one against them.

/// True if `bytes` is exactly one well-formed CBOR data item, with no bytes left over.
///
/// "Well-formed" is RFC 8949's structural sense, not §6.3.1's canonical one: a
/// non-shortest integer head, a `NaN`, an unsorted or duplicated map key, and a tag are
/// all well-formed here even though a real decoder must refuse every one of them. That
/// gap is the point — this only rules out the *other* way a rejecting vector's bytes can
/// be broken.
pub fn well_formed_single_item(bytes: &[u8]) -> Result<(), String> {
    let mut pos = 0usize;
    item(bytes, &mut pos)?;
    if pos != bytes.len() {
        return Err(format!(
            "{} trailing byte(s) after one complete item ({pos} of {} consumed)",
            bytes.len() - pos,
            bytes.len(),
        ));
    }
    Ok(())
}

fn need(bytes: &[u8], pos: usize, n: usize) -> Result<(), String> {
    if pos + n > bytes.len() {
        return Err(format!(
            "truncated: need {n} more byte(s) at offset {pos}, only {} left",
            bytes.len().saturating_sub(pos)
        ));
    }
    Ok(())
}

/// Consumes and returns the next `n` bytes, or an error if fewer than `n` remain.
fn take<'b>(bytes: &'b [u8], pos: &mut usize, n: usize) -> Result<&'b [u8], String> {
    need(bytes, *pos, n)?;
    let s = &bytes[*pos..*pos + n];
    *pos += n;
    Ok(s)
}

/// A head's argument (RFC 8949 §3): an immediate 0-23, a following 1/2/4/8-byte value,
/// `None` for indefinite-length (additional info 31), or an error for the three
/// reserved encodings.
fn argument(bytes: &[u8], pos: &mut usize, additional: u8) -> Result<Option<u64>, String> {
    Ok(Some(match additional {
        0..=23 => additional as u64,
        24 => take(bytes, pos, 1)?[0] as u64,
        25 => u16::from_be_bytes(take(bytes, pos, 2)?.try_into().unwrap()) as u64,
        26 => u32::from_be_bytes(take(bytes, pos, 4)?.try_into().unwrap()) as u64,
        27 => u64::from_be_bytes(take(bytes, pos, 8)?.try_into().unwrap()),
        28..=30 => return Err(format!("reserved additional-info value {additional}")),
        31 => return Ok(None),
        _ => unreachable!("additional info is five bits"),
    }))
}

/// True, and consumes it, if the next byte is the indefinite-length break (`0xff`).
fn at_break(bytes: &[u8], pos: &mut usize) -> Result<bool, String> {
    need(bytes, *pos, 1)?;
    Ok(if bytes[*pos] == 0xff {
        *pos += 1;
        true
    } else {
        false
    })
}

/// A byte or text string (major types 2 and 3): one run of `len` bytes, or — for
/// indefinite length — a run of definite-length chunks of the same major type, closed
/// by a break (RFC 8949 §3.2.3).
fn string(bytes: &[u8], pos: &mut usize, additional: u8, major: u8) -> Result<(), String> {
    match argument(bytes, pos, additional)? {
        Some(len) => {
            let len = usize::try_from(len).map_err(|_| "string length overflows usize")?;
            need(bytes, *pos, len)?;
            *pos += len;
            Ok(())
        }
        None => {
            while !at_break(bytes, pos)? {
                need(bytes, *pos, 1)?;
                let chunk_head = bytes[*pos];
                if chunk_head >> 5 != major || chunk_head & 0x1f == 31 {
                    return Err(format!(
                        "indefinite-length string chunk is not a definite-length \
                         major-type-{major} item"
                    ));
                }
                item(bytes, pos)?;
            }
            Ok(())
        }
    }
}

/// One CBOR data item: a head (major type + argument) followed by whatever that major
/// type demands, recursing into nested items for containers and tags.
fn item(bytes: &[u8], pos: &mut usize) -> Result<(), String> {
    need(bytes, *pos, 1)?;
    let head = bytes[*pos];
    *pos += 1;
    let major = head >> 5;
    let additional = head & 0x1f;

    match major {
        // Unsigned / negative integer: head plus argument, no payload, no indefinite form.
        0 | 1 => {
            if additional == 31 {
                return Err("major type 0/1 cannot be indefinite-length".into());
            }
            argument(bytes, pos, additional)?;
            Ok(())
        }
        2 | 3 => string(bytes, pos, additional, major),
        // Array: `len` nested items, or an indefinite run closed by a break.
        4 => match argument(bytes, pos, additional)? {
            Some(len) => (0..len).try_for_each(|_| item(bytes, pos)),
            None => {
                while !at_break(bytes, pos)? {
                    item(bytes, pos)?;
                }
                Ok(())
            }
        },
        // Map: `len` key/value pairs, or an indefinite run closed by a break.
        5 => match argument(bytes, pos, additional)? {
            Some(len) => (0..len).try_for_each(|_| {
                item(bytes, pos)?; // key
                item(bytes, pos) // value
            }),
            None => {
                while !at_break(bytes, pos)? {
                    item(bytes, pos)?; // key
                    item(bytes, pos)?; // value
                }
                Ok(())
            }
        },
        // Tag: an uninterpreted tag number, always definite, then exactly one nested item.
        6 => {
            if additional == 31 {
                return Err("a tag number cannot be indefinite-length".into());
            }
            argument(bytes, pos, additional)?;
            item(bytes, pos)
        }
        // Simple values, floats, and the break marker.
        7 => match additional {
            0..=23 => Ok(()), // immediate simple value, incl. false/true/null/undefined
            24 => {
                need(bytes, *pos, 1)?;
                *pos += 1;
                Ok(())
            } // one-byte simple value
            25 => {
                need(bytes, *pos, 2)?;
                *pos += 2;
                Ok(())
            } // binary16
            26 => {
                need(bytes, *pos, 4)?;
                *pos += 4;
                Ok(())
            } // binary32
            27 => {
                need(bytes, *pos, 8)?;
                *pos += 8;
                Ok(())
            } // binary64
            28..=30 => Err(format!("reserved simple-value encoding {additional}")),
            31 => Err("unexpected break outside an indefinite-length item".into()),
            _ => unreachable!("additional info is five bits"),
        },
        _ => unreachable!("major type is three bits"),
    }
}
