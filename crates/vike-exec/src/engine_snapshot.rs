//! EngineSnapshot — the full-OMS-state DTO written into the journal as a `Snap` record
//! (exchange-core "snapshot-as-command"). Maps become ordered Vecs (tuple keys are not
//! JSON-object-safe; IndexMap insertion order is state); dedup sets are SORTED (canonical).
//! `state_hash` = FNV-1a64 over the canonical serde_json bytes — the determinism fence.

use serde::{Deserialize, Serialize};

use ustr::Ustr;

use crate::BalanceMode;
use crate::account::{MarkKey, PositionEntry, PositionKey};
use vike_model::ManagedOrder;
use vike_model::RiskLimits;
use vike_model::TradingState;

/// WIRE NOTE: `positions`/`marks`/`fees_by_asset` are keyed by the interned
/// [`PositionKey`]/[`MarkKey`]/[`Ustr`] rather than owned `String`s, a PURELY IN-MEMORY choice:
/// `Ustr` is serde-transparent and `PositionSide` serializes to `"BOTH"/"LONG"/"SHORT"`
/// (`rename_all = "UPPERCASE"`), so this DTO's JSON — and with it the [`state_hash`] determinism
/// fence — is byte-for-byte the `String`-keyed shape's. Pinned by
/// `tests::account_snapshot_key_wire_is_byte_identical`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub venue: String,
    /// The per-unit factor of every LINEAR contract value that is not the default (`ContractValue`
    /// `Linear`; the name and the JSON are the scalar grid's, unchanged).
    pub mult: Vec<(String, f64)>,
    /// The per-unit factor of every INVERSE contract value (decision 0170). Sparse on purpose: empty
    /// for every engine without an inverse symbol, and then ABSENT from the canonical JSON, so the
    /// [`state_hash`] of such an engine is byte-for-byte what it was before this field existed and a
    /// snapshot written before it decodes (`default`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inverse: Vec<(String, f64)>,
    /// The multi-currency half of the ledger: which symbols are quoted in a currency other than the
    /// account's, and the rate of each such currency. `mult` and `inverse` above keep the BASE value
    /// of those symbols (the venue's own unit, never multiplied by a rate), so a restore recomputes
    /// the effective value from base and rate with the same function and a rate change leaves no
    /// divide-back rounding in the snapshot. Sparse on purpose: `None` for every account without a
    /// foreign-quoted symbol, and then ABSENT from the canonical JSON, so the [`state_hash`] of such
    /// an engine is byte-for-byte what it was before this field existed and a snapshot written
    /// before it decodes (`default`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fx: Option<FxSnapshot>,
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

/// What [`AccountSnapshot::fx`] persists: the foreign-quoted symbols and the rate per currency.
/// Lists, not maps (the DTO convention: insertion order is state and is hashed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FxSnapshot {
    /// The currency every rate below is expressed in: the engine's `quote_asset` at the admission
    /// that first needed a rate. Stored so the table is self-describing and so the fee rule can tell
    /// a fee in the account's currency from a fee in a third one.
    pub account_ccy: String,
    /// `(symbol, quote currency)` of every foreign-quoted symbol, in admission order. The symbol's
    /// BASE contract value is in [`AccountSnapshot::mult`] / [`AccountSnapshot::inverse`].
    pub symbols: Vec<(String, String)>,
    /// One row per quote currency a symbol above is quoted in.
    pub rates: Vec<FxRateSnapshot>,
}

/// One currency's standing rate: account-currency units per ONE unit of `ccy`, and when it was
/// observed (the node's clock, journaled with the admission that carried it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FxRateSnapshot {
    pub ccy: String,
    pub rate: f64,
    pub as_of_ms: i64,
    /// A rate refused for moving more than one step at once, remembered so that the SAME rate offered
    /// again, stamped later ([`Self::pending_as_of_ms`]), is accepted (a real jump) and a lone flipped
    /// one is not. Absent while there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<f64>,
    /// The stamp of the offer [`Self::pending`] remembers: what a confirming offer must be later than
    /// (`MIN_QUOTE_RATE_CONFIRM_GAP_MS`), and what the memory's expiry is measured from. Absent with
    /// `pending`; a `pending` without it (a snapshot written before it existed) restores as no memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_as_of_ms: Option<i64>,
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
    /// WHICH ACCOUNT's engine this is (journal v17): the engine's
    /// [`crate::ExecutionEngine::route_key`] when it is NOT its venue (a labelled account,
    /// `venue#LABEL`), `None` when it is (the venue's sole or default account).
    ///
    /// Written only in the labelled case so a single-account engine's canonical JSON — and with it
    /// [`state_hash`], which is compared across binary versions — is byte-identical to a v16 one
    /// (pinned by `tests::state_hash_of_a_default_limits_snapshot_is_pinned`). The cost of that
    /// encoding is that a v16 `Snap` reads back `None` for EVERY engine, labelled or not; a reader
    /// that matches snapshots to engines by route key (`vike_core::replay::restore_for_boot`) treats
    /// two snapshots of one venue that both read `None` as unmatchable rather than guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_key: Option<String>,
    /// The node TOLD this engine that its account currency is `"USDT"`, the very spelling the
    /// constructor guesses (`ExecutionEngine::set_account_ccy`). A symbol quoted in another currency
    /// is admitted only once the engine's currency is a statement and not that guess
    /// (`ExecutionEngine::account_ccy_known`, `account-ccy-unset`), and a replay that starts from
    /// this `Snap` and folds a journaled foreign admission must take the verdict the live engine
    /// took. Every other told currency is already visible as a `quote_asset` that is not the guess,
    /// so only this one case needs a field.
    ///
    /// Written only when `true`, so every engine that was never told, or told any currency but
    /// `"USDT"` (every FX account), keeps the canonical JSON, and with it the [`state_hash`], it
    /// always had.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub account_ccy_set: bool,
    /// WHOSE BOOK this is: the tier the engine ran at (`paper`/`demo`/`live`), so a boot restore can
    /// refuse to put a `demo` account's positions into the `live` engine that took over its route key
    /// (the Binance lane twin `binance#PERP` has the same key at both tiers) or a `paper` book into a
    /// `demo` one. `None` = UNKNOWN: a snapshot written before this field, or by an engine nobody told
    /// its tier ([`crate::ExecutionEngine::set_tier`]; `new` and `from_snapshot` of a tier-less
    /// snapshot seed `Paper`, which is not proof), and a reader treats unknown as it always did,
    /// restores it (`vike_core`'s `apply_restored_engines`); the next snapshot of an engine that was
    /// told carries the tier.
    ///
    /// Skipped while `None`, so an engine that was never told keeps its canonical JSON byte for byte.
    /// ⚠ NOT hashed ([`state_hash`] clears it, like [`Self::now_ms`]): it is assembly configuration,
    /// not folded state, and a hash that covered it would make a build that predates the field (a
    /// rollback) disagree with the hash this one recorded for every `demo`/`live` engine. No journal
    /// format step either (`vike_journal`'s `format.rs`): a build that predates the field ignores the
    /// key and restores tier-blind, as it always did, so there is no new way to misread the journal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<vike_model::AccountTier>,
}

/// FNV-1a64 over the canonical JSON bytes (`vike_util::fnv::fnv1a64`, a frozen algorithm);
/// NOT DefaultHasher (random seed).
///
/// **Two fields are EXCLUDED from the hash** — the top-level [`EngineSnapshot::now_ms`] and the
/// [`AccountSnapshot::marks`] map — because NON-journaled market-data messages mutate both: the
/// runtime's dispatch stamps `engine.now_ms` for EVERY ingest message (before the exec-lane
/// journaling gate), and mark ticks update `account.marks`. Re-folding only the journaled exec lane
/// cannot re-derive them, so hashing them would fail the fence on a valid journal; the fence
/// verifies the RECOVERABLE order/account state. Each order's `created_ms` (set from `now_ms` at a
/// JOURNALED submit) stays hashed and IS reproducible: `QueueClock` replays the recorded
/// per-message `now_ms`.
///
/// **A third is excluded for the same reason: each registry order's `OrderRequest::data_delay`**
/// (decision 0140). The live core stamps it at submit from the delay labels its feeds report on the
/// NON-journaled `Ingest::DataDelay` lane, so a re-fold of the journaled exec lane submits the same
/// order UNLABELLED; hashing the label would fail the fence on a valid journal of a box trading off
/// delayed data. The label stays in the snapshot (a restore keeps it), exactly like the two above.
///
/// **A fourth is excluded because it is only a promise, not state: each registry order's
/// `ManagedOrder::deferred_events`** (W19g), the fills and terminals an `Initialized` order holds for the
/// answer to its lost `OrderSubmitted`. The order's STATUS and fills are hashed as they land; the buffer is
/// not, so an order that took the ordinary road (it never replays them) hashes as it did before the
/// buffer existed, and an older build's recorded hash still fences. It stays in the snapshot, so a refold
/// that starts from a `Snap` taken while an order holds one replays it as the live run did.
///
/// **A fifth is excluded because it is configuration, not state: [`EngineSnapshot::tier`].** What the
/// engine's tier is does not move any folded number, and a hash that covered it would disagree with
/// the one a build that predates the field recomputes for the same journal (the rollback), for every
/// `demo` and `live` engine. A restore compares it directly instead (`apply_restored_engines`).
///
/// All five REMAIN in the snapshot structs (restore needs them); the hash clones and zeroes
/// them, off any hot path (snapshots are infrequent). The stored hashes and the replay
/// recomputation both use THIS function.
pub fn state_hash(engines: &[EngineSnapshot]) -> u64 {
    // Clone-and-clear the non-reproducible fields, then hash the canonical JSON of the rest.
    let mut hashable = engines.to_vec();
    for e in &mut hashable {
        e.tier = None;
        e.now_ms = 0;
        e.account.marks.clear();
        for (_, order) in &mut e.registry {
            order.request.data_delay = None;
            order.deferred_events.clear();
        }
    }
    let bytes = serde_json::to_vec(&hashable).expect("EngineSnapshot serializes");
    vike_util::fnv::fnv1a64(&bytes)
}

#[cfg(test)]
mod tests {
    use crate::testing::RecordingClient;
    use crate::{Account, BalanceMode, ExecutionEngine, Outbox, RiskGate};
    use vike_model::RiskLimits;
    use vike_model::events::PositionSide;

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
    /// Decision 0140: an order's delay label is stamped from a NON-journaled lane, so the fence must
    /// not see it — a labelled registry and its unlabelled replay hash alike — while the snapshot
    /// itself keeps the label for a restore.
    #[test]
    fn an_orders_delay_label_is_kept_in_the_snapshot_and_left_out_of_the_hash() {
        let plain = engine_with_state().snapshot_state();
        let mut labelled = plain.clone();
        for (_, order) in &mut labelled.registry {
            order.request.data_delay = Some(vike_model::DataDelay::DELAYED);
        }
        assert!(!labelled.registry.is_empty(), "the fixture rests an order");
        assert_eq!(
            crate::engine_snapshot::state_hash(std::slice::from_ref(&labelled)),
            crate::engine_snapshot::state_hash(std::slice::from_ref(&plain)),
        );
        let json = serde_json::to_string(&labelled).unwrap();
        assert!(json.contains(r#""data_delay":{"Delayed":{"lag_ms":null}}"#), "{json}");
        assert!(!serde_json::to_string(&plain).unwrap().contains("data_delay"));
    }

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
        assert!(
            !json.contains("max_account_exposure"),
            "the off-by-default ACCOUNT-aggregate exposure ceiling must not reach the canonical \
             JSON: {json}"
        );
        assert!(
            !json.contains("max_sizing_equity"),
            "the off-by-default SIZING-EQUITY ceiling must not reach the canonical JSON: {json}"
        );
        assert_eq!(
            crate::engine_snapshot::state_hash(&[snap]),
            12_885_289_002_910_966_103,
            "canonical-JSON hash drift"
        );
    }

    /// The exact canonical bytes of [`RiskLimits`] on its own, OFF and FULLY ARMED, so a move of
    /// the type to another crate (or any edit to its serde attributes) is checked against the
    /// journal format directly: the bytes carry field names and values, never a crate path, and
    /// this is the proof. The armed value sets every optional axis — both ceilings, the impact
    /// knobs, the collar and its per-symbol map, and a full plus a partial per-symbol grid.
    #[test]
    fn risk_limits_canonical_bytes_are_pinned_off_and_fully_armed() {
        use vike_model::{PriceCollar, SymbolGrid};
        let off = RiskLimits::default();
        let off_json = concat!(
            r#"{"tick_size":null,"lot_size":null,"min_notional":null,"min_qty":null,"#,
            r#""max_notional_per_order":null,"max_total_exposure":null,"#,
            r#""max_orders_per_window":null,"window_ms":0,"max_leverage":null,"#,
            r#""block_reduce_only_overshoot":false,"im_requirement":null,"im_by_symbol":{},"#,
            r#""required_free_bp_pct":0.0}"#,
        );
        assert_eq!(serde_json::to_string(&off).unwrap(), off_json);
        assert_eq!(serde_json::from_str::<RiskLimits>(off_json).unwrap(), off);

        let mut armed = RiskLimits {
            tick_size: Some(0.5),
            lot_size: Some(0.01),
            min_notional: Some(10.0),
            min_qty: Some(0.001),
            max_notional_per_order: Some(25_000.0),
            max_total_exposure: Some(100_000.0),
            max_account_exposure: Some(250_000.0),
            max_sizing_equity: Some(50_000.0),
            max_orders_per_window: Some(5),
            window_ms: 1_000,
            max_leverage: Some(10.0),
            block_reduce_only_overshoot: true,
            im_requirement: Some(0.1),
            required_free_bp_pct: 0.05,
            max_slippage_bps: Some(25.0),
            require_fillable: true,
            price_collar: Some(PriceCollar { pct: 0.1, abs_floor: 0.02 }),
            ..RiskLimits::default()
        };
        armed.im_by_symbol.insert("ETHUSDT".into(), 0.2);
        armed.collar_by_symbol.insert("ETHUSDT".into(), PriceCollar { pct: 0.05, abs_floor: 1.0 });
        armed.grid_by_symbol.insert(
            "ETHUSDT".into(),
            SymbolGrid {
                tick_size: Some(0.01),
                lot_size: Some(0.001),
                min_notional: Some(5.0),
                min_qty: Some(0.001),
                on_demand: false,
            },
        );
        armed
            .grid_by_symbol
            .insert("SOLUSDT".into(), SymbolGrid { lot_size: Some(0.1), ..SymbolGrid::default() });
        let armed_json = concat!(
            r#"{"tick_size":0.5,"lot_size":0.01,"min_notional":10.0,"min_qty":0.001,"#,
            r#""max_notional_per_order":25000.0,"max_total_exposure":100000.0,"#,
            r#""max_account_exposure":250000.0,"max_sizing_equity":50000.0,"#,
            r#""max_orders_per_window":5,"window_ms":1000,"max_leverage":10.0,"#,
            r#""block_reduce_only_overshoot":true,"im_requirement":0.1,"#,
            r#""im_by_symbol":{"ETHUSDT":0.2},"required_free_bp_pct":0.05,"#,
            r#""max_slippage_bps":25.0,"require_fillable":true,"#,
            r#""price_collar":{"pct":0.1,"abs_floor":0.02},"#,
            r#""collar_by_symbol":{"ETHUSDT":{"pct":0.05,"abs_floor":1.0}},"#,
            r#""grid_by_symbol":{"ETHUSDT":{"tick_size":0.01,"lot_size":0.001,"#,
            r#""min_notional":5.0,"min_qty":0.001},"SOLUSDT":{"lot_size":0.1}}}"#,
        );
        assert_eq!(serde_json::to_string(&armed).unwrap(), armed_json);
        assert_eq!(serde_json::from_str::<RiskLimits>(armed_json).unwrap(), armed);
    }

    /// v17's `route_key`: the default account writes NO key (so the pinned hash above is
    /// untouched), a labelled engine writes its key, and `from_snapshot` gives it back.
    #[test]
    fn a_snapshot_without_route_key_hashes_byte_identically() {
        let default = engine_with_state().snapshot_state();
        assert_eq!(default.route_key, None, "route key == venue writes none");
        let json = serde_json::to_string(&default).unwrap();
        assert!(!json.contains("route_key"), "{json}");

        let mut labelled = engine_with_state();
        labelled.route_key = "sim#ALT".into();
        let snap = labelled.snapshot_state();
        assert_eq!(snap.route_key.as_deref(), Some("sim#ALT"));
        let back = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
        assert_eq!(back.route_key, "sim#ALT", "the restored engine routes as the account it was");
        assert_eq!(back.venue, "sim", "and its venue stays the canonical id");
        assert_eq!(back.snapshot_state(), snap);
    }

    /// `tier`: an engine nobody told its tier writes NO key (the seed says nothing, so its canonical
    /// JSON is what it always was), a told one writes it - `paper` included, the case a boot restore
    /// needs to tell a paper book from an old snapshot - and a tier set by plain assignment to
    /// anything but `Paper` is a statement too.
    #[test]
    fn only_a_stated_tier_reaches_the_snapshot() {
        use vike_model::AccountTier;
        let unstated = engine_with_state().snapshot_state();
        assert_eq!(unstated.tier, None);
        assert!(!serde_json::to_string(&unstated).unwrap().contains("tier"));

        for tier in AccountTier::ALL {
            let mut e = engine_with_state();
            e.set_tier(tier);
            let snap = e.snapshot_state();
            assert_eq!(snap.tier, Some(tier), "a told {tier:?} engine says so");
            let json = serde_json::to_string(&snap).unwrap();
            assert!(json.ends_with(&format!(r#","tier":"{}"}}"#, tier.as_str())), "{json}");
        }

        let mut assigned = engine_with_state();
        assigned.tier = AccountTier::Live;
        assert_eq!(assigned.snapshot_state().tier, Some(AccountTier::Live));
    }

    /// The hash does not see the tier (`state_hash` clears it): the pinned hash above holds for a
    /// `demo`/`live`/`paper` engine alike, and a hash this build records is the one a build that
    /// predates the field recomputes.
    #[test]
    fn the_tier_is_not_in_the_state_hash() {
        use vike_model::AccountTier;
        let want = crate::engine_snapshot::state_hash(&[engine_with_state().snapshot_state()]);
        assert_eq!(want, 12_885_289_002_910_966_103, "the pinned hash of the unstated engine");
        for tier in AccountTier::ALL {
            let mut e = engine_with_state();
            e.set_tier(tier);
            assert_eq!(crate::engine_snapshot::state_hash(&[e.snapshot_state()]), want, "{tier:?}");
        }
    }

    /// `from_snapshot` gives the tier back (a replay core re-snapshots what it was handed, so the
    /// boot restore sees the journal's tier and not a seed), and a tier-less snapshot comes back
    /// tier-less: UNKNOWN stays unknown through the replay core, which is what keeps the first restart
    /// after the upgrade restoring as it always did.
    #[test]
    fn a_rebuilt_engine_re_snapshots_the_tier_it_was_given() {
        use vike_model::AccountTier;
        for tier in AccountTier::ALL {
            let mut e = engine_with_state();
            e.set_tier(tier);
            let snap = e.snapshot_state();
            let back = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
            assert_eq!(back.tier, tier);
            assert_eq!(back.snapshot_state(), snap, "{tier:?}");
        }
        let old = engine_with_state().snapshot_state();
        assert_eq!(old.tier, None);
        let back = ExecutionEngine::from_snapshot(&old, RecordingClient::default());
        assert_eq!(back.tier, AccountTier::Paper, "the seed");
        assert_eq!(back.snapshot_state().tier, None, "and it is still not a statement");
    }

    /// Both directions of the compatibility the journal needs: a snapshot WITHOUT the key reads back
    /// `None`, and one carrying the key AND a key this build has never heard of still parses (the
    /// struct takes no `deny_unknown_fields`, which is what a build that predates `tier` relies on to
    /// read a journal this one wrote).
    #[test]
    fn a_snapshot_reads_with_or_without_a_tier_and_with_keys_it_does_not_know() {
        let mut e = engine_with_state();
        e.set_tier(vike_model::AccountTier::Demo);
        let mut tree = serde_json::to_value(e.snapshot_state()).unwrap();
        tree.as_object_mut().unwrap().insert("a_future_field".into(), 1.into());
        let parsed: super::EngineSnapshot = serde_json::from_value(tree.clone()).unwrap();
        assert_eq!(parsed.tier, Some(vike_model::AccountTier::Demo));
        tree.as_object_mut().unwrap().remove("tier");
        let parsed: super::EngineSnapshot = serde_json::from_value(tree).unwrap();
        assert_eq!(parsed.tier, None, "an absent key is unknown");
    }

    /// `restore_runtime_state` writes the folded account, the three dedup sets and the gate's
    /// window onto a mount-built engine, and leaves its limits, route key, trading state and
    /// configuration alone.
    #[test]
    fn restore_runtime_state_keeps_limits_client_and_route_key() {
        use crate::account::PositionEntry;
        let mut journaled = engine_with_state();
        journaled.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), PositionSide::Both),
            PositionEntry { size: 3.0, avg_px: 99.0, ..Default::default() },
        );
        journaled.account.realized_pnl = 12.5;
        journaled.account.fees_paid = 0.25;
        journaled.seed_seen_trade_ids(["t-1".to_string()]);
        journaled.trading_state = vike_model::TradingState::Reducing;
        journaled.gate.set_throttle_times(vec![900, 950]);
        let snap = journaled.snapshot_state();

        let limits = RiskLimits { max_notional_per_order: Some(5.0), ..RiskLimits::default() };
        let mut live = ExecutionEngine::new(
            Account::new(1.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(limits.clone()),
            RecordingClient::default(),
            "sim",
            "BTCUSDT",
        );
        live.route_key = "sim#ALT".into();
        live.equity_seed = 777.0;
        live.restore_runtime_state(&snap);

        assert_eq!(
            live.position_size(vike_model::events::PositionSide::Both),
            3.0,
            "the position came back"
        );
        assert_eq!(live.account.realized_pnl, 12.5);
        assert_eq!(live.account.fees_paid, 0.25);
        assert!(live.seen_trade_ids().any(|t| t == "t-1"), "with its dedup");
        assert_eq!(live.gate.throttle_times(), vec![900, 950], "the rate window came back");
        assert_eq!(live.gate.limits, limits, "the settings' limits stay");
        assert_eq!(live.route_key, "sim#ALT", "the engine keeps its own route key");
        assert_eq!(live.trading_state, vike_model::TradingState::Active, "the caller decides it");
        assert_eq!(live.equity_seed, 777.0, "configuration stays");
        assert!(live.registry.is_empty(), "no journaled order enters the registry at boot");
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

    /// state_hash BYTE-IDENTITY for an all-CROSS account: a default (Cross/None) position
    /// serializes with NEITHER margin key, so the determinism fence is unchanged on the default
    /// path.
    #[test]
    fn cross_position_adds_no_margin_keys_to_the_hash_surface() {
        use crate::account::PositionEntry;
        let mut e = engine_with_state();
        e.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), PositionSide::Both),
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

    /// THE wire-identity pin for the interned fold keys: `AccountSnapshot`'s `(Ustr, Ustr,
    /// PositionSide)` / `(Ustr, Ustr)` / `Ustr` keys must render as the `String` keys did, because `state_hash` is FNV over these canonical JSON bytes and is compared ACROSS
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
            ("sim".into(), "BTCUSDT".into(), PositionSide::Long),
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
    /// Every live producer hands the key a typed `PositionSide` (`account.rs`'s module doc), so
    /// this pins only the string door a label can still take (`PositionSide::from_word_or_both`): an
    /// unknown word folds to `Both`, never a fourth key. A venue word is mapped by its bridge,
    /// never through here.
    #[test]
    fn an_unrecognized_side_label_folds_to_both_not_a_fourth_key() {
        let key: crate::account::PositionKey =
            ("sim".into(), "BTCUSDT".into(), PositionSide::from_word_or_both("long"));
        assert_eq!(key.2, PositionSide::Both);
        for (label, want) in [
            ("BOTH", PositionSide::Both),
            ("LONG", PositionSide::Long),
            ("SHORT", PositionSide::Short),
        ] {
            assert_eq!(PositionSide::from_word_or_both(label), want, "{label}");
            assert_eq!(want.to_string(), label, "{label} must render back unchanged");
        }
    }

    /// An ISOLATED position survives the full `EngineSnapshot` serde round-trip carrying its mode
    /// AND its allocated wallet. (Also exercises the `#[serde(default)]` compat: the surrounding cross positions have no
    /// keys, the isolated one does, and both deserialize.)
    #[test]
    fn isolated_position_round_trips_through_engine_snapshot() {
        use crate::account::PositionEntry;
        use vike_model::MarginMode;
        let mut e = engine_with_state();
        e.account.positions.insert(
            ("sim".into(), "ETHUSDT".into(), PositionSide::Both),
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
            ("sim".into(), "BTCUSDT".into(), PositionSide::Both),
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
        let pos = restored.position_size(vike_model::events::PositionSide::Both);
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
