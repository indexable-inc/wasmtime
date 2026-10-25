;;! target = 'x86_64'
;;! test = 'optimize'
;;! flags = '-Wfuel=100'

(module
  (memory 1)
  (func $copy (param i32 i32 i32)
    (memory.copy (local.get 0) (local.get 1) (local.get 2))
  )
)
;; function u0:0(i64 vmctx, i64, i32, i32, i32) tail {
;;     region0 = 123 ""
;;     region1 = 160 ""
;;     region2 = 7 ""
;;     region3 = 215 ""
;;     region4 = 105 ""
;;     region5 = 171 ""
;;     gv0 = vmctx
;;     gv1 = load.i64 notrap aligned readonly can_move region0 gv0+8
;;     gv2 = load.i64 notrap aligned region1 gv1+24
;;     sig0 = (i64 vmctx) -> i8 tail
;;     sig1 = (i64 vmctx, i64, i64, i64) tail
;;     sig2 = (i64 vmctx, i64, i64, i64, i64, i64) -> i64 tail
;;     fn0 = colocated u805306368:12 sig0
;;     fn1 = colocated u805306368:1 sig1
;;     fn2 = colocated u805306368:51 sig2
;;     stack_limit = gv2
;;
;;                                 block0(v0: i64, v1: i64, v2: i32, v3: i32, v4: i32):
;; @001e                               v5 = load.i64 notrap aligned readonly can_move region0 v0+8
;; @001e                               v6 = load.i64 notrap aligned region2 v5
;; @001e                               v7 = iconst.i64 1
;; @001e                               v8 = iadd v6, v7  ; v7 = 1
;; @001e                               v9 = iconst.i64 0
;; @001e                               v10 = icmp sge v8, v9  ; v9 = 0
;; @001e                               brif v10, block2, block3(v8)
;;
;;                                 block2:
;;                                     v77 = iadd.i64 v6, v7  ; v7 = 1
;; @001e                               store notrap aligned region2 v77, v5
;; @001e                               v12 = call fn0(v0)
;; @001e                               v14 = load.i64 notrap aligned region2 v5
;; @001e                               jump block3(v14)
;;
;;                                 block3(v58: i64):
;; @0025                               v18 = load.i64 notrap aligned region4 v0+72
;; @0025                               v19 = uextend.i64 v2
;; @0025                               v20 = uextend.i64 v4
;; @0025                               v23 = iadd v19, v20
;; @0025                               v24 = icmp ugt v23, v18
;; @0025                               v31 = uextend.i64 v3
;; @0025                               v35 = iadd v31, v20
;; @0025                               v36 = icmp ugt v35, v18
;; @0025                               v42 = bor v24, v36
;; @0025                               trapnz v42, heap_oob
;;                                     v78 = iconst.i64 0
;;                                     v79 = select_spectre_guard v42, v78, v20  ; v78 = 0
;; @0025                               v50 = iconst.i64 0x8000
;; @0025                               v51 = icmp uge v79, v50  ; v50 = 0x8000
;; @0025                               brif v51, block6, block5
;;
;;                                 block5:
;; @0025                               v25 = load.i64 notrap aligned readonly can_move region3 v0+64
;; @0025                               v29 = iadd v25, v19
;;                                     v80 = iconst.i64 0
;;                                     v81 = select_spectre_guard v42, v80, v29  ; v80 = 0
;; @0025                               v41 = iadd v25, v31
;;                                     v82 = select_spectre_guard v42, v80, v41  ; v80 = 0
;; @0025                               call fn1(v0, v81, v82, v79)
;; @0025                               jump block4
;;
;;                                 block6 cold:
;;                                     v83 = iconst.i64 0
;;                                     v84 = load.i64 notrap aligned readonly can_move region3 v0+64
;;                                     v85 = iadd v84, v19
;;                                     v86 = select_spectre_guard v42, v83, v85  ; v83 = 0
;;                                     v87 = iadd v84, v31
;;                                     v88 = select_spectre_guard v42, v83, v87  ; v83 = 0
;; @0025                               jump block7(v83)  ; v83 = 0
;;
;;                                 block7(v52: i64) cold:
;;                                     v89 = iconst.i64 0x8000
;; @0025                               v55 = call fn2(v0, v86, v88, v79, v52, v89)  ; v89 = 0x8000
;; @0025                               v56 = icmp eq v55, v79
;; @0025                               brif v56, block4, block8
;;
;;                                 block8 cold:
;; @0025                               jump block7(v55)
;;
;;                                 block4:
;; @0025                               v60 = iconst.i64 4
;; @0025                               v61 = iadd.i64 v58, v60  ; v60 = 4
;; @0025                               v65 = iadd v61, v20
;;                                     v90 = iconst.i64 0
;;                                     v91 = icmp sge v65, v90  ; v90 = 0
;; @0025                               brif v91, block9, block10(v65)
;;
;;                                 block9:
;; @0025                               store.i64 notrap aligned region2 v65, v5
;; @0025                               v69 = call fn0(v0)
;; @0025                               v71 = load.i64 notrap aligned region2 v5
;; @0025                               jump block10(v71)
;;
;;                                 block10(v73: i64):
;; @0029                               jump block1
;;
;;                                 block1:
;; @0029                               store.i64 notrap aligned region2 v73, v5
;; @0029                               return
;; }
