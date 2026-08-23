;; Configures successfully on every odd life and refuses every even one, using `eio:state`
;; (ABI §7.2) as the continuity a restart's fresh linear memory cannot supply.
;;
;; eieio-35h.13's fixture. `Service::restart` (DAEMON §8) instantiates a fresh store on every
;; life, so nothing a block wrote to its own globals on one life survives to the next — only
;; `eio:state` crosses that gap (ABI §5.1 step 6). That used to make "a block that starts once
;; and then refuses" unbuildable as a `.wat` fixture; it is not, now that `Capability::State`
;; is implemented (`crates/daemon/src/instance.rs`'s `IMPLEMENTED_CAPABILITIES`).
;;
;; The counter, not a one-shot flag, is what lets one fixture cover both halves of DAEMON
;; §8's promise: a failed restart empties the slot (an even life), and indices staying intact
;; is what lets a *later* restart try again and succeed (the next life is odd). A flag that
;; only ever refused after the first success could show the first half but never the second.
(module
  (import "eio:state" "state_get" (func $state_get (param i32 i32 i32 i32) (result i32)))
  (import "eio:state" "state_put" (func $state_put (param i32 i32 i32 i32) (result i32)))

  (memory (export "memory") 1)

  ;; The counter's key, "count", at offset 0 (5 bytes). Offset 16 is `state_get`'s out-buffer
  ;; (never more than one byte long); offset 32 is where the incremented count is staged for
  ;; `state_put` to read it from.
  (data (i32.const 0) "count")

  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))
  (func (export "eio_alloc") (param i32) (result i32) (i32.const 1024))
  (func $free (export "eio_free") (param i32 i32))

  (func (export "eio_configure") (param $ptr i32) (param $len i32) (result i32)
    (local $found i32)
    (local $count i32)

    (local.set $found
      (call $state_get (i32.const 0) (i32.const 5) (i32.const 16) (i32.const 4)))
    ;; ABI §7.2: an absent key (ERR_NOT_FOUND, negative) is this instance's first life ever;
    ;; anything else found is the count from the life before this one.
    (local.set $count
      (if (result i32) (i32.lt_s (local.get $found) (i32.const 0))
        (then (i32.const 0))
        (else (i32.load8_u (i32.const 16)))))
    (local.set $count (i32.add (local.get $count) (i32.const 1)))
    (i32.store8 (i32.const 32) (local.get $count))
    (drop (call $state_put (i32.const 0) (i32.const 5) (i32.const 32) (i32.const 1)))

    ;; The configure payload is a host→guest buffer like any other (ABI §6.1).
    (call $free (local.get $ptr) (local.get $len))

    ;; Odd lives (1st, 3rd, ...) succeed; even lives (2nd, 4th, ...) refuse. -1 is
    ;; ERR_INVALID_ARG, the closest code to "this configuration is refused on purpose".
    (if (result i32) (i32.and (local.get $count) (i32.const 1))
      (then (i32.const 0))
      (else (i32.const -1))))

  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"restart_toggle\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Configures on odd lives and refuses on even ones, via eio:state\",\"capabilities\":[\"state\"]}")
)
