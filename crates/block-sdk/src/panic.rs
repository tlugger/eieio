//! The panic handler: report, then die (SDK §4, ABI §6 invariant 6, ABI §8).
//!
//! **Panics abort → trap → instance death.** The SDK's job is making panics rare in safe
//! code, not catching them: a panicking block has reached a state its author did not
//! anticipate, and ABI §8 reserves death for exactly that. Nothing here recovers.
//!
//! # Why it logs first
//!
//! A trap reaches the operator as a wasmtime backtrace of *function indices*. That says
//! where, in a numbering nobody reads, and never says why. The Rust panic message — the
//! file, the line, and "index out of bounds: the len is 3 but the index is 7" — exists at
//! the moment of the panic and is gone the instant the trap fires.
//!
//! So the handler formats it and calls `eio:core` `log` at level 4 before trapping. The
//! cost is real and is accepted deliberately: this is what pulls `core::fmt`'s formatting
//! machinery into every guest, and guests are measured in kilobytes on the leaf tier. The
//! alternative was a block that dies silently, which is the 2 a.m. mystery the platform's
//! error posture exists to prevent.
//!
//! # Why it cannot allocate
//!
//! A panic may be *from* the allocator, or from an out-of-memory condition that the next
//! allocation would hit again — and a panic inside a panic handler is an abort with no
//! message at all, which is strictly worse than the trap this is trying to improve on. So
//! the message is formatted into a fixed stack buffer, and a message longer than it is
//! truncated rather than grown.

// Compiled for the guest, which uses it, and for the host test build, which checks it.
// Nowhere else: on a bare-metal `check-nostd` leg there is no panic handler to serve, and
// an always-compiled buffer would be dead code that the lint gate is right to object to.
#[cfg(any(all(target_arch = "wasm32", target_os = "unknown"), test))]
mod buffer {
    /// How much of a panic message reaches the log.
    ///
    /// Stack, not heap, for the reason in the module docs. 256 bytes holds a file path, a
    /// line number and a typical message; the standard library's own panics are well under
    /// it.
    pub(super) const MESSAGE_CAPACITY: usize = 256;

    /// A [`core::fmt::Write`] sink over a fixed stack buffer that truncates instead of
    /// failing.
    ///
    /// Truncating is the right failure here: half a panic message still names the file and
    /// line, and no panic message names nothing.
    pub(super) struct Buffer {
        pub(super) bytes: [u8; MESSAGE_CAPACITY],
        pub(super) len: usize,
    }

    impl Buffer {
        pub(super) const fn new() -> Buffer {
            Buffer {
                bytes: [0; MESSAGE_CAPACITY],
                len: 0,
            }
        }

        pub(super) fn as_str(&self) -> &str {
            // `write_str` only ever copies whole UTF-8 characters, so the prefix is valid.
            core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("<panic message not UTF-8>")
        }
    }

    impl core::fmt::Write for Buffer {
        fn write_str(&mut self, text: &str) -> core::fmt::Result {
            // Back off to a character boundary rather than copying a character at a time:
            // `text` is already valid UTF-8, so decoding it only to re-encode the same
            // bytes is work with no product. What matters is that the buffer never ends
            // mid-character — `as_str` would fall back to its placeholder and lose the
            // whole message, which is the one thing this handler exists to prevent.
            let mut take = (self.bytes.len() - self.len).min(text.len());
            while take > 0 && !text.is_char_boundary(take) {
                take -= 1;
            }
            self.bytes[self.len..self.len + take].copy_from_slice(&text.as_bytes()[..take]);
            self.len += take;
            Ok(())
        }
    }
}

/// Reports the panic to the host, then traps (SDK §4).
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use buffer::Buffer;
    use core::fmt::Write;

    let mut message = Buffer::new();
    // The result is deliberately ignored: `Buffer::write_str` cannot fail, and there is
    // nothing this could do about it if it could — it is already on the way to a trap.
    let _ = write!(message, "block panicked: {info}");

    crate::raw::log(eio_abi::Level::Error, message.as_str());

    // ABI §1 invariant 6 and §8: a trap invalidates the instance. `unreachable` is the
    // WASM instruction that produces one, so the host sees the same death it would see
    // from any other trap — no new path, no status code, nothing recoverable.
    core::arch::wasm32::unreachable()
}

#[cfg(test)]
mod tests {
    use super::buffer::{Buffer, MESSAGE_CAPACITY};
    use core::fmt::Write;

    #[test]
    fn a_message_shorter_than_the_buffer_survives_whole() {
        let mut buffer = Buffer::new();
        let reason = "index out of bounds";
        write!(buffer, "block panicked: {reason}").unwrap();
        assert_eq!(buffer.as_str(), "block panicked: index out of bounds");
    }

    #[test]
    fn a_message_longer_than_the_buffer_is_truncated_rather_than_lost() {
        // The property that matters: an over-long message still says something, and the
        // prefix is the part that carries the file and line.
        let mut buffer = Buffer::new();
        for _ in 0..100 {
            write!(buffer, "0123456789").unwrap();
        }
        assert_eq!(buffer.as_str().len(), MESSAGE_CAPACITY);
        assert!(buffer.as_str().starts_with("0123456789"));
    }

    #[test]
    fn truncation_never_splits_a_character() {
        // The reason `write_str` backs off to a character boundary. A buffer cut
        // mid-character would make `as_str` fall back to its placeholder and lose the
        // whole message, which is exactly what this handler exists to avoid.
        for filler in 0..8 {
            let mut buffer = Buffer::new();
            for _ in 0..filler {
                write!(buffer, "a").unwrap();
            }
            // 3-byte characters do not divide the remaining space evenly for every filler.
            for _ in 0..MESSAGE_CAPACITY {
                write!(buffer, "€").unwrap();
            }
            assert!(
                core::str::from_utf8(&buffer.bytes[..buffer.len]).is_ok(),
                "filler {filler} left a split character"
            );
            assert_ne!(buffer.as_str(), "<panic message not UTF-8>");
        }
    }

    #[test]
    fn the_buffer_starts_empty() {
        assert_eq!(Buffer::new().as_str(), "");
    }
}
