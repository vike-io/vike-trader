//! **A reconcile pass runs PER ACCOUNT, not per venue** — the proofs for the producer that stamps
//! `vike_exec::ReconcileReports::route_key`.
//!
//! # What was wrong
//!
//! `ReconcileReports` has carried two facts for a while — `venue` (canonical: the health-probe
//! key, the label on every note, the dedup identity of a held alert) and `route_key` (which of
//! this process's ENGINES the divergences fold into) — and `CoreThread::reconcile_reports` has
//! routed by `ReconcileReports::route` since they split. **Nothing set `route_key` to `Some`.** The
//! reconcile manager keyed its clients by one string and enqueued one payload per that string, so
//! on a two-account node the second account's venue truth was diffed against the FIRST account's
//! local view. Under `hybrid` that is not an alert: `PositionDrift` auto-applies, rewriting the
//! first account's position size onto the second account's number and booking realized PnL at the
//! second account's average price, with no operator in front of it. The shipped default is
//! `quarantine`, which HOLDS instead — a fortunate default, not a safeguard, and one
//! `VIKE_RECONCILE_POLICY=hybrid` away from being neither.
//!
//! # …and the HELD half, which the diff half did not reach
//!
//! Routing a pass correctly is not the same as PARKING its divergence correctly, and under the
//! shipped default (`quarantine`) parking is the normal path rather than an edge case. The
//! held-alert identity (`vike_core`'s `recon_held::HeldId`) and the announcer keyed on it were
//! per VENUE, which at the ruled scale is not "two problems filed as one" — it is 49 accounts'
//! parked alerts being erased by the 50th on every pass, and 50 accounts' divergences collapsing
//! into ONE confirmable row whose route key names the first of them.
//! [`fifty_accounts_each_keep_their_own_held_alert`] and
//! [`a_confirm_at_fifty_accounts_folds_into_the_account_that_raised_it`] are that half.
//!
//! # ⚠ What these fixtures actually mount — read this before trusting a name
//!
//! Every test below mounts **SEVERAL ENGINES OF ONE EXCHANGE**: they all carry `venue: "binance"`
//! and differ only in `vike_exec::ExecutionEngine::route_key` (`"binance"`, `"binance#ALT"`, or
//! `"binance#A00".."binance#A49"`) — which is the shape `vike_mount::make_engine_accounts` builds
//! from an `[accounts]` table, and exactly what `vike_mount::account_route_key` renders. That is
//! stated here because the near-miss this suite exists to avoid is a test whose NAME promises two
//! ACCOUNTS while its body mounts two VENUES: two venues route correctly by accident under every
//! version of this code, so such a test passes before the fix and proves nothing.
//! [`two_engines_of_one_venue_are_two_accounts_not_two_venues`] pins the fixture itself so the
//! distinction cannot rot.
//!
//! ⚠ **Two accounts is the READABLE case; FIFTY is the one that gates.** Two cannot tell "merged"
//! from "erased" — with one neighbour, a set that replaces another looks the same as a set that
//! joins it — and the owner ruled the design must hold at *"50 accounts per venue"*. So the
//! held-half proofs mount fifty.
//!
//! # The mutations these are calibrated against
//!
//! **The diff half.** Change `crates/vike-core/src/recon_manager.rs`'s payload back to
//! `route_key: None` — the pre-fix producer, verbatim. Every leg's canonical `venue` then resolves
//! through `RouteKey::sole_account_of` to the DEFAULT account's engine, so a labelled account's
//! reports reach the default account's book where the symbol matches and are dropped where it does
//! not — except that the refusal below now catches the payload FIRST and folds nothing at all, on
//! either account. Either way the pairing assertions in
//! [`each_account_folds_only_its_own_venue_truth`] and
//! [`a_second_accounts_position_is_never_diffed_against_the_first`] go red, and so do the fifty-
//! account proofs (no pass folds, so no alert is ever raised).
//!
//! **The held half.** Drop the `account` field from `vike_core`'s `recon_held::HeldId` (or key
//! `HeldAnnouncer` by `venue` again) and [`fifty_accounts_each_keep_their_own_held_alert`] goes
//! red on its first assertion: fifty accounts reporting the identical position produce ONE row
//! instead of fifty, because a held identity that cannot name a book cannot tell fifty books apart.
//!
//! **The predicate half (2026-09-14).** Change `vike_core::ReconLeg::name_accounts_of_shared_venues`
//! to count the LEGS handed to it instead of the `engine_venues` it is given — the pre-fix
//! predicate, verbatim — and
//! [`a_venue_whose_second_account_has_no_recon_client_still_reconciles_the_first`] goes red: the
//! surviving leg's own pass is refused for want of a stamp nothing put on it, and the default
//! account's book stays empty while its leg plainly fetched. [`the_predicate_counts_engines_not_legs`]
//! goes red on the same statement, purely. [`one_engine_per_venue_stamps_nothing_at_all`] stays
//! GREEN under that mutation and under the fix alike, which is the point of it: it is the
//! byte-identity claim, and a change that moved it would be a change to every deployment running
//! today.
//!
//! Every mutation here is RUN and its exact red/green split is recorded in the branch's report —
//! never predicted from reading.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, HealthProbe, ReconConfig, ReconHealth, ReconLeg, spawn_core_multi};
use vike_exec::recon::{MassStatus, ReconClient, ReconMode, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, ExecutionClient, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::PositionStatusReport;
use vike_model::events::PositionSide;

type DynClient = Box<dyn ExecutionClient + Send>;

/// The canonical exchange id BOTH accounts below belong to. One venue, two books.
const VENUE: &str = "binance";
/// The second account's routing key — what `vike_mount::account_route_key` renders for a labelled
/// account, and what that account's `ExecutionEngine::route_key` carries.
const ALT_ROUTE: &str = "binance#ALT";

fn engine(route_key: &str, symbol: &str) -> ExecutionEngine<DynClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        Box::new(RecordingClient::default()) as DynClient,
        // ⚠ The CANONICAL venue for both engines — `ExecutionEngine::new` seeds `route_key` from
        // it, and the caller below overrides only that second field. This is the whole difference
        // between "two accounts" and "two venues".
        VENUE,
        symbol,
    );
    e.route_key = route_key.to_string();
    e
}

fn pos(symbol: &str, qty: f64) -> PositionStatusReport {
    PositionStatusReport {
        venue: VENUE.into(),
        symbol: symbol.into(),
        position_side: PositionSide::Both,
        qty,
        avg_px: 100.0,
        ts: 5,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    }
}

/// A `ReconClient` that reports a fixed position set and COUNTS the passes that reached it — the
/// count is what a suppression proof reads, the positions are what a routing proof reads.
struct CountingFake {
    positions: Vec<PositionStatusReport>,
    calls: Arc<AtomicUsize>,
}

impl ReconClient for CountingFake {
    fn fetch_order_status_reports(
        &self,
        _since: i64,
    ) -> Result<Vec<vike_model::OrderStatusReport>, String> {
        Ok(vec![])
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<vike_model::FillReport>, String> {
        Ok(vec![])
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(self.positions.clone())
    }
    /// Counted HERE rather than on the per-report fetches because the manager calls
    /// `fetch_mass_status` once per leg per pass; the default impl fans out to the three above, so
    /// counting one of those would count implementation detail instead of passes.
    fn fetch_mass_status(&self, since: i64) -> Result<MassStatus, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(MassStatus {
            orders: self.fetch_order_status_reports(since)?,
            fills: self.fetch_fill_reports(since)?,
            positions: self.fetch_position_status_reports()?,
        })
    }
}

fn client(positions: Vec<PositionStatusReport>) -> (Box<dyn ReconClient>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (Box::new(CountingFake { positions, calls: calls.clone() }), calls)
}

/// **The production ASSEMBLY step, applied exactly where `vike_run::build_node` applies it.**
///
/// A venue's DEFAULT account reaches the manager through `ReconLeg::sole_account_of` and its
/// labelled accounts through `ReconLeg::account`; neither constructor can see the other's rows, so
/// on a multi-account box the default leg would carry `route_key: None` — the string a
/// single-account box produces — beside siblings that name themselves. This step stamps it, and it
/// is what lets `CoreThread::reconcile_reports` REFUSE a `None` it cannot attribute instead of
/// folding it against the venue's default book.
///
/// Every fixture below goes through it, INCLUDING the single-account one, where it is a no-op —
/// that is the point: the same call produces byte-identical legs there.
///
/// ⚠ **`engine_venues` is a SEPARATE argument on purpose, and every caller below spells it from
/// the engines it actually mounted.** The predicate is the ENGINE set, because that is what the
/// refusal on the other side counts (`CoreThread::engines_of_venue`); deriving it from `legs` here
/// would rebuild the very defect the production caller was fixed for, and would make
/// [`a_venue_whose_second_account_has_no_recon_client_still_reconciles_the_first`] — the one
/// fixture where the two sets genuinely differ — impossible to express.
fn assembled_over(mut legs: Vec<ReconLeg>, engine_venues: &[&str]) -> Vec<ReconLeg> {
    ReconLeg::name_accounts_of_shared_venues(&mut legs, engine_venues);
    legs
}

/// [`assembled_over`] for the fixtures whose engines are ONE PER LEG — a venue with `n` legs is a
/// venue with `n` engines there, so the two sets coincide and the shorthand says nothing false.
/// A fixture where they differ must call [`assembled_over`] and name its engines.
fn assembled(legs: Vec<ReconLeg>) -> Vec<ReconLeg> {
    let owned: Vec<String> = legs.iter().map(|l| l.venue.clone()).collect();
    let venues: Vec<&str> = owned.iter().map(String::as_str).collect();
    assembled_over(legs, &venues)
}

fn core_config() -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    }
}

fn recon_config() -> ReconConfig {
    ReconConfig {
        policy: ReconPolicy::default(), // Synthesize — every divergence folds, nothing is held
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        ..ReconConfig::default()
    }
}

/// The signed size of `symbol` on the engine published at `venue_idx` (0 = primary account,
/// 1 = the first extra), or 0.0 when that engine holds no such position.
fn size_at(snap: &vike_core::CoreSnapshot, venue_idx: usize, symbol: &str) -> f64 {
    snap.portfolio
        .venues
        .get(venue_idx)
        .and_then(|vb| vb.positions.iter().find(|p| p.symbol == symbol))
        .map(|p| p.size)
        .unwrap_or(0.0)
}

/// Poll the published snapshot until `f` holds, or the deadline passes. Returns the last snapshot
/// either way, so the caller's assertions produce the diagnostic rather than a bare timeout.
fn settle(
    handle: &vike_core::CoreHandle,
    f: impl Fn(&vike_core::CoreSnapshot) -> bool,
) -> Arc<vike_core::CoreSnapshot> {
    let cell = handle.snapshot_cell();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snap = cell.load_full();
        if f(&snap) || Instant::now() > deadline {
            return snap;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// **THE FIXTURE ITSELF, pinned.** Two engines, ONE canonical venue, two route keys — so every
/// assertion below is about two ACCOUNTS and not about two venues.
///
/// This exists because the failure it guards against is silent: two engines on `"sim"` and
/// `"bin"` route correctly under the pre-fix producer too (each venue's payload names its own
/// engine), so a suite built on that fixture would have been green before the fix and green after
/// it, proving nothing while carrying a name that promised otherwise.
#[test]
fn two_engines_of_one_venue_are_two_accounts_not_two_venues() {
    let primary = engine(VENUE, "BTCUSDT");
    let alt = engine(ALT_ROUTE, "ETHUSDT");
    assert_eq!(primary.venue, alt.venue, "both engines must be the SAME exchange");
    assert_eq!(primary.venue, VENUE);
    assert_ne!(primary.route_key, alt.route_key, "…and different ACCOUNTS of it");
    assert_eq!(primary.route_key, VENUE, "a default account's route key is the bare venue id");
    assert_eq!(alt.route_key, ALT_ROUTE);
}

/// **PROOF 1 — two accounts, two passes, PAIRED.** A venue with two engines produces two legs, and
/// each leg's venue truth folds into ITS OWN engine's book: the default account ends holding only
/// what its own client reported, and the ALT account only what its own client reported.
///
/// The pairing is the assertion, not the count: a producer that enqueued two payloads and routed
/// both to the same engine would satisfy a count, and is exactly the defect.
///
/// ⚠ MEASURED under the pre-fix producer (`route_key: None`), not predicted: the ALT leg's
/// ETHUSDT row resolves to the DEFAULT account's engine, which is mounted on BTCUSDT and does
/// not accept it — so on this fixture the second account's venue truth is not merely misrouted,
/// it is LOST, and the ALT book ends empty. That is what turns the third assertion red.

#[test]
fn each_account_folds_only_its_own_venue_truth() {
    let handle = spawn_core_multi(
        engine(VENUE, "BTCUSDT"),
        vec![(10_000.0, engine(ALT_ROUTE, "ETHUSDT"))],
        core_config(),
    );

    // Each account's venue truth names a DIFFERENT instrument, so a payload that folds into the
    // wrong engine is visible as a position on a book that should never have heard of it.
    let (default_client, default_calls) = client(vec![pos("BTCUSDT", 2.0)]);
    let (alt_client, alt_calls) = client(vec![pos("ETHUSDT", 5.0)]);
    let driver = vike_core::spawn_recon(
        &handle,
        assembled(vec![
            ReconLeg::sole_account_of(VENUE, default_client),
            ReconLeg::account(VENUE, ALT_ROUTE, alt_client),
        ]),
        recon_config(),
        None,
    );

    let snap =
        settle(&handle, |s| size_at(s, 0, "BTCUSDT") != 0.0 && size_at(s, 1, "ETHUSDT") != 0.0);

    // Both legs actually fetched — the "nothing ran" guard, so every zero below is a fold that
    // happened and routed elsewhere rather than a pass that never happened.
    assert_eq!(default_calls.load(Ordering::Relaxed), 1, "the default account's leg ran");
    assert_eq!(alt_calls.load(Ordering::Relaxed), 1, "the second account's leg ran");

    // Both blocks are the SAME exchange — one per engine, primary first (`CoreSnapshot::build`).
    assert_eq!(snap.portfolio.venues.len(), 2, "one published block per engine");
    assert_eq!(snap.portfolio.venues[0].venue, VENUE);
    assert_eq!(snap.portfolio.venues[1].venue, VENUE);

    // THE PAIRING, both directions on both books.
    assert!(
        (size_at(&snap, 0, "BTCUSDT") - 2.0).abs() < 1e-12,
        "the DEFAULT account must hold its own leg's position; got {}",
        size_at(&snap, 0, "BTCUSDT")
    );
    assert_eq!(
        size_at(&snap, 0, "ETHUSDT"),
        0.0,
        "the ALT account's venue truth must never reach the default account's book"
    );
    assert!(
        (size_at(&snap, 1, "ETHUSDT") - 5.0).abs() < 1e-12,
        "the ALT account must hold its own leg's position; got {}",
        size_at(&snap, 1, "ETHUSDT")
    );
    assert_eq!(
        size_at(&snap, 1, "BTCUSDT"),
        0.0,
        "the default account's venue truth must never reach the ALT account's book"
    );

    driver.shutdown();
    handle.shutdown_and_join();
}

/// **PROOF 2 — the misdiff, closed.** Both accounts trade the SAME instrument, which is the shape
/// that makes the pre-fix defect destructive rather than merely untidy: routed to the wrong engine,
/// the second account's position row is diffed against the FIRST account's local view, and the
/// resulting divergence is one `hybrid` auto-applies — it rewrites the first account's size onto
/// the second's number and books realized PnL at the second's average price, with no operator in
/// front of it.
///
/// **The fixture is the brief's shape literally: account B holds a position account A does not.**
/// A's own venue truth is EMPTY, so after the pass A must still hold nothing — any position on A's
/// book can only have come from B's leg.
///
/// ⚠ MEASURED under the pre-fix producer (`route_key: None`), not predicted: B's leg resolves
/// through `RouteKey::sole_account_of("binance")` to the DEFAULT account's engine, A's local view
/// has no such position, so `diff` raises `PositionOnlyExternal` against A and `Synthesize` folds
/// B's 7.0 into A's book — while B's own book stays empty. Both assertions below go red, which is
/// the point: this is the divergence that would have rewritten A.
///
/// ⚠ The two call-count assertions are the "nothing ran" guard. Without them a pass that never
/// happened would satisfy "A holds nothing" and this test could read green for the wrong reason.
#[test]
fn a_second_accounts_position_is_never_diffed_against_the_first() {
    let handle = spawn_core_multi(
        engine(VENUE, "BTCUSDT"),
        vec![(10_000.0, engine(ALT_ROUTE, "BTCUSDT"))],
        core_config(),
    );

    // ONE instrument. Account A's venue truth is EMPTY; account B holds 7.0 of it.
    let (default_client, default_calls) = client(vec![]);
    let (alt_client, alt_calls) = client(vec![pos("BTCUSDT", 7.0)]);
    let driver = vike_core::spawn_recon(
        &handle,
        assembled(vec![
            ReconLeg::sole_account_of(VENUE, default_client),
            ReconLeg::account(VENUE, ALT_ROUTE, alt_client),
        ]),
        recon_config(),
        None,
    );

    let snap = settle(&handle, |s| size_at(s, 1, "BTCUSDT") != 0.0);

    // Both legs actually fetched — so "A holds nothing" below is a fold that happened and declined
    // to touch A, never a pass that never ran.
    assert_eq!(default_calls.load(Ordering::Relaxed), 1, "the default account's leg ran");
    assert_eq!(alt_calls.load(Ordering::Relaxed), 1, "the second account's leg ran");

    assert_eq!(
        size_at(&snap, 0, "BTCUSDT"),
        0.0,
        "THE MISDIFF: the default account reported no position and must still hold none — a size \
         here is the second account's venue truth folded into the first account's book"
    );
    assert!(
        (size_at(&snap, 1, "BTCUSDT") - 7.0).abs() < 1e-12,
        "…and the second account's book must carry its own venue truth (7.0); got {}",
        size_at(&snap, 1, "BTCUSDT")
    );

    driver.shutdown();
    handle.shutdown_and_join();
}

/// **PROOF 3 — a single-account venue is unchanged.** One engine per venue means one leg per
/// venue, built through `ReconLeg::sole_account_of`, whose payload carries `route_key: None` — the
/// literal value the pre-fix producer hardcoded. Same pass count, same routing, same book.
///
/// This is the shape of every deployment running today, and reconcile runs against REAL accounts
/// on them, so "unchanged" is the claim that matters most here.
#[test]
fn a_single_account_venue_is_unchanged() {
    let handle = spawn_core_multi(engine(VENUE, "BTCUSDT"), vec![], core_config());

    let (only_client, calls) = client(vec![pos("BTCUSDT", 3.0)]);
    let driver = vike_core::spawn_recon(
        &handle,
        assembled(vec![ReconLeg::sole_account_of(VENUE, only_client)]),
        recon_config(),
        None,
    );

    let snap = settle(&handle, |s| size_at(s, 0, "BTCUSDT") != 0.0);
    assert_eq!(snap.portfolio.venues.len(), 1, "one engine ⇒ one published block");
    assert_eq!(snap.portfolio.venues[0].venue, VENUE);
    assert!((size_at(&snap, 0, "BTCUSDT") - 3.0).abs() < 1e-12);
    assert_eq!(calls.load(Ordering::Relaxed), 1, "one leg ⇒ exactly one startup pass");

    driver.shutdown();
    handle.shutdown_and_join();
}

/// **PROOF 4 — the health gate still keys on the VENUE.** The probe is asked about the canonical
/// exchange id for BOTH accounts and never about a route key, so a degraded binance feed suppresses
/// BOTH binance legs while a healthy bybit leg in the same pass still runs.
///
/// ⚠ The probe-argument assertion is the load-bearing half. Keying the payload's `venue` field by
/// route key would route correctly and then silently take the second account OFF its health handle:
/// `should_reconcile` would probe `"binance#ALT"`, miss, and read `Healthy` — the suppression a
/// mid-gap feed is supposed to cause would stop applying to that account, with no error anywhere.
#[test]
fn the_health_gate_keys_on_the_canonical_venue_for_every_account() {
    let handle = spawn_core_multi(
        engine(VENUE, "BTCUSDT"),
        vec![(10_000.0, engine(ALT_ROUTE, "ETHUSDT"))],
        core_config(),
    );

    let (default_client, default_calls) = client(vec![pos("BTCUSDT", 2.0)]);
    let (alt_client, alt_calls) = client(vec![pos("ETHUSDT", 5.0)]);
    let (bybit_client, bybit_calls) = client(vec![]);

    // Every string the gate probes, in order — the direct observation of what the payload's
    // `venue` field carries, taken at the exact site that reads it.
    let asked: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let asked_probe = asked.clone();
    let health: HealthProbe = Arc::new(move |venue: &str| {
        asked_probe.lock().unwrap().push(venue.to_string());
        if venue == VENUE { ReconHealth::Degraded } else { ReconHealth::Healthy }
    });

    let driver = vike_core::spawn_recon(
        &handle,
        assembled(vec![
            ReconLeg::sole_account_of(VENUE, default_client),
            ReconLeg::account(VENUE, ALT_ROUTE, alt_client),
            ReconLeg::sole_account_of("bybit", bybit_client),
        ]),
        ReconConfig { health: Some(health), ..recon_config() },
        None,
    );

    // The bybit leg is the pass's completion signal: it is last in `clients` order, so once it has
    // fetched, both binance legs have already been offered to the gate.
    let deadline = Instant::now() + Duration::from_secs(5);
    while bybit_calls.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }

    let asked = asked.lock().unwrap().clone();
    assert_eq!(
        asked,
        vec![VENUE.to_string(), VENUE.to_string(), "bybit".to_string()],
        "the gate must be probed with CANONICAL venue ids — twice for binance (once per account) \
         and never with a route key"
    );
    assert!(
        !asked.iter().any(|v| v == ALT_ROUTE),
        "a route key must never reach the health probe: it misses the map and reads Healthy"
    );
    assert_eq!(
        default_calls.load(Ordering::Relaxed),
        0,
        "a degraded binance feed suppresses the default account's leg"
    );
    assert_eq!(
        alt_calls.load(Ordering::Relaxed),
        0,
        "…and the SECOND binance account's leg too — they read one exchange's feed"
    );
    assert_eq!(
        bybit_calls.load(Ordering::Relaxed),
        1,
        "a healthy venue in the same pass is unaffected"
    );

    driver.shutdown();
    handle.shutdown_and_join();
}

// ===============================================================================================
// FIFTY ACCOUNTS OF ONE EXCHANGE — the ruled scale, and the HELD half.
// ===============================================================================================

/// The number the owner ruled the design must hold at: *"it can be 50 accounts per venue."*
const FIFTY: usize = 50;

/// `binance` for account 0 (the DEFAULT account, whose route key is the bare venue id — that is
/// what `vike_mount::account_route_key` renders for `AccountLabel::Default`) and
/// `binance#A01`…`binance#A49` for the rest.
fn route_of(i: usize) -> String {
    if i == 0 { VENUE.to_string() } else { format!("{VENUE}#A{i:02}") }
}

/// Fifty engines of ONE exchange on ONE instrument, plus the fifty legs that reconcile them —
/// every leg's client reporting the IDENTICAL venue position.
///
/// ⚠ Identical on purpose. If each account reported a different symbol or size, their held
/// identities would differ by `instance` and a venue-keyed `HeldId` would still tell them apart —
/// the test would pass for a reason that has nothing to do with accounts. With one symbol, one
/// side and one size, the ACCOUNT is the only thing left that can separate them, which is exactly
/// the shape fifty accounts of one exchange running one spread actually take.
fn fifty_account_core() -> (vike_core::CoreHandle, Vec<ReconLeg>, Vec<Arc<AtomicUsize>>) {
    let primary = engine(&route_of(0), "BTCUSDT");
    let extras: Vec<(f64, ExecutionEngine<DynClient>)> =
        (1..FIFTY).map(|i| (10_000.0, engine(&route_of(i), "BTCUSDT"))).collect();
    let handle = spawn_core_multi(primary, extras, core_config());

    let mut legs = Vec::with_capacity(FIFTY);
    let mut calls = Vec::with_capacity(FIFTY);
    for i in 0..FIFTY {
        let (c, n) = client(vec![pos("BTCUSDT", 0.5)]);
        legs.push(if i == 0 {
            ReconLeg::sole_account_of(VENUE, c)
        } else {
            ReconLeg::account(VENUE, route_of(i), c)
        });
        calls.push(n);
    }
    (handle, assembled(legs), calls)
}

/// `quarantine` — the SHIPPED default, and the policy that makes "parked" the normal path rather
/// than an edge case. Nothing folds; every divergence is held for an operator.
fn quarantine_config() -> ReconConfig {
    ReconConfig {
        policy: ReconPolicy { default: ReconMode::Quarantine, ..Default::default() },
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        ..ReconConfig::default()
    }
}

/// Poll the published snapshot until `f` holds or the deadline passes — the fifty-engine twin of
/// [`settle`], with a longer deadline because one pass is fifty folds.
fn settle_slow(
    handle: &vike_core::CoreHandle,
    f: impl Fn(&vike_core::CoreSnapshot) -> bool,
) -> Arc<vike_core::CoreSnapshot> {
    let cell = handle.snapshot_cell();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let snap = cell.load_full();
        if f(&snap) || Instant::now() > deadline {
            return snap;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// **PROOF 5 — THE MAJOR, at the ruled scale.** Fifty accounts of one exchange, one pass, each
/// account holding the IDENTICAL divergence: after the whole pass all FIFTY are still held, and
/// each names its own account.
///
/// ⚠ A two-account version of this cannot distinguish "merged" from "erased", which is why the
/// ruling's number is the one that gates. Keyed by venue, this pass:
///
/// * collapses all fifty into ONE alert row — a `dedup_key` (`position:{symbol}:{side}`) and an
///   un-keyed alert's instrument legs both name an INSTRUMENT, never a book, so fifty accounts
///   holding one position produce fifty EQUAL identities;
/// * leaves that row's stored `route_key` naming the FIRST account while its payload is refreshed
///   with whichever leg ran last, so the one confirm an operator can reach folds another account's
///   synthesized fills into the wrong book;
/// * and announces each leg's set as newly held while reporting the previous leg's — still held,
///   still waiting — as CLEARED, every pass, forever.
#[test]
fn fifty_accounts_each_keep_their_own_held_alert() {
    let (handle, legs, calls) = fifty_account_core();
    let driver = vike_core::spawn_recon(&handle, legs, quarantine_config(), None);

    let snap = settle_slow(&handle, |s| s.recon.alerts.len() >= FIFTY);

    // The "nothing ran" guard, first: every zero and every count below has to be a pass that
    // HAPPENED, never a driver that never got going.
    for (i, n) in calls.iter().enumerate() {
        assert_eq!(n.load(Ordering::Relaxed), 1, "account {i} leg must have fetched exactly once");
    }

    assert_eq!(
        snap.recon.alerts.len(),
        FIFTY,
        "fifty accounts holding one divergence each are FIFTY held rows, not one: {:?}",
        snap.recon.alerts.iter().map(|a| (&a.kind, &a.account)).collect::<Vec<_>>()
    );

    // …and each row names ITS OWN account. One row per route key, all fifty distinct.
    let mut accounts: Vec<String> = snap.recon.alerts.iter().map(|a| a.account.clone()).collect();
    accounts.sort();
    let mut expected: Vec<String> = (0..FIFTY).map(route_of).collect();
    expected.sort();
    assert_eq!(accounts, expected, "every account must own exactly one of the fifty held rows");

    // Nothing folded — `quarantine` holds — so no account's book moved on the strength of another's
    // venue truth. (A collapse would have folded nothing either; this is the byte-identity half.)
    for i in 0..FIFTY {
        assert_eq!(
            size_at(&snap, i, "BTCUSDT"),
            0.0,
            "account {i} must hold nothing: quarantine folds no divergence"
        );
    }

    driver.shutdown();
    handle.shutdown_and_join();
}

/// **PROOF 6 — a confirm resolves to the account that RAISED it, at fifty.** Confirming account
/// 37's held alert folds its synthesized position into account 37's book and touches no other.
///
/// This is the half the collapse made unreachable rather than merely wrong: with fifty identities
/// equal there is ONE confirm id for fifty books, so an operator could not express "account 37"
/// even if they knew which one they meant.
#[test]
fn a_confirm_at_fifty_accounts_folds_into_the_account_that_raised_it() {
    const TARGET: usize = 37;
    let (handle, legs, _calls) = fifty_account_core();
    let driver = vike_core::spawn_recon(&handle, legs, quarantine_config(), None);

    let snap = settle_slow(&handle, |s| s.recon.alerts.len() >= FIFTY);
    assert_eq!(snap.recon.alerts.len(), FIFTY, "PREMISE: fifty rows to choose from");

    let target_route = route_of(TARGET);
    let alert = snap
        .recon
        .alerts
        .iter()
        .find(|a| a.account == target_route)
        .unwrap_or_else(|| panic!("no held row names account {target_route}"))
        .clone();
    assert!(alert.proposed_event_count > 0, "PREMISE: this divergence has something to fold");

    handle.send_command(vike_exec::Command::ConfirmRecon(alert.id));

    let snap = settle_slow(&handle, |s| size_at(s, TARGET, "BTCUSDT") != 0.0);
    assert!(
        (size_at(&snap, TARGET, "BTCUSDT") - 0.5).abs() < 1e-12,
        "the confirmed account book must carry the position it raised; got {}",
        size_at(&snap, TARGET, "BTCUSDT")
    );
    for i in (0..FIFTY).filter(|&i| i != TARGET) {
        assert_eq!(
            size_at(&snap, i, "BTCUSDT"),
            0.0,
            "account {i} was not confirmed and its book must not have moved"
        );
    }
    assert_eq!(
        snap.recon.alerts.len(),
        FIFTY - 1,
        "…and exactly ONE row was resolved — the other 49 accounts still await their own operator"
    );

    driver.shutdown();
    handle.shutdown_and_join();
}

/// **PROOF 7 — CLASS E, THE REFUSAL.** A pass that cannot say WHICH account it read is not
/// reported against the venue's default: on a venue this process runs several engines of, a
/// payload carrying `route_key: None` folds NOTHING.
///
/// The fixture is the pre-account-fan-out producer's payload verbatim — a leg built by
/// `ReconLeg::sole_account_of` and NOT run through the assembly step — which is exactly the shape
/// a REPLAYED journal `Command::ReconcileReports` carries on a box that has since grown accounts.
/// Folding it would diff one book's venue truth against another book's local view; under `hybrid`
/// that is `PositionDrift`, which auto-applies.
///
/// ⚠ The call-count assertion is the "nothing ran" guard: the leg must have FETCHED, so the empty
/// book below is a refusal and not a driver that never started.
#[test]
fn a_pass_that_names_no_account_is_refused_where_the_venue_has_several() {
    let handle = spawn_core_multi(
        engine(VENUE, "BTCUSDT"),
        vec![(10_000.0, engine(ALT_ROUTE, "BTCUSDT"))],
        core_config(),
    );

    // NOT `assembled(...)`: this is the un-stamped leg a pre-fix build (or a pre-fix journal)
    // produces, and it is the whole premise of the test.
    let (unnamed_client, calls) = client(vec![pos("BTCUSDT", 9.0)]);
    let driver = vike_core::spawn_recon(
        &handle,
        vec![ReconLeg::sole_account_of(VENUE, unnamed_client)],
        recon_config(), // Synthesize — so a fold, if one happened, would be VISIBLE
        None,
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while calls.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(calls.load(Ordering::Relaxed), 1, "the leg RAN — its payload reached the core");

    // Give the fold thread room to do the wrong thing if it is going to.
    let snap =
        settle_slow(&handle, |s| size_at(s, 0, "BTCUSDT") != 0.0 || s.recon.last_pass_ts > 0);
    assert_eq!(
        size_at(&snap, 0, "BTCUSDT"),
        0.0,
        "REFUSED: an unattributable pass must not fold into the venue DEFAULT account"
    );
    assert_eq!(size_at(&snap, 1, "BTCUSDT"), 0.0, "…nor into any other account of it");
    assert!(snap.recon.alerts.is_empty(), "a refused pass raises no held alert either");

    driver.shutdown();
    handle.shutdown_and_join();
}

/// …and the OTHER half of the refusal, which is the one every deployment running today depends on:
/// on a venue with ONE account, a `route_key: None` pass is the ordinary shape and folds exactly as
/// it always did. The refusal is structurally unreachable there — `engines_of_venue` answers 1 —
/// rather than merely unlikely.
#[test]
fn a_pass_that_names_no_account_still_folds_where_the_venue_has_one() {
    let handle = spawn_core_multi(engine(VENUE, "BTCUSDT"), vec![], core_config());
    let (only_client, calls) = client(vec![pos("BTCUSDT", 4.0)]);
    let driver = vike_core::spawn_recon(
        &handle,
        vec![ReconLeg::sole_account_of(VENUE, only_client)],
        recon_config(),
        None,
    );

    let snap = settle(&handle, |s| size_at(s, 0, "BTCUSDT") != 0.0);
    assert_eq!(calls.load(Ordering::Relaxed), 1, "one leg ⇒ exactly one startup pass");
    assert!(
        (size_at(&snap, 0, "BTCUSDT") - 4.0).abs() < 1e-12,
        "a sole account `None` pass folds, unchanged; got {}",
        size_at(&snap, 0, "BTCUSDT")
    );

    driver.shutdown();
    handle.shutdown_and_join();
}

/// **The assembly step is a NO-OP on a single-account box, and a stamp on a shared venue** — the
/// pure half, so the refusal's byte-identity claim is a property of the function rather than of a
/// caller's care.
#[test]
fn the_assembly_step_names_only_accounts_of_a_shared_venue() {
    let stub = || client(vec![]).0;
    // One account each of two venues: nothing is stamped.
    let solo = assembled(vec![
        ReconLeg::sole_account_of(VENUE, stub()),
        ReconLeg::sole_account_of("bybit", stub()),
    ]);
    assert!(
        solo.iter().all(|l| l.route_key.is_none()),
        "a venue with one account keeps the payload — and the journal bytes — it always had"
    );

    // Two accounts of one venue: the DEFAULT account's leg is stamped too, which is the half
    // neither constructor could see.
    let shared = assembled(vec![
        ReconLeg::sole_account_of(VENUE, stub()),
        ReconLeg::account(VENUE, ALT_ROUTE, stub()),
        ReconLeg::sole_account_of("bybit", stub()),
    ]);
    assert_eq!(
        shared[0].route_key.as_deref(),
        Some(VENUE),
        "the default account of a SHARED venue names itself"
    );
    assert_eq!(
        shared[1].route_key.as_deref(),
        Some(ALT_ROUTE),
        "…and the labelled one is untouched"
    );
    assert_eq!(shared[2].route_key, None, "…while the unshared venue beside them is not stamped");
}

/// **PROOF 9 — THE TWO GUARDS COUNT THE SAME SET.** A venue with TWO ENGINES and ONE reconcile
/// leg still reconciles the account whose leg survived.
///
/// # The shape, and why it is a real deployment rather than a contrivance
///
/// `vike_run::mount_accounts_of` pushes `AccountExtras::engines` UNCONDITIONALLY and
/// `AccountExtras::recon` only `if let Some(rc) = extra_recon`, so "an account that mounted an
/// engine and produced no reconcile client" is permitted BY CONSTRUCTION — and every arm that can
/// produce it is an ordinary runtime outcome, not a misconfiguration: a cTrader or IBKR account
/// whose synchronous connect failed at mount (the venue demotes to paper, the engine stays, the
/// recon handle is dropped for the session), or an IG / OANDA / deribit account whose dedicated
/// recon handshake returned `None` while its exec client spawned live.
///
/// # What was wrong
///
/// The stamping step counted LEGS and the Class E refusal counts ENGINES. With two engines and one
/// leg the stamper saw a single binance leg, stamped nothing, and the fold thread then read two
/// binance engines and REFUSED the payload for want of a stamp — so the account that was still
/// reconciling correctly folded NOTHING, every interval, for the life of the process, while an
/// `error!` line said the pass "names no account". A live refusal of a legitimate pass, reachable
/// on any box whose `[accounts]` table names a second account of a venue with a fallible recon
/// handshake.
///
/// ⚠ The call-count assertion is the "nothing ran" guard, and the position assertion is the
/// verdict: a refused pass and a driver that never started are both an empty book.
#[test]
fn a_venue_whose_second_account_has_no_recon_client_still_reconciles_the_first() {
    // TWO engines of ONE exchange — the multi-account shape, pinned by
    // `two_engines_of_one_venue_are_two_accounts_not_two_venues`.
    let primary = engine(VENUE, "BTCUSDT");
    let alt = engine(ALT_ROUTE, "ETHUSDT");
    // The ENGINE set, spelled from the engines themselves before they move into the core — exactly
    // what `vike_run::build_node_inner` hands the assembly step.
    let engine_venues: Vec<String> = vec![primary.venue.clone(), alt.venue.clone()];
    let engine_venues: Vec<&str> = engine_venues.iter().map(String::as_str).collect();
    assert_eq!(engine_venues, vec![VENUE, VENUE], "the fixture must be TWO engines of one venue");

    let handle = spawn_core_multi(primary, vec![(10_000.0, alt)], core_config());

    // …and ONE leg: the DEFAULT account's. The second account built no `ReconClient`, so
    // `mount_accounts_of` pushed its engine and no leg beside it.
    let (default_client, calls) = client(vec![pos("BTCUSDT", 6.0)]);
    let driver = vike_core::spawn_recon(
        &handle,
        assembled_over(vec![ReconLeg::sole_account_of(VENUE, default_client)], &engine_venues),
        recon_config(), // Synthesize — a fold is VISIBLE, and a refusal is visible as its absence
        None,
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while calls.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "the surviving leg RAN — its payload reached the core"
    );

    let snap =
        settle_slow(&handle, |s| size_at(s, 0, "BTCUSDT") != 0.0 || s.recon.last_pass_ts > 0);
    assert!(
        (size_at(&snap, 0, "BTCUSDT") - 6.0).abs() < 1e-12,
        "the DEFAULT account's own pass must fold into its own book — it was NOT refused for want \
         of a stamp; got {}",
        size_at(&snap, 0, "BTCUSDT")
    );
    assert_eq!(
        size_at(&snap, 1, "ETHUSDT"),
        0.0,
        "…and the account that produced no reconcile leg is untouched, as it must be"
    );

    driver.shutdown();
    handle.shutdown_and_join();
}

/// **The predicate itself, on the one input where LEGS and ENGINES disagree** — the pure half of
/// PROOF 9, so the fix is a property of the function rather than of one driver fixture.
///
/// Three rows, and each is a different answer: a venue with two ENGINES and one leg is STAMPED
/// (the defect), a venue with one engine and one leg is NOT (the byte-identity claim every
/// deployment running today depends on), and a leg naming a venue with NO engine is not either —
/// nothing can address it, and the fold thread's no-engine note is what answers such a pass.
#[test]
fn the_predicate_counts_engines_not_legs() {
    let stub = || client(vec![]).0;
    let stamped = assembled_over(
        vec![
            ReconLeg::sole_account_of(VENUE, stub()),
            ReconLeg::sole_account_of("bybit", stub()),
            ReconLeg::sole_account_of("okx", stub()),
        ],
        // binance runs TWO engines and is reconciled by one leg; bybit runs one of each; okx runs
        // no engine at all.
        &[VENUE, VENUE, "bybit"],
    );
    assert_eq!(
        stamped[0].route_key.as_deref(),
        Some(VENUE),
        "a venue with two ENGINES names its default account even though it has ONE leg"
    );
    assert_eq!(
        stamped[1].route_key, None,
        "…a venue with one engine keeps the payload — and the journal bytes — it always had"
    );
    assert_eq!(
        stamped[2].route_key, None,
        "…and a leg for a venue with NO engine is untouched: nothing can address it"
    );
}

/// **THE BYTE-IDENTITY CLAIM, stated as the property the branch promised rather than asserted.**
/// On a box with no `[accounts]` table every venue has exactly ONE engine, so the assembly step
/// stamps NOTHING whatever the legs look like — which is what keeps `route_key: None` on the wire
/// and in the journal for every deployment running today, and what makes the Class E refusal
/// structurally unreachable there (`engines_of_venue` answers 1).
///
/// Driven over the ten venues `vike_run::build_node_inner` assembles by default, so it is the real
/// single-account shape and not a two-row convenience.
#[test]
fn one_engine_per_venue_stamps_nothing_at_all() {
    let stub = || client(vec![]).0;
    let venues = [
        "binance",
        "bybit",
        "okx",
        "hyperliquid",
        "aster",
        "deribit",
        "alpaca",
        "ctrader",
        "ig",
        "oanda",
    ];
    let legs: Vec<ReconLeg> =
        venues.iter().map(|v| ReconLeg::sole_account_of(*v, stub())).collect();
    let before: Vec<(String, Option<String>)> =
        legs.iter().map(|l| (l.venue.clone(), l.route_key.clone())).collect();

    let after = assembled_over(legs, &venues);

    let now: Vec<(String, Option<String>)> =
        after.iter().map(|l| (l.venue.clone(), l.route_key.clone())).collect();
    assert_eq!(now, before, "one engine per venue ⇒ the legs are byte-identical after assembly");
    assert!(
        after.iter().all(|l| l.route_key.is_none()),
        "…and every one of them still carries the `None` a single-account box has always sent"
    );
}
