;; `eio:i2c`'s three calls, including `i2c_write_read`'s out-buffer at argument position 4 —
;; unusual next to `state_get` and `i2c_read`, whose `(buf, cap)` sits at position 2
;; (ABI §7.5, `capability.rs`'s `out_buffer` table).
;;
;; No golden block exercises `eio:i2c`: ABI §13.2 does not list one, so this hand-written
;; fixture is `eio:i2c`'s only conformance coverage, the way `state_harness.wat` is the state
;; capability's undersized-buffer case. It writes, reads, and write-reads over the bus in one
;; delivery and emits both answers, so a scenario reads one emitted batch and knows whether
;; every call reached the byte range it was meant to.
;;
;; `i2c_write_read`'s answer is written directly into the template below rather than copied
;; there afterward: `buf` at argument index 4 points straight at the slot the emitted batch
;; reads from, so a host consulting the wrong argument position writes into `wptr`'s memory
;; instead and leaves this slot at its start value of four zero bytes — visibly wrong rather
;; than coincidentally right.
;;
;;   0    01 02        i2c_write's payload
;;   8    0a 0b        i2c_write_read's write-half payload
;;   200  81 a2 61 72 44 <4: i2c_read's answer> 62 77 72 44 <4: i2c_write_read's answer>
;;        `[{"r": h'....', "wr": h'....'}]` — "r" sorts before "wr" (ABI §6.3.1 rule 7)
(module
  (import "eio:i2c" "i2c_write" (func $i2c_write (param i32 i32 i32 i32) (result i32)))
  (import "eio:i2c" "i2c_read" (func $i2c_read (param i32 i32 i32 i32) (result i32)))
  (import "eio:i2c" "i2c_write_read"
    (func $i2c_write_read (param i32 i32 i32 i32 i32 i32) (result i32)))
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))

  (memory (export "memory") 1)
  (data (i32.const 0) "\01\02")
  (data (i32.const 8) "\0a\0b")
  (data (i32.const 200) "\81\a2\61\72\44\00\00\00\00\62\77\72\44\00\00\00\00")

  (global $next (mut i32) (i32.const 1024))
  (global $allocs (mut i32) (i32.const 0))
  (global $frees (mut i32) (i32.const 0))

  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))

  (func (export "eio_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (global.set $allocs (i32.add (global.get $allocs) (i32.const 1)))
    (local.set $ptr (global.get $next))
    (global.set $next
      (i32.add
        (local.get $ptr)
        (i32.and (i32.add (local.get $size) (i32.const 8)) (i32.const -8))))
    (local.get $ptr))

  (func $free (export "eio_free") (param i32 i32)
    (global.set $frees (i32.add (global.get $frees) (i32.const 1))))

  (func (export "eio_configure") (param $ptr i32) (param $len i32) (result i32)
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  (func (export "eio_start") (result i32) (i32.const 0))

  (func (export "eio_stop") (result i32)
    (if (result i32) (i32.eq (global.get $allocs) (global.get $frees))
      (then (i32.const 0))
      (else (i32.const -1))))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (local $status i32)
    (local.set $status (call $probe))
    (call $free (local.get $ptr) (local.get $len))
    (local.get $status))

  ;; The three `eio:i2c` calls (ABI §7.5): a plain write, `i2c_read`'s size convention at its
  ;; usual position 2, and `i2c_write_read`'s at the unusual position 4.
  (func $probe (result i32)
    (local $status i32)

    (local.set $status
      (call $i2c_write (i32.const 0) (i32.const 0x50) (i32.const 0) (i32.const 2)))
    (if (i32.lt_s (local.get $status) (i32.const 0)) (then (return (local.get $status))))

    (local.set $status
      (call $i2c_read (i32.const 0) (i32.const 0x50) (i32.const 205) (i32.const 4)))
    (if (i32.lt_s (local.get $status) (i32.const 0)) (then (return (local.get $status))))

    (local.set $status
      (call $i2c_write_read
        (i32.const 1) (i32.const 0x51) (i32.const 8) (i32.const 2)
        (i32.const 213) (i32.const 4)))
    (if (i32.lt_s (local.get $status) (i32.const 0)) (then (return (local.get $status))))

    (drop (call $emit (i32.const 0) (i32.const 200) (i32.const 17)))
    (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"i2c_probe\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Writes, reads, and write-reads over eio:i2c, and emits both answers\",\"capabilities\":[\"i2c\"],\"inputs\":[{\"name\":\"in\"}],\"outputs\":[{\"name\":\"out\"}]}")
)
