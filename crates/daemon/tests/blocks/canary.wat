;; A block that notices if the host ever calls into it while it is already inside a call.
;;
;; ABI §1.2: "the host MUST NOT call into a guest that is mid-call. Guest→host calls MUST NOT
;; re-enter the guest." A host cannot prove that about itself by inspection, so this block
;; watches from the inside: a depth counter raised on entry and lowered on exit, and a sticky
;; `$violated` flag set if the counter was ever already non-zero. Every callback returns
;; `$violated` as its status, so an overlap surfaces as a non-zero return (ABI §8) on this
;; and every later callback rather than as a message that could be missed.
;;
;; The `emit` call in the middle is the point rather than decoration: it is the moment the
;; guest is on the host's stack, and it is the only opening a host has to re-enter. ABI §6.2
;; closes it by making `emit` enqueue rather than deliver — this block is what fails if that
;; ever stops being true.
;;
;; `--input-port 1` is the same thing without the host call, so a test can tell "the executor
;; overlapped two work items" apart from "emit re-entered".
(module
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))

  (memory (export "memory") 1)

  (global $next (mut i32) (i32.const 1024))
  ;; How many callbacks are on the stack right now. Never above 1 on a correct host.
  (global $depth (mut i32) (i32.const 0))
  ;; Sticky: once an overlap has been seen, every later callback reports it too.
  (global $violated (mut i32) (i32.const 0))

  (func (export "eio_abi_version") (result i32)
    (i32.const 0x00010000))

  (func (export "eio_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $next))
    (global.set $next
      (i32.add
        (local.get $ptr)
        (i32.and (i32.add (local.get $size) (i32.const 8)) (i32.const -8))))
    (local.get $ptr))

  (func $free (export "eio_free") (param i32 i32))

  ;; Raises the depth counter, recording an overlap if there already was one.
  (func $enter
    (if (global.get $depth)
      (then (global.set $violated (i32.const 1))))
    (global.set $depth (i32.add (global.get $depth) (i32.const 1))))

  (func $leave
    (global.set $depth (i32.sub (global.get $depth) (i32.const 1))))

  ;; The status every callback returns: 0, or -1 once an overlap has been seen. Negated
  ;; because ABI §8's error codes are negative and -1 is ERR_INVALID_ARG; a bare `1` is not
  ;; an error code any host knows.
  (func $status (result i32)
    (i32.sub (i32.const 0) (global.get $violated)))

  (func (export "eio_configure") (param $ptr i32) (param $len i32) (result i32)
    (call $enter)
    (call $leave)
    ;; The configure payload is a host→guest buffer like any other (ABI §6.1).
    (call $free (local.get $ptr) (local.get $len))
    (call $status))

  (func (export "eio_start") (result i32)
    (call $enter)
    (call $leave)
    (call $status))

  (func (export "eio_stop") (result i32)
    (call $enter)
    (call $leave)
    (call $status))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (call $enter)
    ;; Port 0 emits — the guest is on the host's stack here — and port 1 does not, so a test
    ;; can tell which of the two ABI §1.2 rules a violation broke.
    (if (i32.eqz (local.get $port))
      (then (drop (call $emit (i32.const 0) (local.get $ptr) (local.get $len)))))
    ;; Port 2 deliberately does not leave, so the *next* callback sees a depth a correct host
    ;; can never produce. It is how a test proves this canary is able to fail at all — a
    ;; detector that has never fired is indistinguishable from one that cannot.
    (if (i32.ne (local.get $port) (i32.const 2))
      (then (call $leave)))
    ;; After the emit above, which reads out of this buffer while it is still the guest's
    ;; (ABI §9.3). Freed on every port, port 2 included: the reentrancy fault this fixture
    ;; stages is about depth, and leaking on top of it would stage two at once.
    (call $free (local.get $ptr) (local.get $len))
    (call $status))

  (@custom "eio:manifest" "{\"name\":\"canary\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Reports any overlapping callback entry\",\"inputs\":[{\"name\":\"emitting\"},{\"name\":\"quiet\"},{\"name\":\"wedge\"}],\"outputs\":[{\"name\":\"out\"}]}")
)
