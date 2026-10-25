# ix artifact fixtures

`.cwasm` files compiled by earlier fork builds, for
`ix_indirect_old_artifacts_rejected` (`tests/all/ix_indirect.rs`). They carry
the old hand-bumped version tags (`50+ix1`, `50+ix3`) and no ABI fields in
their metadata, and must be refused under every version strategy.

| file | fork commit | what it was |
|---|---|---|
| `ca78074.cwasm` | ca78074 (ix-intrinsics) | `ix` intrinsics, before the ABI hash |
| `a162330.cwasm` | a162330 (ix-indirect) | static `call_indirect` dispatch v1 |
| `217de37.cwasm` | 217de37 (ix-epoch-poll) | poll-word interruption, `+ix3` |

Made on x86_64 Linux (dev-aws-5, rustc 1.99.0) with that commit's debug CLI
and default settings (`bench/results/2026-10-25-indirect-fixes/run.py fixtures`
in wasm-host):

    cargo build --bin wasmtime
    target/debug/wasmtime compile -o <commit>.cwasm tests/all/ix_fixtures/fixture.wat

The version string is checked before the target, so the test holds on any
host architecture.
