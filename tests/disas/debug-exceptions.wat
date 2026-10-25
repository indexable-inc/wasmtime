;;! target = "aarch64"
;;! test = "compile"
;;! flags = ["-Wexceptions=yes", "-Wgc=yes", "-Dguest-debug=yes"]

(module
  (tag $t (param i32))
  (import "" "host" (func))
  (func (export "main")
    (block $b (result i32)
      (try_table (catch $t $b)
        (drop (i32.const 42))
        (throw $t (i32.const 42)))
      i32.const 0)
    (call 0)
    (drop)))
;; wasm[0]::function[1]:
;;       stp     x29, x30, [sp, #-0x10]!
;;       mov     x29, sp
;;       stp     x27, x28, [sp, #-0x10]!
;;       stp     x25, x26, [sp, #-0x10]!
;;       stp     x23, x24, [sp, #-0x10]!
;;       stp     x21, x22, [sp, #-0x10]!
;;       stp     x19, x20, [sp, #-0x10]!
;;       stp     d14, d15, [sp, #-0x10]!
;;       stp     d12, d13, [sp, #-0x10]!
;;       stp     d10, d11, [sp, #-0x10]!
;;       stp     d8, d9, [sp, #-0x10]!
;;       sub     sp, sp, #0x20
;;       ldr     x0, [x2, #8]
;;       ldr     x1, [x0, #0x18]
;;       stur    x0, [sp, #0x18]
;;       mov     x0, sp
;;       cmp     x0, x1
;;       b.lo    #0x208
;;   48: stur    x2, [sp]
;;       mov     x0, x2
;;       stur    x2, [sp, #0x10]
;;       nop
;;       ├─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x35, slot at FP-0xb0, locals , stack 
;;       ╰─╼ breakpoint patch: wasm PC 0x35, patch bytes [63, 1, 0, 148]
;;       ldur    x0, [sp, #0x10]
;;       nop
;;       ├─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x37, slot at FP-0xb0, locals , stack 
;;       ╰─╼ breakpoint patch: wasm PC 0x37, patch bytes [61, 1, 0, 148]
;;       nop
;;       ├─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x3d, slot at FP-0xb0, locals , stack 
;;       ╰─╼ breakpoint patch: wasm PC 0x3d, patch bytes [60, 1, 0, 148]
;;       mov     w1, #0x2a
;;       stur    w1, [sp, #8]
;;       nop
;;       ├─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x3f, slot at FP-0xb0, locals , stack I32 @ slot+0x8
;;       ╰─╼ breakpoint patch: wasm PC 0x3f, patch bytes [57, 1, 0, 148]
;;       nop
;;       ├─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x40, slot at FP-0xb0, locals , stack 
;;       ╰─╼ breakpoint patch: wasm PC 0x40, patch bytes [56, 1, 0, 148]
;;       stur    w1, [sp, #8]
;;       nop
;;       ├─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x42, slot at FP-0xb0, locals , stack I32 @ slot+0x8
;;       ╰─╼ breakpoint patch: wasm PC 0x42, patch bytes [54, 1, 0, 148]
;;       bl      #0x4b8
;;   80: ldur    x0, [sp, #0x10]
;;       mov     x19, x2
;;       ldr     x0, [x0, #0x28]
;;       ldr     w3, [x0]
;;       mov     w1, w3
;;       add     x1, x1, #0x20
;;       ldr     w2, [x0, #4]
;;       cmp     x1, x2
;;       b.hi    #0x188
;;   a4: ldur    x4, [sp, #0x18]
;;       add     w1, w3, #0x20
;;       str     w1, [x0]
;;       mov     w2, #2
;;       movk    w2, #0x400, lsl #16
;;       ldr     x5, [x4, #0x20]
;;       add     x1, x5, w3, uxtw
;;       str     w2, [x5, w3, uxtw]
;;       ldur    x0, [sp, #0x10]
;;       ldr     x4, [x0, #0x30]
;;       ldr     w4, [x4, #8]
;;       str     w4, [x1, #4]
;;       mov     x4, #0x20
;;       str     w4, [x1, #8]
;;       mov     w6, #0x2a
;;       str     w6, [x1, #0x18]
;;       mov     x2, x19
;;       str     w2, [x1, #0x10]
;;       mov     w8, #0
;;       str     w8, [x1, #0x14]
;;       ldur    x2, [sp, #0x10]
;;       bl      #0x4f0
;;       ├─╼ exception frame offset: SP = FP - 0xb0
;;       ╰─╼ exception handler: tag=0, context at [SP+0x10], handler=0x100
;;       b       #0x1c0
;;  100: mov     w13, w0
;;       mov     x14, #0x20
;;       adds    x13, x13, x14
;;       b.hs    #0x1f0
;;  110: ldur    x2, [sp, #0x18]
;;       ldr     x14, [x2, #0x28]
;;       cmp     x13, x14
;;       b.hi    #0x1d8
;;  120: ldr     x1, [x2, #0x20]
;;       add     x0, x1, w0, uxtw
;;       ldr     w0, [x0, #0x18]
;;       stur    w0, [sp, #8]
;;       ldur    x0, [sp, #0x10]
;;       nop
;;       ├─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x48, slot at FP-0xb0, locals , stack I32 @ slot+0x8
;;       ╰─╼ breakpoint patch: wasm PC 0x48, patch bytes [7, 1, 0, 148]
;;       ldur    x14, [sp, #0x10]
;;       ldr     x0, [x14, #0x48]
;;       ldr     x2, [x14, #0x58]
;;       ldur    x3, [sp, #0x10]
;;       blr     x0
;;       ╰─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x4a, slot at FP-0xb0, locals , stack I32 @ slot+0x8
;;  14c: ldur    x0, [sp, #0x10]
;;       nop
;;       ├─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x4a, slot at FP-0xb0, locals , stack I32 @ slot+0x8
;;       ╰─╼ breakpoint patch: wasm PC 0x4a, patch bytes [0, 1, 0, 148]
;;       nop
;;       ├─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x4b, slot at FP-0xb0, locals , stack 
;;       ╰─╼ breakpoint patch: wasm PC 0x4b, patch bytes [255, 0, 0, 148]
;;       add     sp, sp, #0x20
;;       ldp     d8, d9, [sp], #0x10
;;       ldp     d10, d11, [sp], #0x10
;;       ldp     d12, d13, [sp], #0x10
;;       ldp     d14, d15, [sp], #0x10
;;       ldp     x19, x20, [sp], #0x10
;;       ldp     x21, x22, [sp], #0x10
;;       ldp     x23, x24, [sp], #0x10
;;       ldp     x25, x26, [sp], #0x10
;;       ldp     x27, x28, [sp], #0x10
;;       ldp     x29, x30, [sp], #0x10
;;       ret
;;  188: mov     w3, #2
;;  18c: movk    w3, #0x400, lsl #16
;;  190: ldur    x0, [sp, #0x10]
;;  194: ldr     x0, [x0, #0x30]
;;  198: ldr     w4, [x0, #8]
;;  19c: mov     w5, #0x20
;;  1a0: mov     w6, #0x10
;;  1a4: ldur    x2, [sp, #0x10]
;;  1a8: bl      #0x3e4
;;  1ac: ldur    x4, [sp, #0x18]
;;  1b0: ldr     x0, [x4, #0x20]
;;  1b4: add     x1, x0, w2, uxtw
;;  1b8: mov     x3, x2
;;  1bc: b       #0xdc
;;  1c0: mov     w3, #9
;;  1c4: ldur    x2, [sp, #0x10]
;;  1c8: bl      #0x44c
;;  1cc: ldur    x2, [sp, #0x10]
;;  1d0: bl      #0x484
;;       ╰─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x42, slot at FP-0xb0, locals , stack I32 @ slot+0x8
;;  1d4: udf     #0xc11f
;;  1d8: mov     w3, #0xfe
;;  1dc: ldur    x2, [sp, #0x10]
;;  1e0: bl      #0x44c
;;  1e4: ldur    x2, [sp, #0x10]
;;  1e8: bl      #0x484
;;       ╰─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x37, slot at FP-0xb0, locals , stack 
;;  1ec: udf     #0xc11f
;;  1f0: mov     w3, #0xfe
;;  1f4: ldur    x2, [sp, #0x10]
;;  1f8: bl      #0x44c
;;  1fc: ldur    x2, [sp, #0x10]
;;  200: bl      #0x484
;;       ╰─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x37, slot at FP-0xb0, locals , stack 
;;  204: udf     #0xc11f
;;  208: stur    x2, [sp, #0x10]
;;  20c: mov     w3, #0
;;  210: bl      #0x44c
;;  214: ldur    x2, [sp, #0x10]
;;  218: bl      #0x484
;;       ╰─╼ debug frame state (after previous inst): func key DefinedWasmFunction(StaticModuleIndex(0), DefinedFuncIndex(0)), wasm PC 0x34, slot at FP-0xb0, locals , stack 
;;  21c: udf     #0xc11f
