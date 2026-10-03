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
    let (events, _rx) = vike_exec::lanes::event_channel(8);
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
    let (events, _rx) = vike_exec::lanes::event_channel(8);
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

// --- the on-chain oracle seam ----------------------------------------------------------------

/// The live four-position wallet: `(conditionId, token, size, heldOutcomeIndex)` — every row
/// `redeemable: true` (2026-07-23), three of them plain losers, each condition holding exactly
/// ONE leg so [`ambiguous_conditions`] can never fire on it.
fn live_wallet() -> Vec<Position> {
    vec![
        pos_idx("0x13bf", "tokSol", 11.11, true, false, 1), // numerators [1,0] ⇒ LOSER
        pos_idx("0xbf33", "tokDoge1", 10.0, true, false, 0), // [0,1] ⇒ LOSER
        pos_idx("0x5a71", "tokDoge2", 8.0, true, false, 0), // [0,1] ⇒ LOSER
        pos_idx("0xf361", "tokBnb", 5.0, true, false, 1),   // [0,1] ⇒ WINNER
    ]
}

/// The chain's verdict on each of those four conditions.
fn with_live_wallet_chain(deps: StubDeps) -> StubDeps {
    deps.with_chain("0x13bf", vec![1, 0], 1)
        .with_chain("0xbf33", vec![0, 1], 1)
        .with_chain("0x5a71", vec![0, 1], 1)
        .with_chain("0xf361", vec![0, 1], 1)
}

/// **The live-refuted case.** Each condition holds ONE leg, so the ambiguity guard cannot fire,
/// and under the `redeemable`-flag source every one settles at 1.0: a fabricated profit. This
/// test pins both halves of that: the flag-only fold is wrong, and the chain fold is right.
#[test]
fn chain_payouts_override_the_redeemable_flag_on_the_live_four_position_shape() {
    // (a) The `RedeemableFlag` opt-out: every leg settles at 1.0 — the fabricated profit, kept
    // pinned so choosing that source is choosing a known-wrong answer, not a surprise.
    {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        let deps = StubDeps::new(vec![live_wallet()]);
        assert_eq!(deps.payout_source(), PayoutSource::RedeemableFlag);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
        assert_eq!(rep.settled.len(), 4);
        assert!(rep.chain_priced.is_empty(), "no oracle ⇒ nothing chain-priced");
        for f in fills(&out) {
            assert_eq!(f.last_px.to_bits(), 1.0f64.to_bits(), "{} settled at 1.0", f.symbol);
        }
    }

    // (b) WITH the chain: three losers at 0.0, the BNB winner at 1.0.
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let deps = with_live_wallet_chain(StubDeps::new(vec![live_wallet()]));
    assert_eq!(deps.payout_source(), PayoutSource::Chain);
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    assert_eq!(rep.settled.len(), 4);
    assert_eq!(rep.chain_priced.len(), 4, "every payout came from the chain");
    let px: BTreeMap<&str, u64> =
        fills(&out).iter().map(|f| (f.symbol.as_str(), f.last_px.to_bits())).collect();
    assert_eq!(px["tokSol"], 0.0f64.to_bits());
    assert_eq!(px["tokDoge1"], 0.0f64.to_bits());
    assert_eq!(px["tokDoge2"], 0.0f64.to_bits());
    assert_eq!(px["tokBnb"], 1.0f64.to_bits(), "the only real winner");
}

// --- the DEFAULT source is the chain, and it fails closed ------------------------------------

/// **The regression guard for the poller's DEFAULT wiring.** `ResolvePoller::spawn` runs
/// [`ChainResolveDeps`], so a pass over the live wallet with the chain UNREACHABLE (the oracle
/// answers `None` for every condition, exactly as an RPC blackout looks at this seam) must book
/// NOTHING — and in particular must not book any of the four at 1.0 off the `redeemable` flag.
/// Everything stays watched and unmarked, so the next pass retries.
#[test]
fn chain_source_books_nothing_when_the_chain_cannot_answer() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    // `Chain` source with an EMPTY verdict map = the RPC answered nothing this pass.
    let deps = StubDeps {
        snapshots: Mutex::new(vec![live_wallet()]),
        local: None,
        chain: BTreeMap::new(),
        source: Some(PayoutSource::Chain),
    };
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

    assert!(out.is_empty(), "a silent chain must emit NO settlement fill at all");
    assert_eq!(rep, SettleTickReport::default(), "nothing settled, skipped, failed or pruned");
    assert_eq!(wl.len(), 4, "all four stay watched");
    for (cid, tok) in
        [("0x13bf", "tokSol"), ("0xbf33", "tokDoge1"), ("0x5a71", "tokDoge2"), ("0xf361", "tokBnb")]
    {
        assert!(!ledger.contains(cid, tok), "{tok} must not be marked — the next pass retries");
    }

    // Next pass, the RPC is back: the same wallet settles at its TRUE payouts, three at 0.0.
    let deps = with_live_wallet_chain(StubDeps::new(vec![live_wallet()]));
    let mut out2 = Vec::new();
    let rep2 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out2));
    assert_eq!(rep2.settled.len(), 4, "fail closed is a delay, never a forfeit");
    let px: BTreeMap<&str, u64> =
        fills(&out2).iter().map(|f| (f.symbol.as_str(), f.last_px.to_bits())).collect();
    assert_eq!(px["tokBnb"], 1.0f64.to_bits(), "only the real winner pays");
    for loser in ["tokSol", "tokDoge1", "tokDoge2"] {
        assert_eq!(px[loser], 0.0f64.to_bits(), "{loser} settles at 0.0, not 1.0");
    }
}

/// The `redeemable` flag cannot make ANYTHING due under [`PayoutSource::Chain`] — not even a
/// condition the chain has never heard of. The partial case: two of four priced, two silent ⇒
/// exactly two settle, and the two the chain could not price are untouched.
#[test]
fn chain_source_settles_only_the_legs_the_chain_priced() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let deps = StubDeps::new(vec![live_wallet()])
        .with_chain("0x13bf", vec![1, 0], 1) // LOSER, priced
        .with_chain("0xf361", vec![0, 1], 1); // WINNER, priced
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

    assert_eq!(rep.settled, vec!["tokBnb".to_string(), "tokSol".to_string()]);
    assert_eq!(rep.chain_priced.len(), 2);
    let px: BTreeMap<&str, u64> =
        fills(&out).iter().map(|f| (f.symbol.as_str(), f.last_px.to_bits())).collect();
    assert_eq!(px["tokSol"], 0.0f64.to_bits());
    assert_eq!(px["tokBnb"], 1.0f64.to_bits());
    assert_eq!(wl.len(), 2, "the two unpriced legs stay watched");
    assert!(!ledger.contains("0xbf33", "tokDoge1") && !ledger.contains("0x5a71", "tokDoge2"));
}

/// A chain verdict that says RESOLVED but has no slot for our `outcome_index` is anomalous data,
/// not a licence to fall back to the flag: the leg is reported `skipped_unpriced` and left
/// watched. (The same fixture under the flag source would settle it at 1.0.)
#[test]
fn chain_source_skips_a_resolved_condition_it_cannot_price_this_leg_of() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    // Held leg is slot 3; the verdict only has slots 0 and 1.
    let deps = StubDeps::new(vec![vec![pos_idx("0xA", "tokOdd", 9.0, true, false, 3)]]).with_chain(
        "0xA",
        vec![1, 0],
        1,
    );
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

    assert!(out.is_empty(), "no fill for a leg the authority cannot price");
    assert_eq!(rep.skipped_unpriced, vec!["tokOdd".to_string()]);
    assert!(rep.settled.is_empty() && rep.chain_priced.is_empty());
    assert_eq!(wl.len(), 1, "still watched");
    assert!(!ledger.contains("0xA", "tokOdd"), "not marked — a later verdict can still settle");

    // The contrast, and the ONLY shape that still settles this fixture at 1.0: the flag opt-out
    // with no oracle at all ([`ProdResolveDeps`]'s shape), where nothing has spoken for the
    // condition but the flag. A flag-source deps that DOES answer `chain_resolution` refuses
    // exactly like the arm above — `a_chain_verdict_refuses_under_the_flag_source_too`.
    let flag_deps = StubDeps::new(vec![vec![pos_idx("0xA", "tokOdd", 9.0, true, false, 3)]]);
    assert_eq!(flag_deps.payout_source(), PayoutSource::RedeemableFlag);
    let dir2 = tempfile::tempdir().unwrap();
    let ledger2 = SettlementLedger::open(dir2.path().join("s.txt"));
    let mut wl2 = ResolveWatchlist::new();
    let mut out2 = Vec::new();
    settle_once(&flag_deps, "0xproxy", &mut wl2, &ledger2, &mut sink(&mut out2));
    assert_eq!(fills(&out2)[0].last_px.to_bits(), 1.0f64.to_bits());
}

/// **The refusal is unconditional in the SOURCE** — the regression this test exists for.
///
/// #974 placed "a chain verdict exists ⇒ never fall back to the heuristic" BEFORE any source
/// branch. #975 restructured into `match (chain_payout, source)`, which narrowed it onto the
/// [`PayoutSource::Chain`] arm alone — so a deps that names [`PayoutSource::RedeemableFlag`]
/// AND overrides [`ResolveDeps::chain_resolution`] fell through to `payout_for`, and a held
/// LOSING leg whose row reports `redeemable: true` (the module's governing fact) settled at
/// **1.0**, fabricating realized PnL. Nothing in-tree pairs those two — `ResolvePoller::spawn`
/// wires [`ChainResolveDeps`] and [`ProdResolveDeps`] has no oracle — but both the trait and
/// the enum are `pub` at the crate root, so the pairing is constructible by any caller.
///
/// Both unpriceable inputs under that pairing, plus the positive control that proves the leg is
/// priced by the VERDICT and not by the flag when the slot IS readable.
#[test]
fn a_chain_verdict_refuses_under_the_flag_source_too() {
    let mut unreadable = pos_idx("0xA", "tok", 10.0, true, false, 1);
    unreadable.outcome_index = None; // the data-api omitted or garbled our slot
    let out_of_range = pos_idx("0xA", "tok", 10.0, true, false, 3); // no slot 3 in `[1, 0]`

    for (label, held) in [("unreadable", unreadable), ("out-of-range", out_of_range)] {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        // ⚠ The dangerous pairing: `RedeemableFlag` declared, oracle answering anyway.
        let deps = StubDeps::new(vec![vec![held]]).with_chain("0xA", vec![1, 0], 1).flag_source();
        assert_eq!(deps.payout_source(), PayoutSource::RedeemableFlag, "{label}");
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

        assert!(out.is_empty(), "{label}: a verdict exists ⇒ NOTHING may settle off the flag");
        assert_eq!(rep.skipped_unpriced, vec!["tok".to_string()], "{label}: refused, reported");
        assert!(rep.skipped_ambiguous.is_empty(), "{label}: not the flag's ambiguity guard");
        assert!(rep.settled.is_empty() && rep.chain_priced.is_empty(), "{label}: nothing booked");
        assert!(!ledger.contains("0xA", "tok"), "{label}: unmarked, so a later pass retries");
        assert_eq!(wl.len(), 1, "{label}: still watched");
    }

    // The control: same pairing, same `[1, 0]` verdict, but our slot IS readable — the leg is
    // priced by the CHAIN at 0.0, not by the `redeemable: true` flag at 1.0.
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let deps = StubDeps::new(vec![vec![pos_idx("0xA", "tokLose", 10.0, true, false, 1)]])
        .with_chain("0xA", vec![1, 0], 1)
        .flag_source();
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    assert_eq!(rep.settled, vec!["tokLose".to_string()]);
    assert_eq!(rep.chain_priced, vec!["tokLose".to_string()], "the verdict priced it");
    assert_eq!(fills(&out)[0].last_px.to_bits(), 0.0f64.to_bits(), "0.0 from the chain, not 1.0");
}

/// The deps `ResolvePoller::spawn` actually builds speak for the CHAIN — the asymmetry this
/// module used to have with `auto_redeem` (which defaults to the chain) pinned shut. Building
/// them opens no socket.
#[test]
fn the_default_poller_deps_are_chain_sourced() {
    let chain = ChainRpcSettings::default();
    assert_eq!(ResolvePoller::default_deps(&chain).payout_source(), PayoutSource::Chain);
    assert_eq!(ChainResolveDeps::from_settings(&chain).payout_source(), PayoutSource::Chain);
    // And the flag-only deps still names itself honestly.
    assert_eq!(ProdResolveDeps.payout_source(), PayoutSource::RedeemableFlag);
}

/// The loser-only wallet — unsettleable from `/positions` alone (no redeemable row anywhere) —
/// settles at 0.0 once the chain confirms the condition resolved against it. The complement of
/// `loser_only_wallet_is_not_settled_on_a_guess`: with proof it is no longer a guess.
#[test]
fn chain_settles_a_loser_only_wallet_that_no_redeemable_row_can_reach() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    // Held leg is index 1; the chain says index 0 won ⇒ this position is worthless.
    let deps = StubDeps::new(vec![vec![pos_idx("0xA", "tokLose", 40.0, false, false, 1)]])
        .with_chain("0xA", vec![1, 0], 1);
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    assert_eq!(rep.settled, vec!["tokLose".to_string()]);
    assert_eq!(rep.chain_priced, vec!["tokLose".to_string()]);
    let fs = fills(&out);
    assert_eq!(fs.len(), 1);
    assert_eq!(fs[0].last_px.to_bits(), 0.0f64.to_bits());
    assert_eq!(fs[0].last_qty, 40.0);
    assert!(wl.is_empty());
}

/// An UNRESOLVED condition is still left alone with a chain oracle wired: the oracle's
/// `denominator == 0` verdict is filtered out before it can make anything due.
#[test]
fn chain_oracle_does_not_settle_an_unresolved_condition() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let deps = StubDeps::new(vec![vec![pos("0xB", "tokOpen", 7.0, false, false)]]).with_chain(
        "0xB",
        vec![],
        0,
    );
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    assert!(rep.settled.is_empty() && out.is_empty());
    assert_eq!(wl.len(), 1, "still watched");
}

/// The ambiguity SKIP is bypassed when the chain can price the leg — ambiguity is only
/// unresolvable without chain data.
#[test]
fn chain_resolves_what_the_ambiguity_guard_can_only_skip() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let deps = StubDeps::new(vec![vec![
        pos_idx("0xA", "tokWin", 100.0, true, false, 0),
        pos_idx("0xA", "tokLose", 40.0, true, false, 1), // both flagged ⇒ ambiguous
    ]])
    .with_chain("0xA", vec![1, 0], 1);
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    assert!(rep.skipped_ambiguous.is_empty(), "the chain answers what the guard could not");
    assert_eq!(rep.settled.len(), 2);
    let px: BTreeMap<&str, u64> =
        fills(&out).iter().map(|f| (f.symbol.as_str(), f.last_px.to_bits())).collect();
    assert_eq!(px["tokWin"], 1.0f64.to_bits());
    assert_eq!(px["tokLose"], 0.0f64.to_bits());
}

/// A SPLIT resolution pays BOTH legs 0.5 — a payout the boolean `redeemable` flag cannot even
/// express, so this case is only reachable through the chain.
#[test]
fn chain_settles_a_split_resolution_at_half() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let deps = StubDeps::new(vec![vec![pos_idx("0xS", "tokA", 10.0, false, false, 0)]]).with_chain(
        "0xS",
        vec![1, 1],
        2,
    );
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    let fs = fills(&out);
    assert_eq!(fs.len(), 1);
    assert_eq!(fs[0].last_px.to_bits(), 0.5f64.to_bits());
}

/// The at-most-once ledger still governs the chain path, and the chain path still respects the
/// local-book override — the oracle changes the PAYOUT, never the bookkeeping.
#[test]
fn chain_path_keeps_the_ledger_and_local_override_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    let local: BTreeMap<String, f64> = [("tokWin".to_string(), 3.0)].into_iter().collect();
    let deps =
        StubDeps::with_local(vec![vec![pos_idx("0xA", "tokWin", 100.0, false, false, 1)]], local)
            .with_chain("0xA", vec![0, 1], 1);
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let r1 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));
    assert_eq!(r1.settled, vec!["tokWin".to_string()]);
    assert_eq!(fills(&out)[0].last_qty, 3.0, "closes the LOCAL size");
    assert_eq!(fills(&out)[0].last_px.to_bits(), 1.0f64.to_bits());

    let mut out2 = Vec::new();
    let r2 = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out2));
    assert!(r2.settled.is_empty() && out2.is_empty(), "not settled twice");
}

/// `outcome_index` rides from the data-api row onto the watch entry — without it the chain
/// verdict could not be applied to the right leg.
#[test]
fn watch_entry_carries_the_outcome_index() {
    let mut wl = ResolveWatchlist::new();
    wl.upsert_from_positions(&[pos_idx("0xA", "tokA", 1.0, false, false, 1)]);
    assert_eq!(wl.get("tokA").unwrap().outcome_index, Some(1));
    // refreshed on re-upsert, like every other field
    wl.upsert_from_positions(&[pos_idx("0xA", "tokA", 1.0, false, false, 0)]);
    assert_eq!(wl.get("tokA").unwrap().outcome_index, Some(0));
}

/// An UNREADABLE `outcomeIndex` rides across as `None` — the row is still watched (it is a held
/// position; skipping it would let `prune_flat` evict it), the slot alone is unknown.
#[test]
fn watch_entry_carries_an_unreadable_outcome_index_as_none() {
    let mut wl = ResolveWatchlist::new();
    let mut p = pos_idx("0xA", "tokA", 1.0, false, false, 1);
    p.outcome_index = None;
    wl.upsert_from_positions(&[p]);
    assert_eq!(wl.get("tokA").unwrap().outcome_index, None);
    assert_eq!(wl.len(), 1, "still watched — only the slot is unknown");
}

/// **The `crate::exec_plane::settlement::resolve` half of the `outcomeIndex` hardening.** The chain has resolved this
/// condition `[1, 0]` and we hold the LOSING leg, whose data-api row reports `redeemable: true`
/// like every row of a resolved condition. With the slot unreadable, the chain verdict cannot
/// be applied — and the `redeemable`-derived fallback would put this very token in `winners`
/// and settle it at 1.0, writing a fabricated realized PnL. It must settle NOTHING and stay
/// watched, so a later snapshot with a readable slot can price it correctly.
///
/// Reported in [`SettleTickReport::skipped_unpriced`], NOT `skipped_ambiguous`: under
/// [`PayoutSource::Chain`] an unreadable slot and an out-of-range slot are the same refusal —
/// a resolved verdict that cannot be read against this leg — and `skipped_ambiguous` is the
/// flag source's own, unrelated guard. `unreadable_and_out_of_range_slots_share_one_skip_field`
/// pins that they are one field; `chain_source_skips_a_resolved_condition_it_cannot_price_this_leg_of`
/// covers the out-of-range twin.
#[test]
fn a_chain_resolved_condition_with_an_unreadable_slot_settles_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("s.txt"));
    // The held LOSING leg: slot 1 of a `[1, 0]` resolution, flagged `redeemable: true` like
    // every row of a resolved condition — but the page did not give us a readable slot.
    let mut held = pos_idx("0xA", "tokLose", 10.0, true, false, 1);
    held.outcome_index = None;
    let deps = StubDeps::new(vec![vec![held]]).with_chain("0xA", vec![1, 0], 1);
    assert_eq!(deps.payout_source(), PayoutSource::Chain, "the production source");
    let mut wl = ResolveWatchlist::new();
    let mut out = Vec::new();
    let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

    assert!(out.is_empty(), "no settlement fill — a guessed payout is a fabricated PnL");
    assert!(rep.settled.is_empty() && rep.chain_priced.is_empty());
    assert_eq!(rep.skipped_unpriced, vec!["tokLose".to_string()], "refused, and reported");
    assert!(rep.skipped_ambiguous.is_empty(), "the flag guard is not what refused here");
    assert!(!ledger.contains("0xA", "tokLose"), "not tombstoned — a later page can price it");
    assert_eq!(wl.len(), 1, "still watched");

    // The control: the SAME snapshot with the slot readable settles the loser at 0.0, proving
    // the refusal above was about the unreadable slot and nothing else.
    let deps2 = StubDeps::new(vec![vec![pos_idx("0xA", "tokLose", 10.0, true, false, 1)]])
        .with_chain("0xA", vec![1, 0], 1);
    let mut wl2 = ResolveWatchlist::new();
    let mut out2 = Vec::new();
    let rep2 = settle_once(&deps2, "0xproxy", &mut wl2, &ledger, &mut sink(&mut out2));
    assert_eq!(rep2.settled, vec!["tokLose".to_string()]);
    assert_eq!(fills(&out2)[0].last_px.to_bits(), 0.0f64.to_bits(), "priced 0, not 1");
}

/// **The rebase witness.** Two PRs independently added a refusal for "the chain resolved this
/// condition but I cannot price this leg": an UNREADABLE slot (`outcome_index: None`) and an
/// OUT-OF-RANGE slot (`Some(3)` against a 2-slot verdict). They are ONE concern — same
/// authority, same predicate (`chain_payout.is_none()` with a verdict in hand), same
/// refusal, same recovery — so they report through ONE field. This pins that: both inputs land
/// in `skipped_unpriced`, neither in `skipped_ambiguous`, and both leave the leg watched and
/// unmarked. The operator tells them apart from the WARN log's `outcome_index`, which is where
/// the distinction is actually actionable (`None` ⇒ refetch the degraded page; `Some(i)` ⇒
/// anomalous chain data).
#[test]
fn unreadable_and_out_of_range_slots_share_one_skip_field() {
    let mut unreadable = pos_idx("0xA", "tok", 10.0, true, false, 1);
    unreadable.outcome_index = None;
    let out_of_range = pos_idx("0xA", "tok", 10.0, true, false, 3);

    for (label, held) in [("unreadable", unreadable), ("out-of-range", out_of_range)] {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SettlementLedger::open(dir.path().join("s.txt"));
        // The SAME `[1, 0]` verdict in both halves — only the leg's own slot differs.
        let deps = StubDeps::new(vec![vec![held]]).with_chain("0xA", vec![1, 0], 1);
        let mut wl = ResolveWatchlist::new();
        let mut out = Vec::new();
        let rep = settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut sink(&mut out));

        assert!(out.is_empty(), "{label}: nothing emitted");
        assert_eq!(rep.skipped_unpriced, vec!["tok".to_string()], "{label}: one field");
        assert!(rep.skipped_ambiguous.is_empty(), "{label}: not the flag guard");
        assert!(rep.settled.is_empty(), "{label}: nothing settled");
        assert!(!ledger.contains("0xA", "tok"), "{label}: unmarked, so a later pass retries");
        assert_eq!(wl.len(), 1, "{label}: still watched");
    }
}
