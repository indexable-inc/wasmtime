;;! target = "aarch64"
;;! test = "compile"
;;! flags = ["-Wpoll-interruption=y"]
;;! objdump = "--traps=true"

;; Poll-page interruption (ix fork): exactly one poll (a load that carries
;; the `interrupt` trap code) at function entry and one per loop
;; iteration, with no compare and no branch. `count` has a loop; `leaf`
;; has none and polls only at entry.
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
;;       ldar    x9, [x8]
;;       ╰─╼ trap: Normal(Interrupt)
;;       mov     w2, #0
;;       ldar    x9, [x8]
;;       ╰─╼ trap: Normal(Interrupt)
;;       add     w2, w2, #1
;;       cmp     w2, w4
;;       b.lo    #0x14
;;   24: ldp     x29, x30, [sp], #0x10
;;       ret
;;
;; wasm[0]::function[1]::leaf:
;;       stp     x29, x30, [sp, #-0x10]!
;;       mov     x29, sp
;;       ldr     x4, [x2, #0x20]
;;       ldar    x4, [x4]
;;       ╰─╼ trap: Normal(Interrupt)
;;       mov     w2, #7
;;       ldp     x29, x30, [sp], #0x10
;;       ret
