;;! target = "aarch64"
;;! test = "compile"
;;! flags = ["-Wpoll-interruption=y"]
;;! objdump = "--traps=true"

;; Poll interruption (ix fork): exactly one poll at function entry and
;; one per loop iteration. A poll is a plain load of the store's poll word
;; and a branch to a trap instruction carrying the `interrupt` trap code;
;; no fence, and no load carries that code. `count` has a loop; `leaf` has
;; none and polls only at entry.
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
;;       ldr     x8, [x2, #0x20]
;;       ldr     x9, [x8]
;;       cbnz    x9, #0x34
;;   14: mov     w2, #0
;;       ldr     x10, [x8]
;;       cbnz    x10, #0x38
;;   20: add     w2, w2, #1
;;       cmp     w2, w4
;;       b.lo    #0x18
;;   2c: ldp     x29, x30, [sp], #0x10
;;       ret
;;   34: udf     #0xc11f
;;       ╰─╼ trap: Normal(Interrupt)
;;   38: udf     #0xc11f
;;       ╰─╼ trap: Normal(Interrupt)
;;
;; wasm[0]::function[1]::leaf:
;;       stp     x29, x30, [sp, #-0x10]!
;;       mov     x29, sp
;;       ldr     x4, [x2, #0x20]
;;       ldr     x4, [x4]
;;       cbnz    x4, #0x60
;;   54: mov     w2, #7
;;       ldp     x29, x30, [sp], #0x10
;;       ret
;;   60: udf     #0xc11f
;;       ╰─╼ trap: Normal(Interrupt)
