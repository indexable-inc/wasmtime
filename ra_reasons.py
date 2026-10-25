#!/usr/bin/env python3
"""Join a regalloc move-reason trace with exact callgrind instruction counts.

Inputs:
  insns.json.gz  from classify.py `vgrecord`: {"all_instructions": N,
                 "funcs": [[jitdump_name, {off: objdump_text}, {off: count}], ...]}
                 (offsets relative to the function's jitdump code start).
  TRACE_DIR      written by a Wasmtime/Cranelift build run with
                 CRANELIFT_RA_TRACE_DIR=TRACE_DIR at compile time:
                   <ns>_<index>.tsv  start end reason kind from to  (one line per regalloc edit)
                   <ns>_<index>.sym  symbol  jitdump_name  module  defined_func_index
                   legend.tsv        reason bit fields

Prints the exact dynamic instruction count of regalloc-inserted moves per
reason, and per function for the top-N functions by dynamic move count.

Run: python3 -I ra_reasons.py insns.json.gz TRACE_DIR [--top 20] [--json out.json]
"""

from __future__ import annotations

import argparse
import bisect
import gzip
import json
import sys
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path

DIR = {0: "reg->reg", 1: "spill(reg->stack)", 2: "reload(stack->reg)", 3: "stack->stack"}
KIND = {0: "intra_block", 1: "block_edge", 2: "blockparam", 3: "reuse_fixup",
        4: "multi_fixed_fixup", 5: "scratch_save", 7: "unknown"}
CAUSE = {0: "original", 1: "conflict_bundle", 2: "conflict_fixed_bundle_call",
         3: "conflict_fixed_bundle_other", 4: "call_clobber", 5: "fixed_resv_at_call",
         6: "fixed_resv_other", 7: "requirement_conflict", 8: "minimal_max_splits",
         9: "minimal_one_inst"}
SRC = {0: "dst_lr_start", 1: "src_lr_end", 2: "dst_bundle", 3: "src_bundle"}


def decode(r: int) -> dict[str, object]:
    if r == 0xFFFF:
        return {"dir": "?", "kind": "no_reason", "cause": "?", "spill": False, "trigger": "?", "src": "?"}
    cause = (r >> 5) & 0xF
    trig = (r >> 10) & 0xF
    return {
        "dir": DIR[r & 3],
        "kind": KIND.get((r >> 2) & 7, f"kind{(r >> 2) & 7}"),
        "cause": CAUSE.get(cause, f"cause{cause}"),
        "spill": bool((r >> 9) & 1),
        "trigger": CAUSE.get(trig, f"cause{trig}") if cause in (8, 9) else "",
        "src": SRC[(r >> 14) & 3],
    }


def label(r: int) -> str:
    d = decode(r)
    cause = str(d["cause"])
    if d["trigger"]:
        cause += f"<-{d['trigger']}"
    if d["spill"]:
        cause += "+spillregion"
    return f"{d['dir']:<19} {d['kind']:<18} {cause}"


@dataclass
class Rec:
    start: int
    end: int
    reason: int
    kind: str
    src: str
    dst: str


def load_trace(tdir: Path) -> tuple[dict[str, list[str]], dict[str, list[Rec]]]:
    by_name: dict[str, list[str]] = defaultdict(list)
    for p in sorted(tdir.glob("*.sym")):
        fields = p.read_text().rstrip("\n").split("\t")
        if len(fields) < 2:
            raise SystemExit(f"{p}: malformed .sym (want symbol\\tjitdump_name...)")
        by_name[fields[1]].append(p.stem)
        if fields[0] != fields[1]:
            by_name[fields[0]].append(p.stem)
    recs: dict[str, list[Rec]] = {}
    for p in sorted(tdir.glob("*.tsv")):
        if p.name == "legend.tsv":
            continue
        out = []
        for ln, line in enumerate(p.read_text().splitlines(), 1):
            f = line.split("\t")
            if len(f) != 6:
                raise SystemExit(f"{p}:{ln}: want 6 tab-separated fields, got {len(f)}")
            out.append(Rec(int(f[0]), int(f[1]), int(f[2]), f[3], f[4], f[5]))
        recs[p.stem] = out
    return by_name, recs


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("insns", type=Path)
    ap.add_argument("trace_dir", type=Path)
    ap.add_argument("--top", type=int, default=20, help="functions to break down (by dynamic move count)")
    ap.add_argument("--reasons-per-func", type=int, default=6)
    ap.add_argument("--json", type=Path, help="also write the aggregates here")
    a = ap.parse_args()

    with gzip.open(a.insns, "rt") as fh:
        data = json.load(fh)
    total = data["all_instructions"]
    by_name, traces = load_trace(a.trace_dir)
    if not traces:
        raise SystemExit(f"{a.trace_dir}: no <ns>_<index>.tsv files")

    agg: dict[int, list[int]] = defaultdict(lambda: [0, 0])  # reason -> [dynamic, static]
    per_func: list[tuple[int, str, str, dict[int, list[int]]]] = []
    guest = 0
    unmatched_names: list[tuple[int, str]] = []
    misaligned = 0
    suspicious: dict[str, int] = defaultdict(int)  # spill/reload whose insn has no stack operand
    for name, ins_raw, cs_raw in data["funcs"]:
        ins = {int(o): t for o, t in ins_raw.items()}
        cs = {int(o): c for o, c in cs_raw.items()}
        fdyn = sum(cs.values())
        guest += fdyn
        stems = by_name.get(name, [])
        if not stems:
            unmatched_names.append((fdyn, name))
            continue
        # Several functions can share a demangled name: pick the trace whose
        # move start offsets land on instruction starts most often.
        # Offsets known from either side: objdump text can desync after inline
        # jump tables, callgrind addresses are always real instruction starts.
        known = set(ins) | set(cs)

        def fit(stem: str) -> int:
            return sum(1 for r in traces.get(stem, []) if r.start in known)
        stem = max(stems, key=fit)
        recs = traces.get(stem, [])
        offs = sorted(known)
        fa: dict[int, list[int]] = defaultdict(lambda: [0, 0])
        for r in recs:
            if r.start not in known:
                misaligned += 1
            i = bisect.bisect_left(offs, r.start)
            n = 0
            while i < len(offs) and offs[i] < r.end:
                o = offs[i]
                n += cs.get(o, 0)
                t = ins.get(o, "")
                if t and r.kind in ("spill", "reload") and "(%rsp" not in t and "(%rbp" not in t and "[sp" not in t:
                    suspicious[t.split()[0] if t else "?"] += 1
                i += 1
            fa[r.reason][0] += n
            fa[r.reason][1] += 1
            agg[r.reason][0] += n
            agg[r.reason][1] += 1
        per_func.append((sum(v[0] for v in fa.values()), name, stem, fa))

    moves_dyn = sum(v[0] for v in agg.values())
    pct = lambda n, d: 100.0 * n / d if d else 0.0  # noqa: E731
    print(f"all instructions {total:,}; guest (jitted) {guest:,}; regalloc moves {moves_dyn:,} "
          f"({pct(moves_dyn, total):.2f}% of all, {pct(moves_dyn, guest):.2f}% of guest)")
    if unmatched_names:
        u = sum(d for d, _ in unmatched_names)
        print(f"note: {len(unmatched_names)} executed functions ({pct(u, guest):.2f}% of guest insns) have no trace "
              f"(trampolines/builtins, or compiled without CRANELIFT_RA_TRACE_DIR); top: "
              + ", ".join(n for _, n in sorted(unmatched_names, reverse=True)[:3]))
    if misaligned:
        print(f"note: {misaligned} traced moves start at an offset neither objdump nor callgrind saw "
              "(never executed and hidden by objdump desync after an inline jump table, or an offset mismatch)")
    if suspicious:
        print("WARNING: spill/reload ranges covering insns without a stack operand: "
              + ", ".join(f"{k}x{v}" for k, v in sorted(suspicious.items(), key=lambda kv: -kv[1])[:8]))

    def table(rows: dict[str, list[int]], title: str) -> None:
        print(f"\n== {title}")
        print(f"{'dynamic':>14} {'%moves':>7} {'%all':>6} {'static':>8}  reason")
        for k, (dyn, st) in sorted(rows.items(), key=lambda kv: -kv[1][0]):
            print(f"{dyn:>14,} {pct(dyn, moves_dyn):>6.2f}% {pct(dyn, total):>5.2f}% {st:>8,}  {k}")

    full = {label(r): v for r, v in agg.items()}
    table(full, "by dir / kind / cause (exact)")
    for field in ("cause", "kind", "dir"):
        rows: dict[str, list[int]] = defaultdict(lambda: [0, 0])
        for r, (dyn, st) in agg.items():
            d = decode(r)
            key = str(d[field])
            if field == "cause" and d["spill"]:
                key += "+spillregion"
            rows[key][0] += dyn
            rows[key][1] += st
        table(rows, f"by {field}")

    print(f"\n== top {a.top} functions by dynamic move count")
    per_func.sort(key=lambda t: -t[0])
    for dyn, name, stem, fa in per_func[: a.top]:
        st = sum(v[1] for v in fa.values())
        print(f"\n{dyn:>14,} ({pct(dyn, moves_dyn):.2f}% of moves) {st:,} static  {name}  [{stem}]")
        for r, (d, s) in sorted(fa.items(), key=lambda kv: -kv[1][0])[: a.reasons_per_func]:
            print(f"    {d:>14,} {s:>6,}  {label(r)}")

    if a.json:
        a.json.write_text(json.dumps({
            "all_instructions": total, "guest_instructions": guest, "move_instructions": moves_dyn,
            "by_reason": {str(r): {"label": label(r), **decode(r), "dynamic": v[0], "static": v[1]}
                          for r, v in agg.items()},
            "functions": [{"name": n, "stem": s, "dynamic": d,
                           "by_reason": {str(r): v for r, v in fa.items()}} for d, n, s, fa in per_func],
        }, indent=1))


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        sys.exit(0)
