;;! target = "aarch64"
;;! test = "compile"
;;! flags = ["-Wpoll-interruption=y"]
;;! objdump = "--traps=true"

;; Poll interruption (ix fork): one poll per loop iteration. A poll is a
;; plain load of the store's poll word and a branch to a trap instruction
;; carrying the `interrupt` trap code; no fence, and no load carries that
;; code. Both functions are bounded (`IxPollPlan`): `count` polls only at
;; its loop header and `leaf` (no loop, no call) does not poll at all; its
;; caller or the host-to-wasm entry polled before it.
(module
  (func $count (param i32) (result i32) (local i32)
    (loop
      (local.set 1 (i32.add (local.get 1) (i32.const 1)))
      (br_if 0 (i32.lt_u (local.get 1) (local.get 0))))
    (local.get 1))
  (func $leaf (result i32) (i32.const 7)))
;; wasm[0]::function[0]::count:
;;       stp     x29, x30, [sp, #-0x10]!
;;       mov     x29, sp
;;       mov     w3, #0
;;       ldr     x7, [x2, #0x20]
;;       ldr     x8, [x7]
;;       cbnz    x8, #0x38
;;   18: mov     x2, x3
;;       add     w2, w2, #1
;;       cmp     w2, w4
;;       b.hs    #0x30
;;   28: mov     x3, x2
;;       b       #0x10
;;   30: ldp     x29, x30, [sp], #0x10
;;       ret
;;   38: udf     #0xc11f
;;       ╰─╼ trap: Normal(Interrupt)
;;
;; wasm[0]::function[1]::leaf:
;;       stp     x29, x30, [sp, #-0x10]!
;;       mov     x29, sp
;;       mov     w2, #7
;;       ldp     x29, x30, [sp], #0x10
;;       ret
