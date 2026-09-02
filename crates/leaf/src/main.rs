//! `eio-leaf` — a host-built bring-up of the leaf-class node runtime (eieio-x7g.2's first
//! milestone).
//!
//! Running this binary builds the two ABI §13.2 golden blocks this milestone drives
//! (`counter`, `transform`), bakes a two-instance graph by hand, and drives it through ABI
//! §5.1's whole lifecycle with one signal routed between the two instances over `eio_leaf`'s
//! wasm3 binding. See `eio_leaf`'s own crate docs for what that proves and what it does not,
//! and `tests/end_to_end.rs` for the assertion this prints the same numbers against.

fn main() {
    let state_dir = std::env::temp_dir().join("eio-leaf-demo-state");
    println!("eio-leaf: a host-built bring-up of LEAF-SPEC's runtime (eieio-x7g.2, milestone 1)");
    println!("engine: wasm3 (interpreted; LEAF §3's bring-up engine)");
    println!("state store: {} (see eio_leaf::state)", state_dir.display());
    println!();

    match eio_leaf::run_demo(&state_dir) {
        Ok(outcome) => {
            println!("counter.process_signals -> {:?}", outcome.counter_status);
            println!(
                "router: counter.out -> instance {} port {}",
                outcome.routed_to.instance, outcome.routed_to.port
            );
            println!(
                "transform.process_signals -> {:?}",
                outcome.transform_status
            );
            println!("transform emitted val = {}", outcome.transform_val);
            println!(
                "callback errors after stop: counter={} transform={}",
                outcome.errors.0, outcome.errors.1
            );
            assert_eq!(
                outcome.transform_val, 44,
                "a first run's count is 3, so transform's (+ $n 41) should answer 44"
            );
            println!();
            println!("OK: the ★ crates drove a signal from one instance to another over wasm3.");
        }
        Err(error) => {
            eprintln!("eio-leaf: {error}");
            std::process::exit(1);
        }
    }
}
