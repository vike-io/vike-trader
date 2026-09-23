//! Test helpers shared by this module's per-file `mod tests` blocks — moved verbatim out of the
//! single `mod tests` `journal.rs` used to carry, and kept in ONE place so the split did not
//! duplicate them. `#[cfg(test)]` at the `mod` declaration, so a normal build never compiles it.

use std::path::{Path, PathBuf};

/// A scratch directory owned by the test that allocated it, removed when the guard drops.
///
/// ⚠ **A SECOND guard, and the duplication is forced rather than chosen.** The original is
/// `crates/vike-core/src/scratch.rs`'s `Scratch`, and these very suites are why it exists: they
/// leaked **27,471 `/tmp/vjl-*` directories totalling 211 GB** on the the CI box CI box (measured
/// 2026-08-23). It could not come with them, and no edge reaches it: that module is declared
/// `#[cfg(test)] mod scratch;` and the type is `pub(crate)`, so it is invisible across a crate
/// boundary on a NORMAL dependency and on a dev one alike. Its own doc refuses the obvious cure
/// in advance — "no `test-support` feature to thread through CI — vike-core declares no features at
/// all, and a feature the derived roster lane does not pass would leave the `tests/` binaries
/// uncovered".
///
/// The copy is cheap BECAUSE the gate splits the same way:
/// `crates/vike-ops/tests/journal_scratch_gate.rs` holds its STRICT one-guard-file rule over
/// `crates/vike-core` ALONE (its `tree_sources` skips that directory by NAME), and every other
/// crate answers to the PROPERTY rule — show a self-deleting handle, or be pinned. This type owns
/// a `tempfile::TempDir`, which is what that rule asks for. The `vjl-` prefix is kept
/// deliberately: a directory found on the CI box still names the family that leaked, whichever
/// crate minted it.
///
/// ⚠ `vike_model::scratch::ScratchDir` is NOT the answer here, and the question is worth
/// answering in advance, because it is fully `pub`, this crate already depends on vike-model, and
/// it owns a `Drop` that removes its directory. It is a different JOB: the retention rule for
/// `<project>/tmp`, deliberately pure — `create_in(root, tag)` takes the root from a composition
/// root and reads no environment — so a test reaching for it would have to mint the temp root
/// itself, which is the whole of what `tempfile` is doing below.
#[derive(Debug)]
pub(super) struct Scratch {
    /// The owning temp root. Never read — held so its `Drop` runs at the end of the test.
    _root: tempfile::TempDir,
    /// The path handed to the code under test: the root itself, or a child of it.
    path: PathBuf,
}

impl Scratch {
    /// A path INSIDE a fresh temp root that does **not** exist yet — for the suites whose code
    /// under test calls `create_dir_all` itself, and the ones that assert on a directory which
    /// never appears. The root is still owned, so whatever the test creates there is cleaned up.
    pub(super) fn reserved(tag: &str) -> Self {
        let root = Self::root(tag);
        let path = root.path().join("j");
        Self { _root: root, path }
    }

    /// A directory that already EXISTS — for the suites that need it in place before the code
    /// under test runs.
    pub(super) fn created(tag: &str) -> Self {
        let root = Self::root(tag);
        let path = root.path().to_path_buf();
        Self { _root: root, path }
    }

    /// The temp root, prefixed with the caller's tag so a directory seen mid-run is attributable.
    fn root(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("vjl-{tag}-"))
            .tempdir()
            .expect("create scratch temp dir")
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// A scratch journal directory that **exists on entry and is removed when the returned guard
/// drops**. Hold it for the whole test: binding it to `_` deletes the directory immediately.
/// It used to hand back a bare `PathBuf` and leak the directory forever — see
/// `crates/vike-core/src/scratch.rs` for the 211 GB that cost.
pub(super) fn tmp_dir(tag: &str) -> Scratch {
    Scratch::created(&format!("test-{tag}"))
}

pub(super) fn ingest(n: u64) -> vike_exec::Ingest {
    vike_exec::Ingest::Command(vike_exec::Command::Order(vike_exec::OrderIntent::Cancel(format!(
        "c{n}"
    ))))
}

/// A deliberately FAT ingest (~4 KiB framed) so a sync test can cross several `sync_chunk`
/// boundaries — whose FLOOR is `SYNC_CHUNK_MIN` = 4 MiB — in ~1 000 appends instead of ~50 000.
pub(super) fn fat_ingest(n: u64) -> vike_exec::Ingest {
    vike_exec::Ingest::Command(vike_exec::Command::Order(vike_exec::OrderIntent::Cancel(format!(
        "c{n}-{}",
        "x".repeat(4000)
    ))))
}

/// A minimal single-engine `Snap` payload for the prune tests (the engine state itself is
/// irrelevant — pruning keys only off record seqs, not snapshot contents).
pub(super) fn snap_engines() -> Vec<vike_exec::EngineSnapshot> {
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate, RiskLimits};
    let e = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    vec![e.snapshot_state()]
}

/// The `.vjl` segment file paths currently in `dir`.
pub(super) fn seg_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension().and_then(|s| s.to_str()) == Some("vjl")).then_some(p)
        })
        .collect()
}

/// The message the LATENCY GATE actually journals — `crates/vike-core/tests/runtime_latency.rs`'s
/// `fill_with_coid` field-for-field, so a measurement taken here prices the same bytes the gate's
/// journal variants write (~2.5x the [`ingest`] cancel above, which is why the two are separate).
/// Kept in step BY HAND: the two files are in different targets and nothing links them.
///
/// `cfg(linux)` to match its ONLY caller, `journal::writer`'s `measure_append_cost_split` — without
/// it, a Windows `cargo check` reports it dead, which `just windows-check` prints and no gate
/// catches.
#[cfg(target_os = "linux")]
pub(super) fn fill_ingest(ts: i64) -> vike_exec::Ingest {
    vike_exec::Ingest::Event(vike_model::events::Event::Fill(vike_model::events::FillEvent {
        trade_id: "t".into(),
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: "OTHER".into(),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts,
        mark_price: None,
        position_side: "BOTH".into(),
    }))
}
