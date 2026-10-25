;;! target = "x86_64"
;;! test = "optimize"
;;! flags = ["-Oix-static-call-indirect=y"]

;; Static-table `call_indirect` dispatch (ix fork): the row entry load is
;; `notrap aligned readonly` and carries the dispatch array's own alias
;; region; a zero entry falls back to the ordinary checked lowering.
(module
  (type $ii (func (param i32) (result i32)))
  (table 2 funcref)
  (elem (i32.const 0) $inc $add10)
  (func $inc (type $ii) (i32.add (local.get 0) (i32.const 1)))
  (func $add10 (type $ii) (i32.add (local.get 0) (i32.const 10)))
  (func (export "call") (param $i i32) (param $x i32) (result i32)
    (call_indirect (type $ii) (local.get $x) (local.get $i))))
;; function u0:0(i64 vmctx, i64, i32) -> i32 tail {
;;     region0 = 123 ""
;;     region1 = 160 ""
;;     gv0 = vmctx
;;     gv1 = load.i64 notrap aligned readonly can_move region0 gv0+8
;;     gv2 = load.i64 notrap aligned region1 gv1+24
;;     stack_limit = gv2
;;
;;                                 block0(v0: i64, v1: i64, v2: i32):
;; @0040                               jump block1
;;
;;                                 block1:
;; @003d                               v3 = iconst.i32 1
;;                                     v4 = iadd.i32 v2, v3  ; v3 = 1
;; @0040                               return v4
;; }
;;
;; function u0:1(i64 vmctx, i64, i32) -> i32 tail {
;;     region0 = 123 ""
;;     region1 = 160 ""
;;     gv0 = vmctx
;;     gv1 = load.i64 notrap aligned readonly can_move region0 gv0+8
;;     gv2 = load.i64 notrap aligned region1 gv1+24
;;     stack_limit = gv2
;;
;;                                 block0(v0: i64, v1: i64, v2: i32):
;; @0048                               jump block1
;;
;;                                 block1:
;; @0045                               v3 = iconst.i32 10
;;                                     v4 = iadd.i32 v2, v3  ; v3 = 10
;; @0048                               return v4
;; }
;;
;; function u0:2(i64 vmctx, i64, i32, i32) -> i32 tail {
;;     region0 = 123 ""
;;     region1 = 160 ""
;;     region2 = 90 ""
;;     region3 = 8 ""
;;     region4 = 93 ""
;;     region5 = 211 ""
;;     region6 = 117 ""
;;     region7 = 136 ""
;;     region8 = 6 ""
;;     region9 = 106 ""
;;     region10 = 116 ""
;;     region11 = 60 ""
;;     gv0 = vmctx
;;     gv1 = load.i64 notrap aligned readonly can_move region0 gv0+8
;;     gv2 = load.i64 notrap aligned region1 gv1+24
;;     sig0 = (i64 vmctx, i64, i32) -> i32 tail
;;     sig1 = (i64 vmctx, i32, i64) -> i64 tail
;;     fn0 = colocated u805306368:7 sig1
;;     stack_limit = gv2
;;
;;                                 block0(v0: i64, v1: i64, v2: i32, v3: i32):
;; @004f                               v4 = load.i64 notrap aligned readonly can_move region2 v0+56
;; @004f                               v5 = iconst.i32 2
;; @004f                               v6 = icmp uge v2, v5  ; v5 = 2
;; @004f                               v7 = select_spectre_guard v6, v5, v2  ; v5 = 2
;; @004f                               v8 = uextend.i64 v7
;; @004f                               v9 = iconst.i64 3
;; @004f                               v10 = ishl v8, v9  ; v9 = 3
;; @004f                               v11 = iadd v4, v10
;; @004f                               v12 = load.i64 notrap aligned readonly region3 v11
;; @004f                               brif v12, block2(v12, v0), block3
;;
;;                                 block3 cold:
;; @004f                               v15 = load.i64 notrap aligned region5 v0+72
;; @004f                               v19 = load.i64 notrap aligned region4 v0+64
;; @004f                               v16 = ireduce.i32 v15
;; @004f                               v17 = icmp.i32 uge v2, v16
;; @004f                               v23 = iconst.i64 0
;; @004f                               v18 = uextend.i64 v2
;;                                     v40 = iconst.i64 3
;;                                     v41 = ishl v18, v40  ; v40 = 3
;; @004f                               v22 = iadd v19, v41
;; @004f                               v24 = select_spectre_guard v17, v23, v22  ; v23 = 0
;; @004f                               v25 = load.i64 user6 aligned region6 v24
;; @004f                               v26 = iconst.i64 -2
;; @004f                               v27 = band v25, v26  ; v26 = -2
;; @004f                               brif v25, block5(v27), block4
;;
;;                                 block4 cold:
;; @004f                               v29 = iconst.i32 0
;; @004f                               v31 = call fn0(v0, v29, v18)  ; v29 = 0
;; @004f                               jump block5(v31)
;;
;;                                 block5(v28: i64) cold:
;; @004f                               v34 = load.i32 user7 aligned readonly region9 v28+16
;; @004f                               v32 = load.i64 notrap aligned readonly can_move region7 v0+48
;; @004f                               v33 = load.i32 notrap aligned readonly can_move region8 v32
;; @004f                               v35 = icmp eq v34, v33
;; @004f                               trapz v35, user8
;; @004f                               v37 = load.i64 notrap aligned readonly region10 v28+8
;; @004f                               v38 = load.i64 notrap aligned readonly region11 v28+24
;; @004f                               jump block2(v37, v38)
;;
;;                                 block2(v13: i64, v14: i64):
;; @004f                               v39 = call_indirect sig0, v13(v14, v0, v3)
;; @0052                               jump block1
;;
;;                                 block1:
;; @0052                               return v39
;; }
