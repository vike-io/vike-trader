//! Test helpers shared by this module's per-file `mod tests` blocks — moved verbatim out of the
//! single `mod tests` `journal.rs` used to carry, and kept in ONE place so the split did not
//! duplicate them. `#[cfg(test)]` at the `mod` declaration, so a normal build never compiles it.

use crate::scratch::Scratch;
use std::path::{Path, PathBuf};

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
