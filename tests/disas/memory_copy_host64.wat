;;! target = 'x86_64'
;;! test = 'optimize'

(module
  (memory $m32_a 1)
  (memory $m32_b 1)
  (memory $m64_a i64 1)
  (memory $m64_b i64 1)

  (func $m32_to_same (param i32 i32 i32)
    local.get 0
    local.get 1
    local.get 2
    memory.copy $m32_a $m32_a
  )

  (func $m32_to_m32 (param i32 i32 i32)
    local.get 0
    local.get 1
    local.get 2
    memory.copy $m32_b $m32_a
  )

  (func $m32_to_m64 (param i64 i32 i32)
    local.get 0
    local.get 1
    local.get 2
    memory.copy $m64_a $m32_a
  )

  (func $m64_to_same (param i64 i64 i64)
    local.get 0
    local.get 1
    local.get 2
    memory.copy $m64_a $m64_a
  )

  (func $m64_to_m64 (param i64 i64 i64)
    local.get 0
    local.get 1
    local.get 2
    memory.copy $m64_b $m64_a
  )

  (func $m64_to_m32 (param i32 i64 i32)
    local.get 0
    local.get 1
    local.get 2
    memory.copy $m32_a $m64_a
  )
)
;; function u0:0(i64 vmctx, i64, i32, i32, i32) tail {
;;     region0 = 123 ""
;;     region1 = 160 ""
;;     region2 = 215 ""
;;     region3 = 105 ""
;;     region4 = 171 ""
;;     gv0 = vmctx
;;     gv1 = load.i64 notrap aligned readonly can_move region0 gv0+8
;;     gv2 = load.i64 notrap aligned region1 gv1+24
;;     sig0 = (i64 vmctx, i64, i64, i64) tail
;;     sig1 = (i64 vmctx, i64, i64, i64, i64, i64) -> i64 tail
;;     fn0 = colocated u805306368:1 sig0
;;     fn1 = colocated u805306368:51 sig1
;;     stack_limit = gv2
;;
;;                                 block0(v0: i64, v1: i64, v2: i32, v3: i32, v4: i32):
;; @0042                               v5 = load.i64 notrap aligned region3 v0+96
;; @0042                               v6 = uextend.i64 v2
;; @0042                               v7 = uextend.i64 v4
;; @0042                               v10 = iadd v6, v7
;; @0042                               v11 = icmp ugt v10, v5
;; @0042                               v18 = uextend.i64 v3
;; @0042                               v22 = iadd v18, v7
;; @0042                               v23 = icmp ugt v22, v5
;; @0042                               v29 = bor v11, v23
;; @0042                               trapnz v29, heap_oob
;;                                     v44 = iconst.i64 0
;; @0042                               v36 = select_spectre_guard v29, v44, v7  ; v44 = 0
;; @0042                               v37 = iconst.i64 0x8000
;; @0042                               v38 = icmp uge v36, v37  ; v37 = 0x8000
;; @0042                               brif v38, block4, block3
;;
;;                                 block3:
;; @0042                               v12 = load.i64 notrap aligned readonly can_move region2 v0+88
;; @0042                               v16 = iadd v12, v6
;;                                     v48 = iconst.i64 0
;;                                     v49 = select_spectre_guard v29, v48, v16  ; v48 = 0
;; @0042                               v28 = iadd v12, v18
;;                                     v50 = select_spectre_guard v29, v48, v28  ; v48 = 0
;; @0042                               call fn0(v0, v49, v50, v36)
;; @0042                               jump block2
;;
;;                                 block4 cold:
;;                                     v51 = iconst.i64 0
;;                                     v52 = load.i64 notrap aligned readonly can_move region2 v0+88
;;                                     v53 = iadd v52, v6
;;                                     v54 = select_spectre_guard v29, v51, v53  ; v51 = 0
;;                                     v55 = iadd v52, v18
;;                                     v56 = select_spectre_guard v29, v51, v55  ; v51 = 0
;; @0042                               jump block5(v51)  ; v51 = 0
;;
;;                                 block5(v39: i64) cold:
;;                                     v57 = iconst.i64 0x8000
;; @0042                               v42 = call fn1(v0, v54, v56, v36, v39, v57)  ; v57 = 0x8000
;; @0042                               v43 = icmp eq v42, v36
;; @0042                               brif v43, block2, block6
;;
;;                                 block6 cold:
;; @0042                               jump block5(v42)
;;
;;                                 block2:
;; @0046                               jump block1
;;
;;                                 block1:
;; @0046                               return
;; }
;;
;; function u0:1(i64 vmctx, i64, i32, i32, i32) tail {
;;     region0 = 123 ""
;;     region1 = 160 ""
;;     region2 = 215 ""
;;     region3 = 105 ""
;;     region4 = 171 ""
;;     region5 = 58 ""
;;     gv0 = vmctx
;;     gv1 = load.i64 notrap aligned readonly can_move region0 gv0+8
;;     gv2 = load.i64 notrap aligned region1 gv1+24
;;     sig0 = (i64 vmctx, i64, i64, i64) tail
;;     sig1 = (i64 vmctx, i64, i64, i64, i64, i64) -> i64 tail
;;     fn0 = colocated u805306368:1 sig0
;;     fn1 = colocated u805306368:51 sig1
;;     stack_limit = gv2
;;
;;                                 block0(v0: i64, v1: i64, v2: i32, v3: i32, v4: i32):
;; @004f                               v5 = load.i64 notrap aligned region3 v0+112
;; @004f                               v17 = load.i64 notrap aligned region3 v0+96
;; @004f                               v6 = uextend.i64 v2
;; @004f                               v7 = uextend.i64 v4
;; @004f                               v10 = iadd v6, v7
;; @004f                               v11 = icmp ugt v10, v5
;; @004f                               v18 = uextend.i64 v3
;; @004f                               v22 = iadd v18, v7
;; @004f                               v23 = icmp ugt v22, v17
;; @004f                               v29 = bor v11, v23
;; @004f                               trapnz v29, heap_oob
;;                                     v44 = iconst.i64 0
;; @004f                               v36 = select_spectre_guard v29, v44, v7  ; v44 = 0
;; @004f                               v37 = iconst.i64 0x8000
;; @004f                               v38 = icmp uge v36, v37  ; v37 = 0x8000
;; @004f                               brif v38, block4, block3
;;
;;                                 block3:
;; @004f                               v12 = load.i64 notrap aligned readonly can_move region2 v0+104
;; @004f                               v16 = iadd v12, v6
;;                                     v48 = iconst.i64 0
;;                                     v49 = select_spectre_guard v29, v48, v16  ; v48 = 0
;; @004f                               v24 = load.i64 notrap aligned readonly can_move region2 v0+88
;; @004f                               v28 = iadd v24, v18
;;                                     v50 = select_spectre_guard v29, v48, v28  ; v48 = 0
;; @004f                               call fn0(v0, v49, v50, v36)
;; @004f                               jump block2
;;
;;                                 block4 cold:
;;                                     v51 = iconst.i64 0
;;                                     v52 = load.i64 notrap aligned readonly can_move region2 v0+104
;;                                     v53 = iadd v52, v6
;;                                     v54 = select_spectre_guard v29, v51, v53  ; v51 = 0
;;                                     v55 = load.i64 notrap aligned readonly can_move region2 v0+88
;;                                     v56 = iadd v55, v18
;;                                     v57 = select_spectre_guard v29, v51, v56  ; v51 = 0
;; @004f                               jump block5(v51)  ; v51 = 0
;;
;;                                 block5(v39: i64) cold:
;;                                     v58 = iconst.i64 0x8000
;; @004f                               v42 = call fn1(v0, v54, v57, v36, v39, v58)  ; v58 = 0x8000
;; @004f                               v43 = icmp eq v42, v36
;; @004f                               brif v43, block2, block6
;;
;;                                 block6 cold:
;; @004f                               jump block5(v42)
;;
;;                                 block2:
;; @0053                               jump block1
;;
;;                                 block1:
;; @0053                               return
;; }
;;
;; function u0:2(i64 vmctx, i64, i64, i32, i32) tail {
;;     region0 = 123 ""
;;     region1 = 160 ""
;;     region2 = 215 ""
;;     region3 = 105 ""
;;     gv0 = vmctx
;;     gv1 = load.i64 notrap aligned readonly can_move region0 gv0+8
;;     gv2 = load.i64 notrap aligned region1 gv1+24
;;     sig0 = (i64 vmctx, i64, i64, i64) tail
;;     fn0 = colocated u805306368:1 sig0
;;     stack_limit = gv2
;;
;;                                 block0(v0: i64, v1: i64, v2: i64, v3: i32, v4: i32):
;; @005c                               v6 = load.i64 notrap aligned region3 v0+128
;; @005c                               v5 = uextend.i64 v4
;; @005c                               v7 = uadd_overflow_trap v2, v5, heap_oob
;; @005c                               v8 = icmp ugt v7, v6
;; @005c                               trapnz v8, heap_oob
;; @005c                               v9 = load.i64 notrap aligned can_move region2 v0+120
;; @005c                               v13 = load.i64 notrap aligned region3 v0+96
;; @005c                               v14 = uextend.i64 v3
;; @005c                               v18 = iadd v14, v5
;; @005c                               v19 = icmp ugt v18, v13
;; @005c                               trapnz v19, heap_oob
;; @005c                               v12 = iadd v9, v2
;; @005c                               v20 = load.i64 notrap aligned readonly can_move region2 v0+88
;; @005c                               v24 = iadd v20, v14
;; @005c                               call fn0(v0, v12, v24, v5)
;; @0060                               jump block1
;;
;;                                 block1:
;; @0060                               return
;; }
;;
;; function u0:3(i64 vmctx, i64, i64, i64, i64) tail {
;;     region0 = 123 ""
;;     region1 = 160 ""
;;     region2 = 215 ""
;;     region3 = 105 ""
;;     gv0 = vmctx
;;     gv1 = load.i64 notrap aligned readonly can_move region0 gv0+8
;;     gv2 = load.i64 notrap aligned region1 gv1+24
;;     sig0 = (i64 vmctx, i64, i64, i64) tail
;;     fn0 = colocated u805306368:1 sig0
;;     stack_limit = gv2
;;
;;                                 block0(v0: i64, v1: i64, v2: i64, v3: i64, v4: i64):
;; @0069                               v5 = load.i64 notrap aligned region3 v0+128
;; @0069                               v6 = uadd_overflow_trap v2, v4, heap_oob
;; @0069                               v7 = icmp ugt v6, v5
;; @0069                               trapnz v7, heap_oob
;; @0069                               v8 = load.i64 notrap aligned can_move region2 v0+120
;; @0069                               v13 = uadd_overflow_trap v3, v4, heap_oob
;; @0069                               v14 = icmp ugt v13, v5
;; @0069                               trapnz v14, heap_oob
;; @0069                               v11 = iadd v8, v2
;; @0069                               v18 = iadd v8, v3
;; @0069                               call fn0(v0, v11, v18, v4)
;; @006d                               jump block1
;;
;;                                 block1:
;; @006d                               return
;; }
;;
;; function u0:4(i64 vmctx, i64, i64, i64, i64) tail {
;;     region0 = 123 ""
;;     region1 = 160 ""
;;     region2 = 215 ""
;;     region3 = 105 ""
;;     gv0 = vmctx
;;     gv1 = load.i64 notrap aligned readonly can_move region0 gv0+8
;;     gv2 = load.i64 notrap aligned region1 gv1+24
;;     sig0 = (i64 vmctx, i64, i64, i64) tail
;;     fn0 = colocated u805306368:1 sig0
;;     stack_limit = gv2
;;
;;                                 block0(v0: i64, v1: i64, v2: i64, v3: i64, v4: i64):
;; @0076                               v5 = load.i64 notrap aligned region3 v0+144
;; @0076                               v6 = uadd_overflow_trap v2, v4, heap_oob
;; @0076                               v7 = icmp ugt v6, v5
;; @0076                               trapnz v7, heap_oob
;; @0076                               v8 = load.i64 notrap aligned can_move region2 v0+136
;; @0076                               v12 = load.i64 notrap aligned region3 v0+128
;; @0076                               v13 = uadd_overflow_trap v3, v4, heap_oob
;; @0076                               v14 = icmp ugt v13, v12
;; @0076                               trapnz v14, heap_oob
;; @0076                               v15 = load.i64 notrap aligned can_move region2 v0+120
;; @0076                               v11 = iadd v8, v2
;; @0076                               v18 = iadd v15, v3
;; @0076                               call fn0(v0, v11, v18, v4)
;; @007a                               jump block1
;;
;;                                 block1:
;; @007a                               return
;; }
;;
;; function u0:5(i64 vmctx, i64, i32, i64, i32) tail {
;;     region0 = 123 ""
;;     region1 = 160 ""
;;     region2 = 215 ""
;;     region3 = 105 ""
;;     gv0 = vmctx
;;     gv1 = load.i64 notrap aligned readonly can_move region0 gv0+8
;;     gv2 = load.i64 notrap aligned region1 gv1+24
;;     sig0 = (i64 vmctx, i64, i64, i64) tail
;;     fn0 = colocated u805306368:1 sig0
;;     stack_limit = gv2
;;
;;                                 block0(v0: i64, v1: i64, v2: i32, v3: i64, v4: i32):
;; @0083                               v6 = load.i64 notrap aligned region3 v0+96
;; @0083                               v7 = uextend.i64 v2
;; @0083                               v5 = uextend.i64 v4
;; @0083                               v11 = iadd v7, v5
;; @0083                               v12 = icmp ugt v11, v6
;; @0083                               trapnz v12, heap_oob
;; @0083                               v18 = load.i64 notrap aligned region3 v0+128
;; @0083                               v19 = uadd_overflow_trap v3, v5, heap_oob
;; @0083                               v20 = icmp ugt v19, v18
;; @0083                               trapnz v20, heap_oob
;; @0083                               v21 = load.i64 notrap aligned can_move region2 v0+120
;; @0083                               v13 = load.i64 notrap aligned readonly can_move region2 v0+88
;; @0083                               v17 = iadd v13, v7
;; @0083                               v24 = iadd v21, v3
;; @0083                               call fn0(v0, v17, v24, v5)
;; @0087                               jump block1
;;
;;                                 block1:
;; @0087                               return
;; }
