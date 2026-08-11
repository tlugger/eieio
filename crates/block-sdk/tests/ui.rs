//! The rejections, as compile errors (SDK §1.1).
//!
//! A macro's error messages are its user interface — a block author meets them far more
//! often than the specification — and they are the only part of it that cannot be tested
//! any other way: the artefact under test is a compile that *fails*, with the right
//! message, pointing at the right token.
//!
//! Every case here is something a host would refuse at load (ABI §11.1) or something the
//! ABI makes meaningless (emitting to a port the block never declared). Catching them at
//! `cargo build` is the difference between a typo found by the compiler and a typo found
//! by a deploy.
//!
//! The expected output lives beside each case in `tests/ui/*.stderr`. Regenerate after an
//! intentional message change with `TRYBUILD=overwrite cargo test -p eio-sdk --test ui`,
//! and *read the diff* — a changed message is a changed user interface.

#[test]
fn rejections_are_compile_errors_with_the_right_message() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}
