use super::*;
use std::sync::Mutex;

/// A scripted `RedeemDeps`: no network. `confirmations` is a queue popped front-to-back; the
/// LAST entry repeats forever once reached (so a test states only the transitions it cares
/// about). `tx_hash` is what the fake relayer returns on a 2xx — `None` reproduces the shape
/// actually pinned from the reference SDK (`{"transactionID":"…","state":"NEW"}`, no hash).
struct StubDeps {
    positions: Vec<Position>,
    /// records `(condition_id, is_neg_risk)` so tests can assert ROUTING, not just the cid.
    redeemed: Mutex<Vec<(String, bool)>>,
    tx_hash: Option<String>,
    confirmations: Mutex<Vec<RedeemConfirmation>>,
    /// Scripted winner verdicts by conditionId. A cid not listed is a `Winner`, so the tests
    /// that are about the ledger/confirmation state machine stay about that and say nothing
    /// about payouts; the payout-specific tests below list their cids explicitly.
    payouts: HashMap<String, Payout>,
}

impl StubDeps {
    fn new(positions: Vec<Position>) -> Self {
        StubDeps {
            positions,
            redeemed: Mutex::new(Vec::new()),
            tx_hash: Some("0xtx".into()),
            confirmations: Mutex::new(vec![RedeemConfirmation::Unmined]),
            payouts: HashMap::new(),
        }
    }
    /// The relayer answers 2xx with NO transaction hash (the pinned `state:"NEW"` shape).
    fn without_tx_hash(mut self) -> Self {
        self.tx_hash = None;
        self
    }
    fn scripted(self, script: Vec<RedeemConfirmation>) -> Self {
        *self.confirmations.lock().unwrap() = script;
        self
    }
    /// Pin the winner verdict for specific conditionIds (everything else stays `Winner`).
    fn with_payouts(mut self, rows: &[(&str, Payout)]) -> Self {
        self.payouts = rows.iter().map(|(c, v)| ((*c).to_string(), *v)).collect();
        self
    }
    fn submits(&self) -> Vec<(String, bool)> {
        self.redeemed.lock().unwrap().clone()
    }
}

fn settled(block: u64) -> RedeemConfirmation {
    RedeemConfirmation::Settled { block, payout_usdc: 12.5 }
}
fn reverted() -> RedeemConfirmation {
    RedeemConfirmation::Reverted { reason: "no PayoutRedemption".into() }
}

impl RedeemDeps for StubDeps {
    fn list_positions(&self, _proxy: &str) -> Result<Vec<Position>, String> {
        Ok(self.positions.clone())
    }
    fn redeem(&self, _proxy: &str, cid: &str, kind: &RedeemKind) -> Result<RedeemResult, String> {
        // Assert the neg-risk amounts array is non-empty when routed NegRisk (routing sanity —
        // the exact on-chain amount value is the arbdub live-verify item, not asserted here).
        if let RedeemKind::NegRisk(amounts) = kind {
            assert!(!amounts.is_empty(), "neg-risk redeem must carry a non-empty amounts array");
        }
        self.redeemed
            .lock()
            .unwrap()
            .push((cid.to_string(), matches!(kind, RedeemKind::NegRisk(_))));
        Ok(RedeemResult { tx_hash: self.tx_hash.clone(), raw: "{\"state\":\"NEW\"}".into() })
    }
    fn confirm(&self, _condition_id: &str, _tx_hash: &str) -> Result<RedeemConfirmation, String> {
        let mut q = self.confirmations.lock().unwrap();
        if q.len() > 1 {
            Ok(q.remove(0))
        } else {
            Ok(q.first().cloned().unwrap_or(RedeemConfirmation::Unmined))
        }
    }
    fn payout(&self, p: &Position) -> Payout {
        self.payouts.get(&p.condition_id).copied().unwrap_or(Payout::Winner)
    }
}

fn pos(cid: &str, redeemable: bool, size: f64, neg: bool) -> Position {
    Position {
        condition_id: cid.into(),
        asset: "a".into(),
        size,
        redeemable,
        neg_risk: neg,
        outcome_index: Some(0),
        title: "t".into(),
        cur_price: Some(1.0),
    }
}

fn ledger(dir: &tempfile::TempDir) -> RedeemLedger {
    RedeemLedger::open(dir.path().join("r.jsonl"))
}

// =============================================================================================
// THE FIX: a relayer 2xx does not settle; only on-chain proof does.
// =============================================================================================

/// **The regression guard for the bug this module was fixed for.** The relayer accepted the
/// redeem (HTTP 2xx, a transaction hash in hand) and the chain has NOT confirmed it. The ledger
/// must NOT be settled — the pre-fix code wrote its permanent tombstone right here, and a
/// dropped or reverted transaction then forfeited the position forever.
#[test]
fn a_relayer_2xx_without_on_chain_confirmation_does_not_settle_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]);

    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
    assert_eq!(rep.submitted, vec!["0x1".to_string()], "handed to the relayer");
    assert!(rep.settled.is_empty(), "NOTHING is settled by a 2xx");
    assert!(!l.is_settled("0x1"), "no permanent tombstone on an unconfirmed submit");
    assert_eq!(
        l.pending("0x1").map(|p| p.tx_hash),
        Some(Some("0xtx".to_string())),
        "recorded as in-flight, with the relayer's tx hash"
    );
}

/// The other half: once the chain proves it, the ledger settles permanently and survives a
/// restart.
#[test]
fn a_confirmed_receipt_settles_the_ledger_permanently() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.jsonl");
    let deps =
        StubDeps::new(vec![pos("0x1", true, 100.0, false)]).scripted(vec![settled(90_715_029)]);
    {
        let l = RedeemLedger::open(path.clone());
        redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0); // submit
        let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000); // confirm
        assert_eq!(rep.settled, vec!["0x1".to_string()]);
        assert!(l.is_settled("0x1"));
    }
    // the tombstone is persisted, and a later tick never re-submits.
    let l = RedeemLedger::open(path);
    assert!(l.is_settled("0x1"), "settlement survives a restart");
    let before = deps.submits().len();
    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 2_000);
    assert!(rep.submitted.is_empty() && rep.settled.is_empty());
    assert_eq!(deps.submits().len(), before, "a settled condition is never re-submitted");
}

/// A CONFIRMED redemption that paid ~nothing still settles PERMANENTLY. This looks like the
/// wrong answer and is the right one, so it is pinned: the alarm added alongside it is a `warn`,
/// deliberately NOT a `reopen`. `Settled` already carries on-chain proof (status 1 + a
/// `PayoutRedemption` for THIS conditionId + `MIN_CONFIRMATIONS` deep) — the redemption really
/// happened, so there is nothing to retry, and reopening would re-submit the same call forever
/// against a condition that has already paid out. What the zero payout indicts is the DECISION
/// to redeem (winner selection) or the CALLDATA, neither of which a retry would change.
#[test]
fn a_zero_payout_settlement_still_tombstones_and_never_retries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.jsonl");
    // Below ZERO_PAYOUT_ALERT_USDC: the shape a V1 index-set `[1, 2]` would have redeemed.
    let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)])
        .scripted(vec![RedeemConfirmation::Settled { block: 90_715_029, payout_usdc: 0.000_003 }]);
    let l = RedeemLedger::open(path);
    redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0); // submit
    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000); // confirm
    assert_eq!(rep.settled, vec!["0x1".to_string()], "settled, not reverted");
    assert!(rep.reverted.is_empty(), "a proven redemption is never reopened for retry");
    assert!(l.is_settled("0x1"), "the tombstone is written on proof, not on amount");
    let before = deps.submits().len();
    redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 2_000);
    assert_eq!(deps.submits().len(), before, "never re-submitted");
    // The threshold itself: far above the dust it catches, far below any real winner. `const`
    // blocks because these ARE compile-time facts (clippy::assertions_on_constants), which is
    // the stronger form anyway — a bad edit to the constant fails the BUILD, not just this test.
    const { assert!(0.000_003 < ZERO_PAYOUT_ALERT_USDC, "dust must trip the alarm") };
    const { assert!(ZERO_PAYOUT_ALERT_USDC < 1.0, "a one-share winner must not") };
}

/// Mined but not yet buried `MIN_CONFIRMATIONS` deep: wait, do not settle, do not re-submit.
#[test]
fn a_maturing_receipt_neither_settles_nor_resubmits() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)])
        .scripted(vec![RedeemConfirmation::Maturing { block: 90_715_029, confirmations: 4 }]);
    redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
    // far beyond BOTH timeouts — a transaction we can see mined never ages into "dropped".
    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), PENDING_TX_TIMEOUT_MS * 10);
    assert_eq!(rep.awaiting, vec!["0x1".to_string()]);
    assert!(rep.settled.is_empty() && rep.reverted.is_empty() && rep.submitted.is_empty());
    assert!(!l.is_settled("0x1"));
    assert_eq!(deps.submits().len(), 1, "no re-submit while it matures");
}

/// A REVERTED transaction reopens the row and the next tick retries it — the failure mode that
/// used to be a silent permanent forfeit.
#[test]
fn a_reverted_transaction_reopens_the_row_and_retries() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]).scripted(vec![reverted()]);
    redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0); // submit #1
    assert_eq!(deps.submits().len(), 1);

    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000);
    assert_eq!(rep.reverted, vec!["0x1".to_string()], "the chain said it did not happen");
    assert_eq!(rep.submitted, vec!["0x1".to_string()], "and it is re-submitted the same tick");
    assert_eq!(deps.submits().len(), 2);
    assert!(!l.is_settled("0x1"));
    // `reopen` clears the row outright, so the fresh submit starts a fresh window at attempt 1.
    // Cross-tick failure counting is the poller's session-local job, not the ledger's.
    assert_eq!(l.pending("0x1").map(|p| p.attempts), Some(1), "a fresh in-flight window");
}

/// A transaction hash that never gets a receipt is presumed DROPPED once past the timeout, and
/// only then. Before the timeout the row is left strictly alone.
#[test]
fn an_unmined_transaction_is_reopened_only_after_the_drop_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]);
    redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);

    let early = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), PENDING_TX_TIMEOUT_MS - 1);
    assert_eq!(early.awaiting, vec!["0x1".to_string()]);
    assert!(early.reverted.is_empty());
    assert_eq!(deps.submits().len(), 1, "no re-submit inside the drop window");

    let late = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), PENDING_TX_TIMEOUT_MS);
    assert_eq!(late.reverted, vec!["0x1".to_string()]);
    assert_eq!(late.submitted, vec!["0x1".to_string()]);
    assert_eq!(deps.submits().len(), 2);
}

/// An RPC failure is a TRANSPORT verdict, never a settlement one: the row is untouched.
#[test]
fn a_confirmation_rpc_error_changes_nothing() {
    struct RpcDownDeps(StubDeps);
    impl RedeemDeps for RpcDownDeps {
        fn list_positions(&self, p: &str) -> Result<Vec<Position>, String> {
            self.0.list_positions(p)
        }
        fn redeem(&self, p: &str, c: &str, k: &RedeemKind) -> Result<RedeemResult, String> {
            self.0.redeem(p, c, k)
        }
        fn confirm(&self, _c: &str, _t: &str) -> Result<RedeemConfirmation, String> {
            Err("rpc eth_getTransactionReceipt: network: timed out".into())
        }
        fn payout(&self, p: &Position) -> Payout {
            self.0.payout(p)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = RpcDownDeps(StubDeps::new(vec![pos("0x1", true, 100.0, false)]));
    redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
    let before = l.pending("0x1");
    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000);
    assert_eq!(rep.awaiting, vec!["0x1".to_string()]);
    assert!(rep.settled.is_empty() && rep.reverted.is_empty() && rep.submitted.is_empty());
    assert_eq!(l.pending("0x1"), before, "ledger row byte-identical after an RPC blip");
}

// =============================================================================================
// The crash window — and which of the two errors this design prefers.
// =============================================================================================

/// **Crash mid-window, and the choice stated in the test name.** The write-ahead record is on
/// disk before the relayer is contacted; the process then dies before any transaction hash is
/// recorded. On restart the row is PENDING-with-no-hash, so:
///   * it is NOT settled → the position is never forfeited (the pre-fix failure), and
///   * it is NOT immediately re-submitted → no blind double-send on the very next tick;
///   * after `PENDING_UNKNOWN_TIMEOUT_MS` it is re-submitted exactly once more.
///
/// **The chosen failure is a bounded, delayed DUPLICATE SUBMIT — never a forfeit.** That is
/// safe because `redeemPositions` burns the caller's balance: a second call against an
/// already-redeemed condition pays nothing rather than paying twice (see `redeem_ledger`'s
/// module doc), and because the retry only fires for a position the data-api still reports as
/// redeemable — i.e. one whose first submit did not land.
#[test]
fn crash_between_write_ahead_and_confirmation_prefers_a_delayed_duplicate_submit_over_a_forfeit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.jsonl");
    // the relayer answers 2xx with NO hash — the pinned `{"state":"NEW"}` shape, and also what a
    // crash before `attach_tx` leaves behind.
    let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]).without_tx_hash();

    {
        let l = RedeemLedger::open(path.clone());
        redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
        // "crash": drop the ledger with the row still pending and no hash.
    }
    assert_eq!(deps.submits().len(), 1);

    let l = RedeemLedger::open(path);
    assert!(!l.is_settled("0x1"), "NOT a forfeit: the crashed submit is not a tombstone");
    assert_eq!(l.pending("0x1").map(|p| p.tx_hash), Some(None), "pending, no hash to confirm");

    // Inside the window: suppressed, no second send.
    let inside = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), PENDING_UNKNOWN_TIMEOUT_MS - 1);
    assert_eq!(inside.awaiting, vec!["0x1".to_string()]);
    assert!(inside.submitted.is_empty());
    assert_eq!(deps.submits().len(), 1, "no double-send inside the window");

    // Past the window: exactly one more attempt — the deliberate duplicate-submit choice.
    let after = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), PENDING_UNKNOWN_TIMEOUT_MS);
    assert_eq!(after.submitted, vec!["0x1".to_string()]);
    assert_eq!(deps.submits().len(), 2);
    assert_eq!(l.pending("0x1").map(|p| p.attempts), Some(2));
}

/// The other half of the crash choice: if the crashed submit DID land, the position stops being
/// redeemable and discovery never re-offers it — so the duplicate submit above does not even
/// happen in the case that matters.
#[test]
fn a_landed_submit_is_never_retried_because_discovery_stops_offering_it() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]).without_tx_hash();
    redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);

    // the redeem landed on-chain: the data-api now reports the position as no longer redeemable.
    let settled_deps = StubDeps::new(vec![pos("0x1", false, 100.0, false)]).without_tx_hash();
    let rep =
        redeem_once(&settled_deps, "0xproxy", &l, &HashSet::new(), PENDING_UNKNOWN_TIMEOUT_MS * 10);
    assert!(rep.submitted.is_empty(), "gone from discovery ⇒ never re-submitted");
    assert!(settled_deps.submits().is_empty());
}

/// A relayer error leaves the pending record standing (the outcome is genuinely unknown), so
/// the retry waits out the submit window instead of firing on the next tick.
#[test]
fn a_relayer_error_leaves_the_row_pending_and_delays_the_retry() {
    struct FailingDeps {
        calls: Mutex<u32>,
    }
    impl RedeemDeps for FailingDeps {
        fn list_positions(&self, _p: &str) -> Result<Vec<Position>, String> {
            Ok(vec![pos("0x1", true, 100.0, false)])
        }
        fn redeem(&self, _p: &str, _c: &str, _k: &RedeemKind) -> Result<RedeemResult, String> {
            *self.calls.lock().unwrap() += 1;
            Err("relayer 500".into())
        }
        fn confirm(&self, _c: &str, _t: &str) -> Result<RedeemConfirmation, String> {
            panic!("no tx hash was ever recorded — confirm must not be called");
        }
        fn payout(&self, _p: &Position) -> Payout {
            Payout::Winner
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = FailingDeps { calls: Mutex::new(0) };

    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
    assert_eq!(rep.failed, vec!["0x1".to_string()]);
    assert!(rep.submitted.is_empty());
    assert!(!l.is_settled("0x1"), "a failed submit is never a tombstone");
    assert!(l.pending("0x1").is_some(), "left pending: accepted-then-disconnected is possible");

    // next tick, inside the window → no second POST.
    redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000);
    assert_eq!(*deps.calls.lock().unwrap(), 1);
    // past the window → one more.
    redeem_once(&deps, "0xproxy", &l, &HashSet::new(), PENDING_UNKNOWN_TIMEOUT_MS);
    assert_eq!(*deps.calls.lock().unwrap(), 2);
}

/// Confirmation does not depend on discovery: a data-api outage still lets an in-flight
/// redemption settle.
#[test]
fn confirmation_runs_even_when_the_data_api_is_down() {
    struct DiscoveryDownDeps {
        inner: StubDeps,
        down: Mutex<bool>,
    }
    impl RedeemDeps for DiscoveryDownDeps {
        fn list_positions(&self, p: &str) -> Result<Vec<Position>, String> {
            if *self.down.lock().unwrap() {
                return Err("data-api 503".into());
            }
            self.inner.list_positions(p)
        }
        fn redeem(&self, p: &str, c: &str, k: &RedeemKind) -> Result<RedeemResult, String> {
            self.inner.redeem(p, c, k)
        }
        fn confirm(&self, c: &str, t: &str) -> Result<RedeemConfirmation, String> {
            self.inner.confirm(c, t)
        }
        fn payout(&self, p: &Position) -> Payout {
            self.inner.payout(p)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = DiscoveryDownDeps {
        inner: StubDeps::new(vec![pos("0x1", true, 100.0, false)])
            .scripted(vec![settled(90_715_029)]),
        down: Mutex::new(false),
    };
    redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0); // submit while discovery works
    *deps.down.lock().unwrap() = true;
    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000);
    assert_eq!(rep.settled, vec!["0x1".to_string()], "settled despite the data-api being down");
    assert!(l.is_settled("0x1"));
}

/// A session-disabled conditionId still gets CONFIRMED — an exclusion must stop new submits, not
/// strand money that is already in flight.
#[test]
fn an_excluded_condition_is_still_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps =
        StubDeps::new(vec![pos("0x1", true, 100.0, false)]).scripted(vec![settled(90_715_029)]);
    redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);

    let mut exclude = HashSet::new();
    exclude.insert("0x1".to_string());
    let rep = redeem_once(&deps, "0xproxy", &l, &exclude, 1_000);
    assert_eq!(rep.settled, vec!["0x1".to_string()]);
    assert!(l.is_settled("0x1"));
}

// =============================================================================================
// THE WINNER CHECK: `redeemable: true` is a RESOLUTION flag, so only the PAYOUT may authorise a
// relayer call. The fixture is this repo's real mainnet wallet — see `positions.rs`'s
// `live_wallet_fixture` for the same table exercised at the filter level.
// =============================================================================================

/// The four real positions of funder `0x107C01D0…`, in data-api order: three resolved LOSERS
/// then the one $5 winner, every row `redeemable: true`.
fn live_wallet() -> Vec<Position> {
    vec![
        pos("0x13bf6efb", true, 29.11, false), // Solana Up/Down Jun 9  — LOSER
        pos("0xbf336239", true, 10.0, false),  // Doge  Up/Down Jun 11  — LOSER
        pos("0x5a71ff88", true, 10.0, false),  // Doge  Up/Down Jun 9   — LOSER
        pos("0xf361b0aa", true, 5.0, false),   // BNB   Up/Down Jun 12  — WINNER, $5
    ]
}

fn live_wallet_payouts() -> Vec<(&'static str, Payout)> {
    vec![
        ("0x13bf6efb", Payout::Loser),
        ("0xbf336239", Payout::Loser),
        ("0x5a71ff88", Payout::Loser),
        ("0xf361b0aa", Payout::Winner),
    ]
}

/// **The D1 regression guard.** All four rows report `redeemable: true`; exactly one pays. The
/// poller must contact the relayer ONCE — for the winner — not four times.
#[test]
fn redeem_once_submits_only_the_winner_of_four_redeemable_positions() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = StubDeps::new(live_wallet()).with_payouts(&live_wallet_payouts());

    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
    assert_eq!(
        rep.submitted,
        vec!["0xf361b0aa".to_string()],
        "one relayer call, for the only position that pays"
    );
    assert_eq!(
        deps.submits(),
        vec![("0xf361b0aa".to_string(), false)],
        "the three resolved losers never reach submit_redeem"
    );
    for loser in ["0x13bf6efb", "0xbf336239", "0x5a71ff88"] {
        assert!(l.pending(loser).is_none(), "no ledger row is written for {loser}");
        assert!(!l.is_settled(loser), "and certainly no tombstone");
    }
}

/// A `Payout::Unknown` verdict (an RPC blip, or a chain that says the condition has not resolved)
/// submits NOTHING and writes NOTHING — and the very next pass, once the source answers,
/// submits the winner. Fail closed is a delay, never a forfeit.
#[test]
fn an_unknown_payout_submits_nothing_and_the_next_pass_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.jsonl");
    let l = RedeemLedger::open(path);

    // EVERY cid unknown — an unreachable RPC does not answer for one condition and not another.
    let blind = StubDeps::new(live_wallet()).with_payouts(
        &live_wallet_payouts().iter().map(|(c, _)| (*c, Payout::Unknown)).collect::<Vec<_>>(),
    );
    let rep = redeem_once(&blind, "0xproxy", &l, &HashSet::new(), 0);
    assert!(rep.submitted.is_empty() && blind.submits().is_empty(), "nothing on an Unknown");
    assert!(l.pending("0xf361b0aa").is_none(), "and no write-ahead record either");

    let seeing = StubDeps::new(live_wallet()).with_payouts(&live_wallet_payouts());
    let rep = redeem_once(&seeing, "0xproxy", &l, &HashSet::new(), 1_000);
    assert_eq!(rep.submitted, vec!["0xf361b0aa".to_string()], "recovered on the next pass");
}

/// A neg-risk LOSER is skipped by the same rule — the winner check runs before the binary /
/// neg-risk routing, so neither contract path can be handed a worthless position.
#[test]
fn a_neg_risk_loser_is_skipped_too() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = StubDeps::new(vec![pos("0xneg", true, 7.0, true), pos("0xbin", true, 7.0, false)])
        .with_payouts(&[("0xneg", Payout::Loser), ("0xbin", Payout::Winner)]);
    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
    assert_eq!(rep.submitted, vec!["0xbin".to_string()]);
    assert_eq!(deps.submits(), vec![("0xbin".to_string(), false)]);
}

// =============================================================================================
// Pre-existing behaviour that must not regress.
// =============================================================================================

/// Both a binary AND a neg-risk winner are submitted (an unresolved position is skipped), then
/// both settle on chain proof.
#[test]
fn redeem_once_submits_binary_and_neg_risk_winners_then_settles_them() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = StubDeps::new(vec![
        pos("0x1", true, 100.0, false), // binary winner
        pos("0x2", false, 5.0, false),  // still trading (condition not resolved)
        pos("0x3", true, 5.0, true),    // neg-risk winner
    ])
    .scripted(vec![settled(1)]);

    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
    assert_eq!(rep.submitted.len(), 2, "both winners submitted, the unresolved one skipped");
    assert!(rep.settled.is_empty(), "nothing settles on the submit tick");
    assert!(!l.is_settled("0x1") && !l.is_settled("0x3"));

    let rep2 = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 1_000);
    assert_eq!(rep2.settled.len(), 2, "both settle once the chain proves them");
    assert!(l.is_settled("0x1") && l.is_settled("0x3"));

    let calls = deps.submits();
    assert!(calls.contains(&("0x1".to_string(), false)), "0x1 routed Binary");
    assert!(calls.contains(&("0x3".to_string(), true)), "0x3 routed NegRisk");
}

/// The routing assertion: a binary winner routes [`RedeemKind::Binary`] and a neg-risk winner
/// routes [`RedeemKind::NegRisk`] (with a non-empty amounts array — asserted in the stub).
#[test]
fn redeem_once_routes_binary_and_neg_risk() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false), pos("0x3", true, 5.0, true)]);
    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
    let calls = deps.submits();
    assert!(calls.contains(&("0x1".to_string(), false)));
    assert!(calls.contains(&("0x3".to_string(), true)));
    assert_eq!(rep.submitted.len(), 2);
}

/// `neg_risk_amounts` puts the 6-decimal base-unit balance at the position's slot and 0 at the
/// other, and is panic-free for garbage sizes (NaN/negative → 0). ASSUMED derivation — the exact
/// on-chain amount correctness is the arbdub live-verify item; this just guards the shape.
#[test]
fn neg_risk_amounts_places_base_units_at_slot() {
    let mut p = pos("0xc", true, 5.0, true); // slot 0, 5.0 tokens
    assert_eq!(neg_risk_amounts(&p), vec![5_000_000, 0]);
    p.outcome_index = Some(1);
    assert_eq!(neg_risk_amounts(&p), vec![0, 5_000_000]);
    p.outcome_index = Some(7); // out-of-range slot clamps to 1
    assert_eq!(neg_risk_amounts(&p), vec![0, 5_000_000]);
    // panic-free garbage sizes → 0 amount (safe no-op).
    let mut bad = pos("0xc", true, f64::NAN, true);
    assert_eq!(neg_risk_amounts(&bad), vec![0, 0]);
    bad.size = -3.0;
    assert_eq!(neg_risk_amounts(&bad), vec![0, 0]);
}

/// An UNREADABLE slot never invents one: the amounts array stays all-zero, the same safe no-op a
/// garbage size yields. Belt-and-braces — `payout_of` already refuses such a position, which the
/// second half asserts end-to-end: it is not even a redeem candidate, so nothing is submitted.
#[test]
fn neg_risk_amounts_invents_no_slot_when_the_outcome_index_is_unreadable() {
    let mut p = pos("0xc", true, 5.0, true);
    p.outcome_index = None;
    assert_eq!(neg_risk_amounts(&p), vec![0, 0], "no slot to address ⇒ no amount placed");

    // ...and it can't get here anyway: an unreadable slot is `Unknown` under every source.
    assert_eq!(payout_of(&p, &WinnerSource::CurPriceOnly), Payout::Unknown);

    // End-to-end through `redeem_once` with the REAL payout policy (`ProdDeps`'s
    // `CurPriceOnly` arm verbatim) rather than `StubDeps`'s scripted verdict — otherwise this
    // would only be testing the stub. `p` is `redeemable: true`, `size > 0`, `curPrice: 1.0`:
    // everything the old code needed to call it a winner. Only the unreadable slot stops it.
    struct RealPayoutDeps(StubDeps);
    impl RedeemDeps for RealPayoutDeps {
        fn list_positions(&self, p: &str) -> Result<Vec<Position>, String> {
            self.0.list_positions(p)
        }
        fn redeem(&self, p: &str, c: &str, k: &RedeemKind) -> Result<RedeemResult, String> {
            self.0.redeem(p, c, k)
        }
        fn confirm(&self, c: &str, t: &str) -> Result<RedeemConfirmation, String> {
            self.0.confirm(c, t)
        }
        fn payout(&self, p: &Position) -> Payout {
            payout_of(p, &WinnerSource::CurPriceOnly)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = RealPayoutDeps(StubDeps::new(vec![p]));
    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
    assert!(rep.submitted.is_empty(), "an unreadable slot must not reach the relayer");
    assert!(deps.0.submits().is_empty(), "redeem() was never called");
    assert!(l.pending("0xc").is_none(), "and no ledger row was written");
}

/// Credentials for a spawn that must never reach the network: no test here gets past a gate with
/// them.
fn test_creds() -> PolymarketCreds {
    PolymarketCreds::default()
}

/// D4: the poller starts only when its caller enables it — creds alone start nothing.
#[test]
fn spawn_returns_none_unless_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let h = AutoRedeemPoller::spawn(
        Some(test_creds()),
        "0xproxy".into(),
        dir.path().join("ledger.jsonl"),
        dir.path().join("HALT"),
        Duration::from_secs(60),
        false,
        false,
        ChainRpcSettings::default(),
    );
    assert!(h.is_none(), "not enabled -> never starts");
}

/// The kill switch: the caller's `halted` OR the halt file — the FILE stays a runtime switch an
/// operator drops or removes while the poller runs.
#[test]
fn kill_switch_is_the_parameter_or_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let halt = dir.path().join("HALT");
    assert!(!kill_switch_tripped(false, &halt), "neither set");
    std::fs::write(&halt, "").unwrap();
    assert!(kill_switch_tripped(false, &halt), "the file trips it");
    std::fs::remove_file(&halt).unwrap();
    assert!(!kill_switch_tripped(false, &halt), "removing the file un-trips it");
    assert!(kill_switch_tripped(true, &halt), "the caller's halt trips it");
}

/// CRITICAL real-money regression guard: a wallet holding BOTH legs (YES+NO) of one resolved
/// binary market yields two Position rows with the SAME condition_id (different outcome_index),
/// both redeemable. `redeemPositions` settles the whole condition once, so the poller must call
/// `redeem` for that condition EXACTLY ONCE per tick — not once per leg.
#[test]
fn redeem_once_dedupes_both_legs_of_one_condition() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let yes = pos("0xcond", true, 100.0, false); // outcome_index 0 (from `pos`)
    let mut no = pos("0xcond", true, 100.0, false);
    no.outcome_index = Some(1); // the other leg of the SAME condition
    let deps = StubDeps::new(vec![yes, no]);
    let rep = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), 0);
    assert_eq!(rep.submitted, vec!["0xcond".to_string()], "one condition submitted, not two");
    assert_eq!(
        deps.submits(),
        vec![("0xcond".to_string(), false)],
        "redeem() called EXACTLY ONCE for the shared condition (no double on-chain submit)"
    );
    assert!(l.pending("0xcond").is_some());
}

/// The bounded-failure policy is FUNCTIONAL: a cid in the `exclude` set never reaches
/// `deps.redeem`. This is what the poller feeds after MAX_CONSECUTIVE_FAILURES, so a
/// permanently-failing cid stops being retried for the rest of the session.
#[test]
fn redeem_once_excludes_session_disabled_cids() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]);
    let mut exclude = HashSet::new();
    exclude.insert("0x1".to_string());
    let rep = redeem_once(&deps, "0xproxy", &l, &exclude, 0);
    assert!(rep.submitted.is_empty(), "excluded cid is not submitted");
    assert!(deps.submits().is_empty(), "redeem() not called for an excluded cid");
}

/// End-to-end of the bounded-failure loop logic (the exact bookkeeping the poller thread runs):
/// a cid whose redeem keeps REVERTING on chain hits the bound and stops being attempted. This
/// is the pre-fix `cid_failing_k_times_stops_being_attempted`, re-aimed at the failure mode the
/// fix made visible — a revert used to be invisible (it was tombstoned as a success).
#[test]
fn cid_reverting_k_times_stops_being_attempted() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    // every confirmation reverts → every tick re-submits, until the bound bites.
    let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]).scripted(vec![reverted()]);

    let mut consecutive_failures: HashMap<String, u32> = HashMap::new();
    let mut disabled_this_session: HashSet<String> = HashSet::new();
    let mut submits_per_tick = Vec::new();
    for tick in 0..(MAX_CONSECUTIVE_FAILURES + 4) {
        let report =
            redeem_once(&deps, "0xproxy", &l, &disabled_this_session, i64::from(tick) * 1_000);
        for cid in &report.settled {
            consecutive_failures.remove(cid);
        }
        for cid in report.failed.iter().chain(report.reverted.iter()) {
            let n = consecutive_failures.entry(cid.clone()).or_insert(0);
            *n += 1;
            if *n >= MAX_CONSECUTIVE_FAILURES {
                disabled_this_session.insert(cid.clone());
            }
        }
        submits_per_tick.push(deps.submits().len());
    }
    assert!(disabled_this_session.contains("0x1"), "the bound bit");
    let total = *submits_per_tick.last().unwrap();
    assert_eq!(
        submits_per_tick[submits_per_tick.len() - 2],
        total,
        "no further relayer calls once the bound bit"
    );
    // K reverts disable it; tick 0's own submit precedes the first revert, so the ceiling is
    // K+1 attempts, bounded either way — the point is that it STOPS.
    assert!(
        total <= MAX_CONSECUTIVE_FAILURES as usize + 1,
        "bounded retries, got {total} relayer calls"
    );
}

/// An on-chain settlement clears the failure counter — the poller's own bookkeeping, mirrored.
#[test]
fn a_settlement_clears_the_failure_counter() {
    let dir = tempfile::tempdir().unwrap();
    let l = ledger(&dir);
    let deps = StubDeps::new(vec![pos("0x1", true, 100.0, false)]).scripted(vec![
        reverted(),
        RedeemConfirmation::Unmined,
        settled(7),
    ]);
    let mut consecutive_failures: HashMap<String, u32> = HashMap::new();
    for tick in 0..4 {
        let report = redeem_once(&deps, "0xproxy", &l, &HashSet::new(), i64::from(tick) * 1_000);
        for cid in &report.settled {
            consecutive_failures.remove(cid);
        }
        for cid in report.failed.iter().chain(report.reverted.iter()) {
            *consecutive_failures.entry(cid.clone()).or_insert(0) += 1;
        }
    }
    assert!(l.is_settled("0x1"));
    assert!(consecutive_failures.is_empty(), "the settlement cleared the revert count");
}

#[test]
fn spawn_returns_none_without_creds() {
    // ENABLED, so the missing creds are the only thing that can stop it.
    let dir = tempfile::tempdir().unwrap();
    let h = AutoRedeemPoller::spawn(
        None,
        "0xproxy".into(),
        dir.path().join("ledger.jsonl"),
        dir.path().join("HALT"),
        Duration::from_secs(60),
        true,
        false,
        ChainRpcSettings::default(),
    );
    assert!(h.is_none(), "no creds -> never starts");
}
