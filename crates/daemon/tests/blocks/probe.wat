;; Calls all seven `eio:core` functions (ABI §7.0) and emits what it observed.
;;
;; The four checks are encoded in a CBOR batch built by hand as a data segment: one signal
;; with four boolean fields, each starting `false` (0xf4) and overwritten with `true` (0xf5)
;; when its check passes. That is the whole reason this block exists — a host that stubbed
;; `time_unix_ms` to zero, or answered `rand` with a size instead of a status, would pass a
;; test that only looked at return codes.
;;
;; The template, at address 128, is 26 bytes:
;;
;;   81            array(1)          the batch
;;   a4            map(4)            the signal
;;   64 "mono" V   "mono": bool      offset 135
;;   64 "prop" V   "prop": bool      offset 141
;;   64 "rand" V   "rand": bool      offset 147
;;   64 "unix" V   "unix": bool      offset 153
;;
;; Keys are in ascending bytewise order of their UTF-8 content, which ABI §6.3.1 rule 7
;; requires — written that way here so the host's decoder accepts it, which is itself part of
;; what this block tests. (0x64 is both the CBOR head for a 4-byte text string and the letter
;; `d`, which is why the segment below spells it as one.)
(module
  (import "eio:core" "log" (func $log (param i32 i32 i32)))
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))
  (import "eio:core" "prop" (func $prop (param i32 i32 i32 i32) (result i32)))
  (import "eio:core" "error" (func $error (param i32 i32 i32)))
  (import "eio:core" "time_unix_ms" (func $time_unix_ms (result i64)))
  (import "eio:core" "time_mono_ms" (func $time_mono_ms (result i64)))
  (import "eio:core" "rand" (func $rand (param i32 i32) (result i32)))

  (memory (export "memory") 1)
  (data (i32.const 0) "configured")
  (data (i32.const 16) "stop detail")
  (data (i32.const 128) "\81\a4dmono\f4dprop\f4drand\f4dunix\f4")

  (global $next (mut i32) (i32.const 1024))

  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))

  (func (export "eio_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $next))
    (global.set $next
      (i32.add
        (local.get $ptr)
        (i32.and (i32.add (local.get $size) (i32.const 8)) (i32.const -8))))
    (local.get $ptr))

  (func $free (export "eio_free") (param i32 i32))

  (func (export "eio_configure") (param $ptr i32) (param $len i32) (result i32)
    ;; One line at each of ABI §7.0's five levels.
    (call $log (i32.const 0) (i32.const 0) (i32.const 10))
    (call $log (i32.const 1) (i32.const 0) (i32.const 10))
    (call $log (i32.const 2) (i32.const 0) (i32.const 10))
    (call $log (i32.const 3) (i32.const 0) (i32.const 10))
    (call $log (i32.const 4) (i32.const 0) (i32.const 10))
    ;; The configure payload is a host→guest buffer like any other (ABI §6.1).
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  (func (export "eio_start") (result i32) (i32.const 0))

  (func (export "eio_stop") (result i32)
    ;; A non-zero callback return with detail attached (ABI §7.0, §8). Not a trap: the host
    ;; logs it, counts it, and the instance stops all the same.
    (call $error (i32.const -3) (i32.const 16) (i32.const 11))
    (i32.const -3))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (local $first i32)
    (local $second i32)

    ;; Milliseconds since some origin: never negative.
    (if (i64.ge_s (call $time_mono_ms) (i64.const 0))
      (then (i32.store8 (i32.const 135) (i32.const 0xf5))))

    ;; Wall clock, after 2020-09-13. A stub returning zero fails this.
    (if (i64.gt_s (call $time_unix_ms) (i64.const 1600000000000))
      (then (i32.store8 (i32.const 153) (i32.const 0xf5))))

    ;; `rand` follows the status convention, not the size one: exactly `len` bytes, and `0`
    ;; to say so.
    (if (i32.eqz (call $rand (i32.const 512) (i32.const 32)))
      (then (i32.store8 (i32.const 147) (i32.const 0xf5))))

    ;; `prop` under ABI §8's size convention: ask with no buffer, then read into one of the
    ;; size it named. The two answers must agree, and the first must be positive.
    (local.set $first
      (call $prop (i32.const 0) (i32.const 0) (i32.const 256) (i32.const 0)))
    (local.set $second
      (call $prop (i32.const 0) (i32.const 0) (i32.const 256) (local.get $first)))
    (if (i32.and
          (i32.gt_s (local.get $first) (i32.const 0))
          (i32.eq (local.get $first) (local.get $second)))
      (then (i32.store8 (i32.const 141) (i32.const 0xf5))))

    ;; Before the emit, and safely so: what is emitted lives at 128, not in the inbound
    ;; buffer, so nothing reads `$ptr` after this (ABI §6.1, §9.3).
    (call $free (local.get $ptr) (local.get $len))
    (call $emit (i32.const 0) (i32.const 128) (i32.const 26)))

  (@custom "eio:manifest" "{\"name\":\"probe\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Calls every eio:core function and reports\",\"inputs\":[{\"name\":\"in\"}],\"outputs\":[{\"name\":\"out\"}],\"properties\":[{\"name\":\"n\",\"type\":\"int\",\"default\":\"7\"}]}")
)
