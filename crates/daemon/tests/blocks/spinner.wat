;; A block that never returns from `eio_process_signals`.
;;
;; ABI §10's hostile case: "callbacks MUST return promptly. Blocking is a defect." Nothing
;; here is malformed — the module validates, configures and starts like any other block — so
;; the only thing that can end this callback is the host's execution budget. What the tests
;; assert against it is that the budget is what ends it (fuel or deadline, DAEMON §5.1's trap
;; table), that the instance it ends is exactly this one, and that the rest of the daemon
;; carries on while it spins.
(module
  (memory (export "memory") 1)

  (global $next (mut i32) (i32.const 1024))

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

  (func (export "eio_free") (param i32 i32))

  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    ;; A bare `loop`/`br` and nothing else: no allocation, no host call, no memory traffic.
    ;; The only thing it consumes is fuel, which is what makes it a test of the budget rather
    ;; than of anything the guest touches.
    (loop $forever (br $forever))
    (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"spinner\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Never returns from process_signals\",\"inputs\":[{\"name\":\"in\"}],\"outputs\":[]}")
)
