;;! target = 'x86_64'
;;! test = 'optimize'
;;! flags = '-Wepoch-interruption'

(module
  (memory 1)
  (func $copy (param i32 i32 i32)
    (memory.copy (local.get 0) (local.get 1) (local.get 2))
  )
)
;; function u0:0(i64 vmctx, i64, i32, i32, i32) tail {
;;     region0 = 123 ""
;;     region1 = 160 ""
;;     region2 = 235 ""
;;     region3 = 18 ""
;;     region4 = 109 ""
;;     region5 = 215 ""
;;     region6 = 105 ""
;;     region7 = 171 ""
;;     gv0 = vmctx
;;     gv1 = load.i64 notrap aligned readonly can_move region0 gv0+8
;;     gv2 = load.i64 notrap aligned region1 gv1+24
;;     sig0 = (i64 vmctx) -> i64 tail
;;     sig1 = (i64 vmctx, i64, i64, i64) tail
;;     sig2 = (i64 vmctx, i64, i64, i64, i64, i64) -> i64 tail
;;     fn0 = colocated u805306368:13 sig0
;;     fn1 = colocated u805306368:1 sig1
;;     fn2 = colocated u805306368:51 sig2
;;     stack_limit = gv2
;;
;;                                 block0(v0: i64, v1: i64, v2: i32, v3: i32, v4: i32):
;; @001e                               v5 = load.i64 notrap aligned region2 v0+24
;; @001e                               v6 = load.i64 notrap aligned region3 v5
;; @001e                               v7 = load.i64 notrap aligned readonly can_move region0 v0+8
;; @001e                               v8 = load.i64 notrap aligned region4 v7+8
;; @001e                               v9 = icmp uge v6, v8
;; @001e                               brif v9, block3, block2(v8)
;;
;;                                 block3 cold:
;; @001e                               v10 = call fn0(v0)
;; @001e                               jump block2(v10)
;;
;;                                 block2(v63: i64):
;; @0025                               v14 = load.i64 notrap aligned region6 v0+72
;; @0025                               v15 = uextend.i64 v2
;; @0025                               v16 = uextend.i64 v4
;; @0025                               v19 = iadd v15, v16
;; @0025                               v20 = icmp ugt v19, v14
;; @0025                               v27 = uextend.i64 v3
;; @0025                               v31 = iadd v27, v16
;; @0025                               v32 = icmp ugt v31, v14
;; @0025                               v38 = bor v20, v32
;; @0025                               trapnz v38, heap_oob
;;                                     v73 = iconst.i64 0
;; @0025                               v45 = select_spectre_guard v38, v73, v16  ; v73 = 0
;; @0025                               v46 = iconst.i64 0x8000
;; @0025                               v47 = icmp uge v45, v46  ; v46 = 0x8000
;; @0025                               brif v47, block6, block5
;;
;;                                 block5:
;; @0025                               v21 = load.i64 notrap aligned readonly can_move region5 v0+64
;; @0025                               v25 = iadd v21, v15
;;                                     v77 = iconst.i64 0
;;                                     v78 = select_spectre_guard v38, v77, v25  ; v77 = 0
;; @0025                               v37 = iadd v21, v27
;;                                     v79 = select_spectre_guard v38, v77, v37  ; v77 = 0
;; @0025                               call fn1(v0, v78, v79, v45)
;; @0025                               jump block4(v63)
;;
;;                                 block6 cold:
;;                                     v80 = iconst.i64 0
;;                                     v81 = load.i64 notrap aligned readonly can_move region5 v0+64
;;                                     v82 = iadd v81, v15
;;                                     v83 = select_spectre_guard v38, v80, v82  ; v80 = 0
;;                                     v84 = iadd v81, v27
;;                                     v85 = select_spectre_guard v38, v80, v84  ; v80 = 0
;; @0025                               jump block7(v80, v63)  ; v80 = 0
;;
;;                                 block7(v48: i64, v55: i64) cold:
;;                                     v86 = iconst.i64 0x8000
;; @0025                               v51 = call fn2(v0, v83, v85, v45, v48, v86)  ; v86 = 0x8000
;; @0025                               v52 = icmp eq v51, v45
;; @0025                               brif v52, block4(v55), block8
;;
;;                                 block8 cold:
;; @0025                               v54 = load.i64 notrap aligned region3 v5
;; @0025                               v56 = icmp uge v54, v55
;; @0025                               brif v56, block10, block9(v55)
;;
;;                                 block10 cold:
;; @0025                               v58 = load.i64 notrap aligned region4 v7+8
;; @0025                               v59 = icmp.i64 uge v54, v58
;; @0025                               brif v59, block11, block9(v58)
;;
;;                                 block11 cold:
;; @0025                               v60 = call fn0(v0)
;; @0025                               jump block9(v60)
;;
;;                                 block9(v64: i64):
;; @0025                               jump block7(v51, v64)
;;
;;                                 block4(v67: i64):
;; @0025                               v66 = load.i64 notrap aligned region3 v5
;; @0025                               v68 = icmp uge v66, v67
;; @0025                               brif v68, block13, block12
;;
;;                                 block13 cold:
;; @0025                               v70 = load.i64 notrap aligned region4 v7+8
;; @0025                               v71 = icmp.i64 uge v66, v70
;; @0025                               brif v71, block14, block12
;;
;;                                 block14 cold:
;; @0025                               v72 = call fn0(v0)
;; @0025                               jump block12
;;
;;                                 block12:
;; @0029                               jump block1
;;
;;                                 block1:
;; @0029                               return
;; }
