;; A pin, a watch, and an emission per edge (ABI §7.4, §4.2).
;;
;; The second capability shape after `counter.wat`, and a different one: `eio:state` answers
;; under the size convention, `eio:gpio` answers with ids and levels. Both callbacks-with-ids
;; (`gpio_watch` → `eio_on_gpio`) and answers-with-values (`gpio_read`) are here, which is the
;; pattern `eio:timer` and `eio:http` share.
;;
;; It refuses a level that is neither `0` nor `1`. ABI §7.4 defines `gpio_read` as answering
;; one of those or an error, so anything else is a non-conformant host — and a block that
;; believed it would emit a signal about a pin state that does not exist. A scenario can script
;; exactly that, which is the only way the branch is reachable.
;;
;;   256  81 a1 61 76    `[{"v": ` — the level lands at 260 as a one-byte CBOR integer
(module
  (import "eio:gpio" "gpio_mode" (func $gpio_mode (param i32 i32) (result i32)))
  (import "eio:gpio" "gpio_watch" (func $gpio_watch (param i32 i32) (result i32)))
  (import "eio:gpio" "gpio_read" (func $gpio_read (param i32) (result i32)))
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))

  (memory (export "memory") 1)
  (data (i32.const 256) "\81\a1\61\76")

  (global $next (mut i32) (i32.const 1024))
  (global $watch (mut i32) (i32.const -1))

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
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  ;; Pin 4 as an input, watched on both edges (ABI §7.4: 0 = input, 3 = both).
  (func (export "eio_start") (result i32)
    (local $status i32)
    (local.set $status (call $gpio_mode (i32.const 4) (i32.const 0)))
    (if (i32.ne (local.get $status) (i32.const 0))
      (then (return (local.get $status))))
    (global.set $watch (call $gpio_watch (i32.const 4) (i32.const 3)))
    (if (result i32) (i32.lt_s (global.get $watch) (i32.const 0))
      (then (global.get $watch))
      (else (i32.const 0))))

  (func (export "eio_stop") (result i32) (i32.const 0))

  (func (export "eio_on_gpio") (param $watch i32) (param $value i32) (result i32)
    (local $level i32)
    (if (i32.ne (local.get $watch) (global.get $watch))
      (then (return (i32.const -1))))

    ;; Read the pin rather than trusting the edge's `value`: the two can differ on a line that
    ;; moved again, and which one a block wants is the block's business. Reading is what makes
    ;; the host's answer observable here.
    (local.set $level (call $gpio_read (i32.const 4)))
    (if (i32.gt_u (local.get $level) (i32.const 1))
      ;; Not a level ABI §7.4 defines — including a negative one, which `gt_u` catches as a
      ;; very large unsigned. `ERR_INVALID_ARG` says the host answered with something this
      ;; block will not act on.
      (then (return (i32.const -1))))

    ;; A CBOR integer below 24 is its own byte, so the level is the encoding.
    (i32.store8 (i32.const 260) (local.get $level))
    (drop (call $emit (i32.const 0) (i32.const 256) (i32.const 5)))
    (i32.const 0))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"gpio-echo\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Watches a pin and emits the level it reads on every edge\",\"capabilities\":[\"gpio\"],\"inputs\":[{\"name\":\"in\"}],\"outputs\":[{\"name\":\"out\"}]}")
)
