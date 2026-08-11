;; `eio:state`, read with a real buffer and written back (ABI §7.2).
;;
;; The fixture the three state faults of ABI §13.1 are injected into:
;;
;; - **An undersized buffer.** The first `state_get` offers four bytes. A scenario that
;;   scripts a longer answer drives ABI §8's grow-and-retry, and the second call appears in
;;   the report; a scenario that seeds a short value does not, and it does not. Which of the
;;   two happened is therefore visible rather than inferred.
;; - **`ERR_THROTTLED`.** A leaf host refuses `state_put` on a flash wear budget (ABI §7.2),
;;   which is a property of the hardware — so a block's back-off branch is unreachable without
;;   a scripted refusal. This one reports the code as its status.
;; - **Denial.** With the capability denied, the first `state_get` answers `ERR_CAPABILITY`
;;   and the block never reaches its own logic, which is the whole of what a denied block can
;;   do.
;;
;; Every refusal comes back as the callback's status, so the scenario reads one number and
;; knows which branch ran.
;;
;;   0    "count"                 the key
;;   256  81 a1 61 6e 07          `[{"n": 7}]`, emitted on success
;;   300  ..                      where a read lands
;;   320  18 2a                   `42`, what is written back
(module
  (import "eio:state" "state_get" (func $state_get (param i32 i32 i32 i32) (result i32)))
  (import "eio:state" "state_put" (func $state_put (param i32 i32 i32 i32) (result i32)))
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))

  (memory (export "memory") 1)
  (data (i32.const 0) "count")
  (data (i32.const 256) "\81\a1\61\6e\07")
  (data (i32.const 320) "\18\2a")

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
    (local $size i32)
    (local $status i32)

    (local.set $status (call $count))
    (call $free (local.get $ptr) (local.get $len))
    (local.get $status))

  (func $count (result i32)
    (local $size i32)
    (local $put i32)

    ;; Four bytes, which is enough for a small integer and not for much else. Whether the
    ;; retry below runs is a property of the answer, not of this block.
    (local.set $size
      (call $state_get (i32.const 0) (i32.const 5) (i32.const 300) (i32.const 4)))
    (if (i32.lt_s (local.get $size) (i32.const 0))
      ;; `ERR_NOT_FOUND` on the first run, `ERR_CAPABILITY` when denied. Both are the host
      ;; telling this block something it can only report.
      (then (return (local.get $size))))
    (if (i32.gt_s (local.get $size) (i32.const 4))
      (then (drop (call $state_get (i32.const 0) (i32.const 5) (i32.const 300)
                        (local.get $size)))))

    (local.set $put
      (call $state_put (i32.const 0) (i32.const 5) (i32.const 320) (i32.const 2)))
    (if (i32.ne (local.get $put) (i32.const 0))
      ;; ABI §7.2: persistence is best-effort, so a refusal is reported and the instance
      ;; lives. A block treating this as fatal would be the defect §7.2 warns about.
      (then (return (local.get $put))))

    (drop (call $emit (i32.const 0) (i32.const 256) (i32.const 5)))
    (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"counter\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Reads and writes durable state, and reports every refusal as its status\",\"capabilities\":[\"state\"],\"inputs\":[{\"name\":\"in\"}],\"outputs\":[{\"name\":\"out\"}]}")
)
