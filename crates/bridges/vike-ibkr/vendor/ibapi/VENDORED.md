# Vendored `ibapi`

- **Upstream:** https://github.com/wboayue/rust-ibapi
- **Version:** 3.2.1
- **Commit:** `caac6e8d36448545b3835d76b4f5823ede42f7b1`
- **Copied:** 2026-07-14
- **License:** MIT (see `LICENSE`)
- **Consumed with:** `default-features = false, features = ["sync"]`

⚠ **Those `Upstream` / `Version` / `Commit` lines are LOAD-BEARING, not decoration.**
`scripts/ibapi_vendor_drift.sh` parses them to decide what to fetch and compare, so a refresh that
edits the tree without editing them fails the gate (and vice versa — `Version` is cross-checked
against the committed `Cargo.toml`'s own `version`). Keep the exact `- **Field:** value` shape.

## Why vendored in-tree

Full ownership of the IBKR wire-protocol client. Consumed as a path dependency by
`vike-ibkr` with the **blocking (sync) client only** — `default-features = false` drops the
async/tokio client entirely so no tokio runtime enters the otherwise-blocking bridge tree.
With `sync` alone the blocking client is reachable at both `ibapi::Client` and the canonical
`ibapi::client::blocking::Client`; the socket backend uses the latter.

## What was copied

`src/`, `Cargo.toml`, `LICENSE`. NOT copied: `.git`, `examples/`, `benches/`, `tests/`,
`integration/`, `tools/`, CI/workflow files, docs.

⚠ Also not copied: upstream's **`rustfmt.toml`** (`max_width = 150`). The copy was subsequently
reformatted under this repo's own `rustfmt.toml`, so **the committed `src/` is not byte-identical
to upstream** — almost every file re-wraps, and nothing else about them changed. This is why the
drift gate compares `src/` under rustfmt normalization instead of byte-for-byte; the argument, and
what that does and does not cover, is in `scripts/ibapi_vendor_drift.sh`'s `normalize`
section. One file escaped the reformat and sits here as upstream's verbatim bytes:
`src/proto/protobuf.rs` reaches the crate through `include!` rather than a `mod`, and rustfmt only
walks the `mod` tree.

## Cargo.toml edits (see the header comment in that file)

The vendored `Cargo.toml` was trimmed so it resolves as a standalone package:
- removed the `[workspace]` table (its `integration/*` + `tools/*` members were not copied;
  the crate is workspace-excluded by the vike root manifest instead);
- removed every `[[example]]` target declaration (examples/ was not copied);
- removed `[dev-dependencies]` (only used by the dropped examples/tests/benches).

The package **version**, the **feature graph**, and all **dependency versions** are verbatim
upstream — unchanged, and gated: those three trims reproduce the committed manifest byte-for-byte.

## src/ edits

Three test-only modules were dropped — each a `#[cfg(test)] #[path = "..."] mod tests;`
declaration plus the file it names:

- `src/trace/async_tests.rs` (declared from `src/trace/async.rs`)
- `src/trace/sync_tests.rs` (declared from `src/trace/sync.rs`)
- `src/transport/recorder_tests.rs` (declared from `src/transport/recorder.rs`)

No build in this tree compiles them: the crate is workspace-excluded, so nothing runs `cargo test`
in it. **This was undocumented until the drift gate measured it** — it is recorded here and in that
script's `TEST_MODULE_DROPS` because a copy that silently lacks upstream files is exactly what the
gate exists to notice, and an unrecorded omission is indistinguishable from an accident. Nothing
else was removed from `src/`, and the file set is gated in both directions.

## Refresh procedure

Re-clone upstream at the desired tag, re-copy `src/ Cargo.toml LICENSE`, re-apply the three
Cargo.toml trims and the three `src/` test-module drops above, and update the version/commit/date
fields here. Then **run the gate before pushing**:

```sh
bash scripts/ibapi_vendor_drift.sh
```

It fetches the newly-recorded commit and fails on anything the record does not account for.
`.github/workflows/ibapi-vendor.yml` runs the same script on any PR touching this tree.
