use super::*;

/// Scripted stub: canned snapshots (one per tick, last repeats) + an optional local-book map.
///
/// Its [`PayoutSource`] follows its wiring, so every test below reads as what it is: a stub with
/// scripted chain verdicts speaks for [`PayoutSource::Chain`] (the production shape), and one
/// without speaks for the [`PayoutSource::RedeemableFlag`] opt-out — which is what the pre-chain
/// tests were always exercising. `flag_source()` forces the latter even WITH verdicts scripted,
/// which is the ONLY way to build the source/oracle pairing nothing in-tree wires — see
/// `a_chain_verdict_refuses_under_the_flag_source_too`.
struct StubDeps {
    snapshots: Mutex<Vec<Vec<Position>>>,
    local: Option<BTreeMap<String, f64>>,
    /// Scripted chain oracle: conditionId → verdict. Empty ⇒ `chain_resolution` answers `None`
    /// for everything (an oracle that cannot see, or a deps that has none).
    chain: BTreeMap<String, ChainResolution>,
    /// `None` ⇒ derive from `chain`; `Some(s)` ⇒ speak for `s` regardless.
    source: Option<PayoutSource>,
}
impl StubDeps {
    fn new(snapshots: Vec<Vec<Position>>) -> Self {
        StubDeps {
            snapshots: Mutex::new(snapshots),
            local: None,
            chain: BTreeMap::new(),
            source: None,
        }
    }
    fn with_local(snapshots: Vec<Vec<Position>>, local: BTreeMap<String, f64>) -> Self {
        StubDeps {
            snapshots: Mutex::new(snapshots),
            local: Some(local),
            chain: BTreeMap::new(),
            source: None,
        }
    }
    /// Attach a chain verdict for one condition: `numerators` over denominator 1.
    fn with_chain(mut self, cid: &str, numerators: Vec<u128>, denominator: u128) -> Self {
        self.chain.insert(
            cid.to_string(),
            ChainResolution { condition_id: cid.into(), denominator, numerators },
        );
        self
    }
    /// Speak for the `redeemable`-flag opt-out even when chain verdicts are scripted.
    fn flag_source(mut self) -> Self {
        self.source = Some(PayoutSource::RedeemableFlag);
        self
    }
}
impl ResolveDeps for StubDeps {
    fn list_positions(&self, _proxy: &str) -> Result<Vec<Position>, String> {
        let mut s = self.snapshots.lock().unwrap();
        if s.len() > 1 { Ok(s.remove(0)) } else { Ok(s.first().cloned().unwrap_or_default()) }
    }
    fn payout_source(&self) -> PayoutSource {
        self.source.unwrap_or(if self.chain.is_empty() {
            PayoutSource::RedeemableFlag
        } else {
            PayoutSource::Chain
        })
    }
    fn local_position(&self, token_id: &str) -> Option<f64> {
        self.local.as_ref().map(|m| m.get(token_id).copied().unwrap_or(0.0))
    }
    fn chain_resolution(&self, condition_id: &str) -> Option<ChainResolution> {
        self.chain.get(condition_id).cloned()
    }
}

fn pos(cid: &str, asset: &str, size: f64, redeemable: bool, neg: bool) -> Position {
    pos_idx(cid, asset, size, redeemable, neg, 0)
}

fn pos_idx(
    cid: &str,
    asset: &str,
    size: f64,
    redeemable: bool,
    neg: bool,
    outcome_index: u32,
) -> Position {
    Position {
        condition_id: cid.into(),
        asset: asset.into(),
        size,
        redeemable,
        neg_risk: neg,
        outcome_index: Some(outcome_index),
        title: "t".into(),
        cur_price: None,
    }
}

/// Collecting sink: records every emitted event, always accepting.
fn sink(out: &mut Vec<Event>) -> impl FnMut(Event) -> bool + '_ {
    move |e| {
        out.push(e);
        true
    }
}

fn fills(events: &[Event]) -> Vec<&FillEvent> {
    events
        .iter()
        .map(|e| match e {
            Event::Fill(f) => f,
            other => panic!("expected only bare Fill events, got {other:?}"),
        })
        .collect()
}

// --- fixture parsing: resolved / unresolved -------------------------------------------------

/// A snapshot with a redeemable row proves its condition resolved and names the winning token;
/// a snapshot with none proves nothing.
#[test]
fn resolution_status_from_positions_fixture() {
    let unresolved =
        vec![pos("0xA", "tokA1", 10.0, false, false), pos("0xA", "tokA2", 5.0, false, false)];
    assert!(resolved_conditions(&unresolved).is_empty(), "no redeemable row ⇒ not resolved");
    assert!(winning_tokens(&unresolved).is_empty());

    let resolved_snap =
        vec![pos("0xA", "tokA1", 10.0, true, false), pos("0xA", "tokA2", 5.0, false, false)];
    let rc = resolved_conditions(&resolved_snap);
    assert_eq!(rc.len(), 1);
    assert!(rc.contains("0xA"));
    let w = winning_tokens(&resolved_snap);
    assert!(w.contains("tokA1") && !w.contains("tokA2"));
}

/// The real fixture shape: `parse_positions` output feeds the resolution derivation unchanged
/// (the JSON field pinning lives in positions.rs; this proves the seam holds end-to-end).
#[test]
fn parses_data_api_fixture_into_resolution_status() {
    let json = serde_json::json!([
        {"conditionId":"0xA","asset":"tokWin","size":100.0,"redeemable":true,"negativeRisk":false,"outcomeIndex":0,"title":"won"},
        {"conditionId":"0xA","asset":"tokLose","size":40.0,"redeemable":false,"negativeRisk":false,"outcomeIndex":1,"title":"lost"},
        {"conditionId":"0xB","asset":"tokOpen","size":7.0,"redeemable":false,"negativeRisk":true,"outcomeIndex":0,"title":"still trading"}
    ]);
    let ps = crate::exec_plane::settlement::positions::parse_positions(&json);
    let rc = resolved_conditions(&ps);
    assert!(rc.contains("0xA"), "0xA has a redeemable leg ⇒ resolved");
    assert!(!rc.contains("0xB"), "0xB has none ⇒ left alone");
    assert_eq!(winning_tokens(&ps).iter().cloned().collect::<Vec<_>>(), vec!["tokWin"]);
}

#[test]
fn payout_is_one_for_winner_zero_for_loser() {
    let winners: BTreeSet<String> = ["tokWin".to_string()].into_iter().collect();
    let win = WatchEntry {
        token_id: "tokWin".into(),
        condition_id: "0xA".into(),
        neg_risk: false,
        qty: 10.0,
        outcome_index: Some(0),
    };
    let lose = WatchEntry { token_id: "tokLose".into(), ..win.clone() };
    assert_eq!(payout_for(&win, &winners).to_bits(), WINNER_PAYOUT.to_bits());
    assert_eq!(payout_for(&lose, &winners).to_bits(), LOSER_PAYOUT.to_bits());
}

// --- the ambiguity guard --------------------------------------------------------------------

/// Only a condition with 2+ DISTINCT redeemable tokens is ambiguous. One redeemable leg (the
/// assumed-normal shape) is not, and the same token appearing twice is not either.
#[test]
fn ambiguous_conditions_needs_two_distinct_redeemable_tokens() {
    // normal: one winner + one loser under 0xA ⇒ unambiguous.
    let normal =
        vec![pos("0xA", "tokWin", 100.0, true, false), pos("0xA", "tokLose", 40.0, false, false)];
    assert!(ambiguous_conditions(&normal).is_empty());

    // contradiction: BOTH legs of 0xA flagged redeemable ⇒ ambiguous.
    let both =
        vec![pos("0xA", "tokWin", 100.0, true, false), pos("0xA", "tokLose", 40.0, true, false)];
    let amb = ambiguous_conditions(&both);
    assert_eq!(amb.len(), 1);
    assert!(amb.contains("0xA"));

    // a duplicate row for the SAME token is not a second leg.
    let dup =
        vec![pos("0xA", "tokWin", 100.0, true, false), pos("0xA", "tokWin", 100.0, true, false)];
    assert!(ambiguous_conditions(&dup).is_empty(), "same token twice is not ambiguous");

    // distinct conditions each with their own single winner ⇒ neither is ambiguous.
    let two_markets =
        vec![pos("0xA", "tokA", 1.0, true, false), pos("0xB", "tokB", 1.0, true, false)];
    assert!(ambiguous_conditions(&two_markets).is_empty());
}

/// The money-protecting behavior: when both legs are flagged redeemable, NOTHING settles — no
/// fill is emitted, nothing is ledger-marked, and both legs stay watched. This is the guard
/// against booking a fabricated 1.0 payout on a losing leg.
#[test]
fn ambiguous_condition_settles_nothing_and_stays_watched() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let deps = StubDeps::new(vec![vec![
        pos("0xA", "tokWin", 100.0, true, false),
        pos("0xA", "tokLose", 40.0, true, false), // contradicts the winner rule
    ]]);
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

    assert!(out.is_empty(), "an ambiguous condition must emit NO settlement fill");
    assert!(rep.settled.is_empty() && rep.failed.is_empty());
    assert_eq!(
        rep.skipped_ambiguous,
        vec!["tokLose".to_string(), "tokWin".to_string()],
        "both legs reported skipped (token-id ordered)"
    );
    assert_eq!(wl.len(), 2, "both legs stay watched — status quo, not a guess");
    assert!(!ledger.contains("0xA", "tokWin"), "nothing marked — a later fix can still settle");
    assert!(!ledger.contains("0xA", "tokLose"));
}

/// The guard is scoped per condition: an ambiguous market does not block an unrelated,
/// unambiguous one in the same snapshot.
#[test]
fn ambiguity_does_not_block_other_conditions() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let deps = StubDeps::new(vec![vec![
        pos("0xA", "tokWin", 100.0, true, false),
        pos("0xA", "tokLose", 40.0, true, false), // ambiguous condition
        pos("0xB", "tokClean", 7.0, true, false), // clean single-winner condition
    ]]);
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

    assert_eq!(rep.settled, vec!["tokClean".to_string()], "the clean condition still settles");
    assert_eq!(rep.skipped_ambiguous.len(), 2);
    let fs = fills(&out);
    assert_eq!(fs.len(), 1);
    assert_eq!(fs[0].symbol, "tokClean");
    assert_eq!(fs[0].last_px.to_bits(), 1.0f64.to_bits());
}

// --- watchlist upsert / prune ---------------------------------------------------------------

#[test]
fn watchlist_upserts_and_refreshes_qty() {
    let mut wl = ResolveWatchlist::new();
    assert!(wl.is_empty());
    let n = wl.upsert_from_positions(&[
        pos("0xA", "tokA", 10.0, false, false),
        pos("0xB", "tokB", 3.0, false, true),
    ]);
    assert_eq!(n, 2);
    assert_eq!(wl.len(), 2);
    assert_eq!(wl.get("tokA").unwrap().qty, 10.0);
    assert_eq!(wl.get("tokA").unwrap().condition_id, "0xA");
    assert!(wl.get("tokB").unwrap().neg_risk, "neg-risk flag carried onto the entry");

    // second snapshot: tokA grew — the SAME entry is refreshed, not duplicated.
    wl.upsert_from_positions(&[pos("0xA", "tokA", 25.0, false, false)]);
    assert_eq!(wl.len(), 2, "upsert, not insert");
    assert_eq!(wl.get("tokA").unwrap().qty, 25.0);
}

#[test]
fn watchlist_ignores_zero_size_rows() {
    let mut wl = ResolveWatchlist::new();
    assert_eq!(wl.upsert_from_positions(&[pos("0xA", "tokA", 0.0, true, false)]), 0);
    assert!(wl.is_empty(), "a flat row is never watched");
}

#[test]
fn watchlist_prunes_on_flat() {
    let mut wl = ResolveWatchlist::new();
    wl.upsert_from_positions(&[
        pos("0xA", "tokA", 10.0, false, false),
        pos("0xB", "tokB", 3.0, false, false),
    ]);
    // tokB gone from the snapshot (sold/transferred) → pruned; tokA stays.
    let pruned = wl.prune_flat(&[pos("0xA", "tokA", 10.0, false, false)]);
    assert_eq!(pruned, vec!["tokB".to_string()]);
    assert_eq!(wl.len(), 1);
    assert!(wl.get("tokA").is_some());

    // a size-0 row counts as flat too
    let pruned2 = wl.prune_flat(&[pos("0xA", "tokA", 0.0, false, false)]);
    assert_eq!(pruned2, vec!["tokA".to_string()]);
    assert!(wl.is_empty());
}

// --- settlement emission via a scripted sink ------------------------------------------------

/// The core case: a resolved condition where the wallet holds BOTH legs settles TWO local
/// positions — the winner at 1.0 and the loser at 0.0 — each as ONE bare closing Fill.
#[test]
fn settles_both_legs_at_their_payouts() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("settled.txt"));
    let deps = StubDeps::new(vec![vec![
        pos("0xA", "tokWin", 100.0, true, false),
        pos("0xA", "tokLose", 40.0, false, false),
    ]]);
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

    assert_eq!(rep.settled.len(), 2, "both legs settled");
    assert!(rep.failed.is_empty());
    let fs = fills(&out);
    assert_eq!(fs.len(), 2, "exactly one bare Fill per settled position — no FSM wraps");

    // BTreeMap ordering: "tokLose" < "tokWin"
    let lose = fs[0];
    assert_eq!(lose.symbol, "tokLose");
    assert_eq!(lose.last_px.to_bits(), 0.0f64.to_bits(), "loser settles at 0.0");
    assert_eq!(lose.last_qty, 40.0);
    assert_eq!(lose.side, -1, "closes the long");
    assert_eq!(lose.trade_id, "resolution:0xA:tokLose");
    assert_eq!(lose.client_order_id, "resolution:0xA");
    assert_eq!(lose.venue, "polymarket");
    assert_eq!(lose.commission.to_bits(), 0.0f64.to_bits());
    assert!(lose.mark_price.is_none(), "a settlement never writes the price board");

    let win = fs[1];
    assert_eq!(win.symbol, "tokWin");
    assert_eq!(win.last_px.to_bits(), 1.0f64.to_bits(), "winner settles at 1.0");
    assert_eq!(win.last_qty, 100.0);
    assert_eq!(win.side, -1);
    assert_eq!(win.trade_id, "resolution:0xA:tokWin");

    assert!(wl.is_empty(), "settled entries leave the watchlist");
    assert!(ledger.contains("0xA", "tokWin") && ledger.contains("0xA", "tokLose"));
}

/// An UNRESOLVED condition is watched and left completely alone — the guard against fabricating
/// a loss on a live position.
#[test]
fn unresolved_conditions_emit_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let deps = StubDeps::new(vec![vec![pos("0xB", "tokOpen", 7.0, false, false)]]);
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

    assert!(rep.settled.is_empty() && out.is_empty(), "nothing settled, nothing emitted");
    assert_eq!(wl.len(), 1, "still watched");
    assert!(!ledger.contains("0xB", "tokOpen"));
}

/// A losing leg of a condition whose winner is NOT held stays unsettled — the documented
/// limitation, asserted so a future change to it is a deliberate one.
#[test]
fn loser_only_wallet_is_not_settled_on_a_guess() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    // Only the losing leg is held: no redeemable row anywhere ⇒ indistinguishable from open.
    let deps = StubDeps::new(vec![vec![pos("0xA", "tokLose", 40.0, false, false)]]);
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    assert!(rep.settled.is_empty() && out.is_empty());
    assert_eq!(wl.len(), 1, "kept under watch rather than settled at a guessed 0.0");
}

/// The neg-risk winner settles identically — `neg_risk` rides on the entry (it selects the redeem
/// CONTRACT in auto_redeem) but never changes the local payout, which is 1.0 for any winner.
#[test]
fn neg_risk_winner_settles_at_one() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let deps = StubDeps::new(vec![vec![pos("0xN", "tokNeg", 12.0, true, true)]]);
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    let fs = fills(&out);
    assert_eq!(fs.len(), 1);
    assert_eq!(fs[0].last_px.to_bits(), 1.0f64.to_bits());
    assert_eq!(fs[0].last_qty, 12.0);
}

/// Settlement runs BEFORE the prune, so a resolved winner still held this tick settles rather
/// than being dropped; and a token that went flat WITHOUT resolving is pruned unsettled.
#[test]
fn settles_before_pruning_and_prunes_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let mut wl = ResolveWatchlist::new();
    // tick 1: two open positions.
    let deps = StubDeps::new(vec![
        vec![pos("0xA", "tokWin", 100.0, false, false), pos("0xC", "tokSold", 5.0, false, false)],
        // tick 2: 0xA resolved (still held, pre-redeem); tokSold is gone.
        vec![pos("0xA", "tokWin", 100.0, true, false)],
    ]);
    let mut out = Vec::new();
    let r1 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    assert!(r1.settled.is_empty() && r1.pruned.is_empty());
    assert_eq!(wl.len(), 2);

    let mut out2 = Vec::new();
    let r2 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out2));
    assert_eq!(r2.settled, vec!["tokWin".to_string()], "resolved winner settled, not pruned");
    assert_eq!(r2.pruned, vec!["tokSold".to_string()], "vanished token pruned unsettled");
    assert!(wl.is_empty());
    assert_eq!(fills(&out2).len(), 1);
    assert!(!ledger.contains("0xC", "tokSold"), "a pruned-unsettled token is never marked");
}

// --- idempotency ----------------------------------------------------------------------------

/// Re-running the SAME resolved snapshot emits nothing more: the ledger is the at-most-once
/// guard even while the position is still reported (redeem has not landed yet).
#[test]
fn settle_once_is_idempotent_across_ticks() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let deps = StubDeps::new(vec![vec![pos("0xA", "tokWin", 100.0, true, false)]]);
    let mut wl = ResolveWatchlist::new();

    let mut out = Vec::new();
    let r1 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    assert_eq!(r1.settled.len(), 1);
    assert_eq!(out.len(), 1);

    // Same snapshot again — the position is still held/redeemable, so it is re-upserted into the
    // watchlist, but the ledger keeps it from settling twice.
    let mut out2 = Vec::new();
    let r2 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out2));
    assert!(r2.settled.is_empty(), "not settled twice");
    assert!(out2.is_empty(), "no second Fill emitted");
}

/// RESTART idempotency: a fresh process (new watchlist, new in-memory ledger) reading the SAME
/// ledger FILE re-discovers the still-held resolved position and does NOT re-settle it.
#[test]
fn restart_does_not_resettle_from_the_ledger_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settled.txt");
    let snapshot = vec![pos("0xA", "tokWin", 100.0, true, false)];

    {
        let ledger = SettlementLedger::open(path.clone());
        let deps = StubDeps::new(vec![snapshot.clone()]);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let r = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert_eq!(r.settled, vec!["tokWin".to_string()]);
    }
    // --- process restart: everything in-memory is gone, only the file survives ---
    let ledger2 = SettlementLedger::open(path);
    assert!(ledger2.contains("0xA", "tokWin"), "loaded from disk");
    let deps2 = StubDeps::new(vec![snapshot]);
    let mut wl2 = ResolveWatchlist::new();
    let mut out2 = Vec::new();
    let r2 = settle_once(&deps2, "0xproxy", &mut wl2, &ledger2, &mut sink(&mut out2));
    assert!(r2.settled.is_empty(), "restart must not re-settle");
    assert!(out2.is_empty(), "no duplicate settlement Fill after restart");
}

/// A failed emit (core lane gone) leaves the position UNMARKED and still watched, so the next
/// tick retries it — `auto_redeem`'s failure rule.
#[test]
fn failed_emit_is_not_marked_and_retries() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let deps = StubDeps::new(vec![vec![pos("0xA", "tokWin", 100.0, true, false)]]);
    let mut wl = ResolveWatchlist::new();

    let mut rejected = 0;
    let mut reject = |_e: Event| {
        rejected += 1;
        false
    };
    let r = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut reject);
    assert_eq!(r.failed, vec!["tokWin".to_string()]);
    assert!(r.settled.is_empty());
    assert_eq!(rejected, 1);
    assert!(!ledger.contains("0xA", "tokWin"), "a failed settlement must not be marked");
    assert_eq!(wl.len(), 1, "still watched — retried next tick");

    // next tick, the lane is back: it settles.
    let mut out = Vec::new();
    let r2 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    assert_eq!(r2.settled, vec!["tokWin".to_string()]);
    assert_eq!(out.len(), 1);
}

// --- local-book override --------------------------------------------------------------------

/// With a local-position hook the settlement closes the LOCAL size, not the on-chain balance —
/// the case where the wallet holds tokens this process never traded.
#[test]
fn local_position_hook_overrides_the_on_chain_qty() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let local: BTreeMap<String, f64> = [("tokWin".to_string(), 30.0)].into_iter().collect();
    // on-chain balance 100 (80 bought in the UI), locally only 30 was traded here.
    let deps = StubDeps::with_local(vec![vec![pos("0xA", "tokWin", 100.0, true, false)]], local);
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    let fs = fills(&out);
    assert_eq!(fs.len(), 1);
    assert_eq!(fs[0].last_qty, 30.0, "closes the LOCAL size, not the 100 on-chain balance");
    assert_eq!(fs[0].side, -1);
}

/// A locally-flat token emits nothing (there is no position to close) but IS marked so it stops
/// being re-examined.
#[test]
fn locally_flat_position_emits_nothing_but_is_marked() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let deps = StubDeps::with_local(
        vec![vec![pos("0xA", "tokWin", 100.0, true, false)]],
        BTreeMap::new(), // hook returns Some(0.0) for every token
    );
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let r = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    assert!(out.is_empty(), "nothing to close ⇒ no fill");
    assert!(r.settled.is_empty() && r.failed.is_empty());
    assert!(ledger.contains("0xA", "tokWin"), "marked so it is not re-examined every tick");
}

/// A local SHORT closes with a BUY — the fill is always the exact inverse of the local position.
#[test]
fn short_local_position_closes_with_a_buy() {
    let entry = WatchEntry {
        token_id: "tok".into(),
        condition_id: "0xA".into(),
        neg_risk: false,
        qty: 10.0,
        outcome_index: Some(0),
    };
    let f = settlement_fill(&entry, WINNER_PAYOUT, -10.0, 1_700_000_000_000);
    assert_eq!(f.side, 1, "closing a short is a BUY");
    assert_eq!(f.last_qty, 10.0, "qty is always positive");
    assert_eq!(f.ts, 1_700_000_000_000);
}

// --- ledger ---------------------------------------------------------------------------------

#[test]
fn ledger_marks_persist_and_are_keyed_per_condition_and_token() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settled.txt");
    let l = SettlementLedger::open(path.clone());
    assert!(!l.contains("0xA", "tokWin"));
    l.mark("0xA", "tokWin");
    assert!(l.contains("0xA", "tokWin"));
    // the OTHER leg of the SAME condition is a DIFFERENT key — the whole reason for the composite
    assert!(!l.contains("0xA", "tokLose"), "sibling leg must still be settleable");
    l.mark("0xA", "tokWin"); // idempotent
    l.mark("0xA", "tokLose");

    let l2 = SettlementLedger::open(path);
    assert!(l2.contains("0xA", "tokWin") && l2.contains("0xA", "tokLose"));
    assert!(!l2.contains("0xB", "tokWin"));
}

#[test]
fn settlement_key_and_trade_id_shapes() {
    assert_eq!(settlement_key("0xA", "tok"), "0xA:tok");
    assert_eq!(settlement_trade_id("0xA", "tok"), "resolution:0xA:tok");
}

// --- gating ---------------------------------------------------------------------------------

#[test]
fn spawn_returns_none_without_a_proxy_address() {
    // ENABLED, so the blank proxy address is the only thing that can stop it.
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = vike_exec::event_channel(8);
    let h = ResolvePoller::spawn(
        "   ".into(),
        dir.path().join("settled.txt"),
        Duration::from_secs(60),
        events,
        true,
        &ChainRpcSettings::default(),
    );
    assert!(h.is_none(), "no proxy address -> never starts");
}

/// D4: the poller starts only when its caller enables it.
#[test]
fn spawn_returns_none_unless_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = vike_exec::event_channel(8);
    let h = ResolvePoller::spawn(
        "0xproxy".into(),
        dir.path().join("settled.txt"),
        Duration::from_secs(60),
        events,
        false,
        &ChainRpcSettings::default(),
    );
    assert!(h.is_none(), "not enabled -> never starts");
}

/// A tick whose discovery fails is a no-op: nothing settled, nothing pruned, watchlist intact.
#[test]
fn discovery_failure_is_a_no_op_tick() {
    struct FailingDeps;
    impl ResolveDeps for FailingDeps {
        fn list_positions(&self, _proxy: &str) -> Result<Vec<Position>, String> {
            Err("data-api 503".into())
        }
        fn payout_source(&self) -> PayoutSource {
            PayoutSource::Chain
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let mut wl = ResolveWatchlist::new();
    wl.upsert_from_positions(&[pos("0xA", "tokA", 10.0, false, false)]);
    let mut out = Vec::new();
    let r = settle_once(&FailingDeps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    assert_eq!(r, SettleTickReport::default());
    assert!(out.is_empty());
    assert_eq!(wl.len(), 1, "a failed fetch must NOT prune the watchlist");
}

#[path = "resolve_tests/chain_source.rs"]
#[cfg(test)]
mod chain_source;
