//! EngineSnapshot — the full-OMS-state DTO written into the journal as a `Snap` record
//! (exchange-core "snapshot-as-command"). Maps become ordered Vecs (tuple keys are not
//! JSON-object-safe; IndexMap insertion order is state); dedup sets are SORTED (canonical).
//! `state_hash` = FNV-1a64 over the canonical serde_json bytes — the determinism fence.

use serde::{Deserialize, Serialize};

use ustr::Ustr;

use crate::account::{MarkKey, PositionEntry, PositionKey};
use crate::order::ManagedOrder;
use crate::risk::{RiskLimits, TradingState};
use crate::BalanceMode;

/// WIRE NOTE (perf audit 2026-07-28, finding #3): `positions`/`marks`/`fees_by_asset` are keyed by
/// the interned [`PositionKey`]/[`MarkKey`]/[`Ustr`] rather than by owned `String`s. That is a
/// PURELY IN-MEMORY change — `Ustr` is serde-transparent (serializes as its `str`, deserializes
/// from one) and `PositionSide` serializes to the same `"BOTH"/"LONG"/"SHORT"` via
/// `rename_all = "UPPERCASE"` — so this DTO's JSON is byte-for-byte what the `String`-keyed shape
/// produced, and with it the [`state_hash`] determinism fence. Pinned by
/// `tests::account_snapshot_key_wire_is_byte_identical`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub venue: String,
    pub mult: Vec<(String, f64)>,
    pub mult_default: f64,
    pub balance_mode: BalanceMode,
    pub positions: Vec<(PositionKey, PositionEntry)>,
    pub realized_pnl: f64,
    pub closed_pnls: Vec<f64>,
    pub balance: f64,
    pub balances_by_asset: Vec<(String, f64)>,
    pub marks: Vec<(MarkKey, f64)>,
    pub funding_paid: f64,
    pub fees_paid: f64,
    pub fees_by_asset: Vec<(Ustr, f64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineSnapshot {
    pub venue: String,
    pub symbol: String,
    pub quote_asset: String,
    pub reduce_only_on_close: bool,
    pub extra_symbols: Vec<String>,
    pub trading_state: TradingState,
    pub limits: RiskLimits,
    pub gate_order_times: Vec<i64>,
    /// insertion order preserved — the registry's reversed-scan semantics depend on it
    pub registry: Vec<(String, ManagedOrder)>,
    pub account: AccountSnapshot,
    /// SORTED — HashSet iteration order is nondeterministic; sorting is the canonical form
    pub seen_trade_ids: Vec<String>,
    pub seen_fsm_trade_ids: Vec<String>,
    pub seen_liq_ids: Vec<String>,
    pub dropped_terminal_on_live: u64,
    pub equity_seed: f64,
    pub collect_applied_fills: bool,
    pub now_ms: i64,
}

/// FNV-1a64 over the canonical JSON bytes. Inline (no dep); NOT DefaultHasher (random seed).
///
/// **Two fields are EXCLUDED from the hash** — the top-level [`EngineSnapshot::now_ms`] and the
/// [`AccountSnapshot::marks`] map — because both are mutated by NON-journaled market-data messages:
/// `runtime.rs`'s dispatch stamps `engine.now_ms = clock.now_ms()` for EVERY ingest message
/// (including market/tick messages, before the exec-lane journaling gate), and mark ticks update
/// `account.marks`. Neither is reproducible by re-folding only the journaled exec lane, so a live
/// session's final snapshot would carry a `now_ms`/`marks` that offline replay cannot re-derive —
/// making the determinism fence spuriously fail on a perfectly valid journal. Excluding them makes
/// the fence verify the RECOVERABLE order/account state, which is the point of the fence.
///
/// Everything else stays hashed. In particular each order's `created_ms` (set from `now_ms` at
/// submit, a JOURNALED `Command`) STAYS in the hash via the `ManagedOrder`s in `registry` and IS
/// reproducible — `QueueClock` replays the recorded per-message now_ms during replay. Only the
/// top-level last-touch `now_ms` and the mark-price cache are omitted.
///
/// Both fields REMAIN in the snapshot structs (restore needs them); the hash simply clones and
/// zeroes them before serializing. Snapshots are infrequent (cadence + shutdown), so the clone is
/// never on a hot path. Both the stored snapshot hashes and the replay recomputation use THIS
/// function, so they stay consistent.
pub fn state_hash(engines: &[EngineSnapshot]) -> u64 {
    // Clone-and-clear the two non-reproducible fields, then hash the canonical JSON of the rest.
    // Same serde shape as before, minus `now_ms`/`marks` content — the FNV-1a algorithm and every
    // other field are unchanged.
    let mut hashable = engines.to_vec();
    for e in &mut hashable {
        e.now_ms = 0;
        e.account.marks.clear();
    }
    let bytes = serde_json::to_vec(&hashable).expect("EngineSnapshot serializes");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in &bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use crate::testing::RecordingClient;
    use crate::{Account, BalanceMode, ExecutionEngine, Outbox, RiskGate, RiskLimits};

    fn engine_with_state() -> ExecutionEngine<RecordingClient> {
        let mut e = ExecutionEngine::new(
            Account::new(1.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::default()),
            RecordingClient::default(),
            "sim",
            "BTCUSDT",
        );
        let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
            "client_order_id": "s1", "venue": "sim", "symbol": "BTCUSDT",
            "side": 1, "qty": 2.0, "order_type": "limit", "price": 101.0
        }))
        .unwrap();
        e.now_ms = 1_000;
        let mut outbox = Outbox::default();
        e.submit_order(&req, 1_000, &mut outbox);
        e
    }

    #[test]
    fn snapshot_restore_round_trips_exactly() {
        let live = engine_with_state();
        let snap = live.snapshot_state();
        let restored = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
        // Debug-format compare = exhaustive field check (catches any field a future dev
        // forgets to add to the DTO). Client/exec_db excluded by comparing re-snapshots.
        assert_eq!(snap, restored.snapshot_state());
        assert_eq!(
            crate::engine_snapshot::state_hash(&[snap]),
            crate::engine_snapshot::state_hash(&[restored.snapshot_state()])
        );
    }

    /// The determinism fence in bytes. `state_hash` is compared ACROSS BINARY VERSIONS
    /// (`vike_core::replay` re-hashes a restored snapshot and compares it to the hash a
    /// possibly-older binary recorded in the journal), so the canonical JSON of a
    /// default-limits snapshot is a persisted format. Adding a field that serializes when it
    /// is off silently changes every recorded hash and surfaces as a spurious "determinism
    /// fence failed" on a perfectly valid journal.
    ///
    /// If this test fails you changed that format. Either mark the new field
    /// `skip_serializing_if` so an off/default value stays ABSENT (the fix for the
    /// `max_slippage_bps`/`require_fillable` knobs), or deliberately bump the journal
    /// `VERSION` and re-pin the constant below.
    #[test]
    fn state_hash_of_a_default_limits_snapshot_is_pinned() {
        let snap = engine_with_state().snapshot_state();
        let json = serde_json::to_string(&snap).unwrap();
        assert!(
            !json.contains("max_slippage_bps") && !json.contains("require_fillable"),
            "off-by-default impact knobs must not reach the canonical JSON: {json}"
        );
        assert!(
            !json.contains("price_collar") && !json.contains("collar_by_symbol"),
            "the off-by-default price collar must not reach the canonical JSON: {json}"
        );
        assert_eq!(
            crate::engine_snapshot::state_hash(&[snap]),
            12_885_289_002_910_966_103,
            "canonical-JSON hash drift"
        );
    }

    #[test]
    fn hash_changes_when_state_changes() {
        let a = engine_with_state().snapshot_state();
        let mut b_eng = engine_with_state();
        b_eng.account.balance += 1.0;
        let b = b_eng.snapshot_state();
        assert_ne!(
            crate::engine_snapshot::state_hash(&[a]),
            crate::engine_snapshot::state_hash(&[b])
        );
    }

    /// state_hash BYTE-IDENTITY for an all-CROSS account (feat/margin-mode-field): a snapshot
    /// carrying a default (Cross/None) position serializes with NEITHER new margin key, so the
    /// FNV-over-canonical-JSON `state_hash` is exactly what it was before the field existed. This
    /// is the journal-determinism fence proof for the default path — the ONE thing this PR must
    /// not perturb.
    #[test]
    fn cross_position_adds_no_margin_keys_to_the_hash_surface() {
        use crate::account::PositionEntry;
        let mut e = engine_with_state();
        e.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        let snap = e.snapshot_state();
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("BTCUSDT"), "precondition: the cross position is in the snapshot");
        assert!(
            !json.contains("margin_mode") && !json.contains("isolated_margin"),
            "an all-cross account must not add either key to the canonical JSON (hash surface): {json}"
        );
    }

    /// THE wire-identity pin for the interned fold keys (perf audit 2026-07-28, finding #3).
    ///
    /// `AccountSnapshot`'s `positions` / `marks` / `fees_by_asset` moved from owned `String` keys
    /// to `(Ustr, Ustr, PositionSide)` / `(Ustr, Ustr)` / `Ustr`. That must be invisible on the
    /// wire, because `state_hash` is FNV over these canonical JSON bytes and is compared ACROSS
    /// BINARY VERSIONS by `vike_core::replay` — a key that rendered differently would fail the
    /// determinism fence on a perfectly valid journal.
    ///
    /// This asserts the EXACT rendered JSON fragments rather than "it round-trips": a round-trip
    /// would still pass if `Ustr` had picked up a newtype/struct representation.
    #[test]
    fn account_snapshot_key_wire_is_byte_identical() {
        use crate::account::PositionEntry;
        let mut e = engine_with_state();
        e.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "LONG".into()),
            PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        e.account.set_mark_from("sim", "BTCUSDT", 105.0, crate::MarkSource::VenueMark, 1_000);
        e.account.fees_by_asset.insert(ustr::ustr("BNB"), 0.5);
        let json = serde_json::to_string(&e.snapshot_state()).unwrap();

        // A position key is THREE bare JSON strings, in the same order and the same spelling the
        // `(String, String, String)` key produced — `PositionSide::Long` renders "LONG".
        assert!(
            json.contains(r#"[["sim","BTCUSDT","LONG"],{"size":2.0,"avg_px":100.0}]"#),
            "position key must render as three plain JSON strings: {json}"
        );
        // A mark key is TWO bare strings, and the per-asset fee key one.
        assert!(json.contains(r#"[["sim","BTCUSDT"],105.0]"#), "mark key wire changed: {json}");
        assert!(json.contains(r#"["BNB",0.5]"#), "fee-asset key wire changed: {json}");
        // Nothing interned may leak a wrapper representation.
        assert!(!json.contains("Ustr") && !json.contains("PositionSide"), "{json}");

        // …and the whole DTO still round-trips into the same value.
        let back: super::EngineSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e.snapshot_state());
    }

    /// The `PositionSide` key element is a CLOSED SET, so a venue label outside
    /// `{BOTH,LONG,SHORT}` folds to `Both` instead of minting a fourth, permanently-orphaned key.
    /// Documented as a deliberate narrowing (see `account.rs`'s module doc): every adapter already
    /// normalizes to those three labels before `ReconcileSnapshot::position_sides`, so no live
    /// producer changes behavior — but the fold direction is pinned here so a future adapter that
    /// forgets to normalize fails loudly in review rather than silently accumulating ghost keys.
    #[test]
    fn an_unrecognized_side_label_folds_to_both_not_a_fourth_key() {
        use vike_model::events::PositionSide;
        let key: crate::account::PositionKey = ("sim".into(), "BTCUSDT".into(), "long".into());
        assert_eq!(key.2, PositionSide::Both);
        for (label, want) in [
            ("BOTH", PositionSide::Both),
            ("LONG", PositionSide::Long),
            ("SHORT", PositionSide::Short),
        ] {
            assert_eq!(PositionSide::from(label), want, "{label}");
            assert_eq!(want.to_string(), label, "{label} must render back unchanged");
        }
    }

    /// An ISOLATED position survives the full `EngineSnapshot` serde round-trip carrying its mode
    /// AND its allocated wallet — the carrier is persisted, ready for the scope-parameterized-law
    /// PR. (Also exercises the `#[serde(default)]` compat: the surrounding cross positions have no
    /// keys, the isolated one does, and both deserialize.)
    #[test]
    fn isolated_position_round_trips_through_engine_snapshot() {
        use crate::account::PositionEntry;
        use vike_model::MarginMode;
        let mut e = engine_with_state();
        e.account.positions.insert(
            ("sim".into(), "ETHUSDT".into(), "BOTH".into()),
            PositionEntry {
                size: 3.0,
                avg_px: 50.0,
                margin_mode: MarginMode::Isolated,
                isolated_margin: Some(75.0),
            },
        );
        let snap = e.snapshot_state();
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains(r#""margin_mode":"Isolated""#), "{json}");
        let back: super::EngineSnapshot = serde_json::from_str(&json).unwrap();
        let (_, pos) = back.account.positions.iter().find(|((_, s, _), _)| s == "ETHUSDT").unwrap();
        assert_eq!(pos.margin_mode, MarginMode::Isolated);
        assert_eq!(pos.isolated_margin, Some(75.0));
        // and the pinned all-cross hash from the sibling test is unaffected by this isolated pos
        // living in a DIFFERENT snapshot — restore is exact.
        let restored = ExecutionEngine::from_snapshot(&back, RecordingClient::default());
        assert_eq!(back, restored.snapshot_state());
    }

    /// `from_snapshot` PriceBoard re-seed (self-consistency): `PriceBoard` sits OUTSIDE
    /// `EngineSnapshot`, so a journal restore rebuilds `Account.marks` but would leave the read-side
    /// board EMPTY — and `resolved_position_price` (the price the resolver-routed gate now reads)
    /// would return `Missing`/0.0 for a symbol whose restored mark is actually fresh, the two mark
    /// stores disagreeing until the next tick. Assert the board answers the restored mark BEFORE any
    /// tick, matching `Account.marks`.
    #[test]
    fn from_snapshot_reseeds_price_board_from_restored_marks() {
        use crate::account::PositionEntry;
        use crate::price_board::PriceCfg;
        let mut e = engine_with_state();
        // A real open position plus a fresh venue mark in Account.marks.
        e.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        e.account.set_mark_from("sim", "BTCUSDT", 105.0, crate::MarkSource::VenueMark, 1_000);
        let snap = e.snapshot_state();
        assert!(
            snap.account
                .marks
                .iter()
                .any(|((v, s), m)| v == "sim" && s == "BTCUSDT" && *m == 105.0),
            "precondition: the mark rode into the snapshot"
        );

        let restored = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
        let pos = restored.position_size("BOTH");
        assert_eq!(pos, 2.0, "precondition: the position restored");
        // BEFORE any tick: the resolver must return the restored mark, not Missing/0.0.
        assert_eq!(
            restored.resolved_position_price("sim", "BTCUSDT", pos, &PriceCfg::default()),
            Some(105.0),
            "PriceBoard must be re-seeded from Account.marks on restore"
        );
        // and the account side agrees — the two mark stores are consistent post-restore.
        assert_eq!(restored.account.mark_of("sim", "BTCUSDT"), Some(105.0));
    }
}
