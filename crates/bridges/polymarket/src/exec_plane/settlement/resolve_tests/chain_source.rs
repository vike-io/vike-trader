//! The on-chain oracle seam: chain-sourced payouts and their fail-closed refusals.

use super::*;

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
