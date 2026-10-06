use super::*;
use vike_model::events::{FillEvent, LiquiditySide, PositionSide, TradeId};

/// One reconcile pass's worth of clock (the `VIKE_RECONCILE_INTERVAL_MS` default).
const PASS_MS: i64 = 60_000;

/// A synthesized position leg exactly as `vike_exec::recon::resolve`'s `synth_position_legs`
/// mints one: the `trade_id` and the `client_order_id` BAKE IN the pass clock, so every pass
/// produces different ones for an unchanged position. That churn is the defect's engine and is
/// reproduced here on purpose.
fn ext_pos_leg(venue: &str, symbol: &str, qty: f64, ts: i64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::prefixed("EXT-POS-", format_args!("{venue}-{symbol}-{ts}-0")),
        client_order_id: format!("EXT-{venue}-POS-{symbol}-{ts}-0"),
        venue: ustr::ustr(venue),
        symbol: ustr::ustr(symbol),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: ustr::ustr(""),
        liquidity_side: LiquiditySide::Unknown,
        ts,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

/// The the CI box alert: `PositionOnlyExternal`, no dedup key, detail = the kind name — on a venue
/// with ONE account, so its account key is its venue id. That is the shape every test below
/// except the per-account ones is about.
fn ext_pos_id(venue: &str, symbol: &str, qty: f64, ts: i64) -> HeldId {
    ext_pos_id_at(venue, venue, symbol, qty, ts)
}

/// …and the same alert raised by a NAMED account of that venue.
fn ext_pos_id_at(venue: &str, account: &str, symbol: &str, qty: f64, ts: i64) -> HeldId {
    HeldId::new(
        venue,
        account,
        DivergenceKind::PositionOnlyExternal,
        None,
        "PositionOnlyExternal",
        &[ext_pos_leg(venue, symbol, qty, ts)],
    )
}

fn set(ids: impl IntoIterator<Item = HeldId>) -> BTreeSet<HeldId> {
    ids.into_iter().collect()
}

/// THE regression pin for the identity choice: the same unchanged external position, resolved
/// on two different passes, is ONE identity — even though its synthesized `trade_id`, its
/// `client_order_id` and its `ts` all differ. Keying on any of those would re-announce forever,
/// which is the measured the CI box defect.
#[test]
fn an_unchanged_external_position_keeps_one_identity_across_passes() {
    let a = ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000);
    let b = ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000 + PASS_MS);
    assert_eq!(a, b, "ts/trade_id/coid churn must not create a second identity");
    assert!(a.instance.contains("BTCUSDT"), "the symbol must be IN the key: {}", a.instance);
    assert!(!a.instance.contains("EXT-POS-"), "the ts-baked trade id must NOT be: {}", a.instance);
}

/// The measured defect, as an assertion: 60 passes of the identical bybit
/// `PositionOnlyExternal` produce ONE announcement, not 60.
#[test]
fn the_same_divergence_held_across_many_passes_announces_once() {
    let mut a = HeldAnnouncer::default();
    let mut announced = 0usize;
    let mut silent_passes = 0usize;
    for pass in 0..60 {
        let ts = 1_000 + pass * PASS_MS;
        let t = a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts)]));
        announced += t.newly_held.len();
        if t.silent() {
            silent_passes += 1;
        }
        assert!(t.cleared.is_empty(), "nothing cleared on pass {pass}");
    }
    assert_eq!(announced, 1, "one divergence, one announcement");
    assert_eq!(silent_passes, 59, "every later pass says nothing");
    assert_eq!(a.held("bybit"), 1);
}

/// A NEW divergence is announced on the pass it appears, and only that one — the already-held
/// neighbour stays quiet. This is the signal the old per-pass repetition buried.
#[test]
fn a_new_divergence_announces_immediately_and_alone() {
    let mut a = HeldAnnouncer::default();
    let first = a.observe("bybit", 1_000, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000)]));
    assert_eq!(first.newly_held.len(), 1);

    // ...many quiet passes...
    for pass in 1..10 {
        let ts = 1_000 + pass * PASS_MS;
        assert!(a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts)])).silent());
    }

    let ts = 1_000 + 10 * PASS_MS;
    let t = a.observe(
        "bybit",
        ts,
        set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts), ext_pos_id("bybit", "ETHUSDT", 3.0, ts)]),
    );
    assert_eq!(t.newly_held.len(), 1, "only the newcomer");
    let new = t.newly_held.iter().next().expect("one");
    assert!(new.instance.contains("ETHUSDT"), "the ETH leg is the news: {}", new.instance);
    assert_eq!(a.held("bybit"), 2);
}

/// Two same-kind divergences that differ ONLY in symbol must not collapse: the un-keyed alert's
/// `detail` is the bare kind name, so the instrument legs are the only thing keeping them apart.
#[test]
fn two_divergences_differing_only_in_symbol_do_not_collapse() {
    let btc = ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000);
    let eth = ext_pos_id("bybit", "ETHUSDT", 0.5, 1_000);
    assert_ne!(btc, eth);
    let mut a = HeldAnnouncer::default();
    let t = a.observe("bybit", 1_000, set([btc, eth]));
    assert_eq!(t.newly_held.len(), 2, "both announced");
    assert_eq!(a.held("bybit"), 2);
}

/// An external position that changes SIZE is a real state change, so it announces again.
#[test]
fn a_changed_position_size_is_a_new_divergence() {
    let mut a = HeldAnnouncer::default();
    assert_eq!(
        a.observe("bybit", 1_000, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000)]))
            .newly_held
            .len(),
        1
    );
    let ts = 1_000 + PASS_MS;
    let t = a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.9, ts)]));
    assert_eq!(t.newly_held.len(), 1, "0.5 -> 0.9 is news");
    assert_eq!(t.cleared.len(), 1, "...and the old size is gone");
    assert_eq!(a.held("bybit"), 1, "one live divergence, not two");
}

/// A divergence the venue stops reporting is OBSERVABLE — the transition an operator had no way
/// to see at all before, since the held-alert store never self-clears.
#[test]
fn a_divergence_that_clears_is_announced_once_and_then_forgotten() {
    let mut a = HeldAnnouncer::default();
    a.observe("bybit", 1_000, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000)]));
    let t = a.observe("bybit", 1_000 + PASS_MS, BTreeSet::new());
    assert_eq!(t.cleared.len(), 1, "the clear is announced");
    assert!(t.newly_held.is_empty());
    assert_eq!(a.held("bybit"), 0, "and the row is pruned");
    let quiet = a.observe("bybit", 1_000 + 2 * PASS_MS, BTreeSet::new());
    assert!(quiet.silent(), "a clear is announced ONCE, not every pass thereafter");
}

/// The summary fires on its OWN cadence, not the pass cadence: 24h of one-minute passes over an
/// unchanging held set produces one line per [`HELD_SUMMARY_INTERVAL_MS`], and the line names
/// what is waiting.
#[test]
fn the_summary_fires_on_its_own_cadence_not_the_pass_cadence() {
    let mut a = HeldAnnouncer::default();
    let day = 24 * 60 * 60 * 1_000;
    let passes = day / PASS_MS; // 1440
    let mut summaries = Vec::new();
    for pass in 0..passes {
        let ts = 1_000 + pass * PASS_MS;
        let t = a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts)]));
        if let Some(s) = t.summary {
            summaries.push(s);
        }
    }
    let expected = (day / HELD_SUMMARY_INTERVAL_MS) as usize;
    assert_eq!(expected, 24, "sanity: an hourly summary over a day");
    assert_eq!(summaries.len(), expected - 1, "one per interval after the raise re-armed it");
    assert!(!summaries.is_empty(), "a summary DID fire");
    let first = &summaries[0];
    assert_eq!(first.held, 1);
    assert_eq!(first.kinds, "PositionOnlyExternal x1");
    assert!(first.quiet_ms >= HELD_SUMMARY_INTERVAL_MS, "quiet_ms = {}", first.quiet_ms);
    // The whole point, as a rate: 1440 passes, 24 lines.
    assert!(
        summaries.len() * 50 < passes as usize,
        "the summary must be an order of magnitude quieter than the pass cadence"
    );
}

/// A transition RE-ARMS the summary clock — a summary must never land right behind a line that
/// just said the same thing.
#[test]
fn a_transition_rearms_the_summary_clock() {
    let mut a = HeldAnnouncer::default();
    a.observe("bybit", 0, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 0)]));
    // One tick short of the interval: nothing.
    let ts = HELD_SUMMARY_INTERVAL_MS - 1;
    assert!(a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts)])).silent());
    // A new divergence lands (a transition) — and that re-arms the clock...
    let ts = HELD_SUMMARY_INTERVAL_MS;
    let t = a.observe(
        "bybit",
        ts,
        set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts), ext_pos_id("bybit", "ETHUSDT", 1.0, ts)]),
    );
    assert_eq!(t.newly_held.len(), 1);
    assert!(t.summary.is_none(), "the transition IS the line; no summary behind it");
    // ...so the next summary is an interval after the TRANSITION, not after the first raise.
    let ts = 2 * HELD_SUMMARY_INTERVAL_MS - 1;
    let t = a.observe(
        "bybit",
        ts,
        set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts), ext_pos_id("bybit", "ETHUSDT", 1.0, ts)]),
    );
    assert!(t.silent(), "one tick short of the interval after the transition");
    let ts = 2 * HELD_SUMMARY_INTERVAL_MS;
    let t = a.observe(
        "bybit",
        ts,
        set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts), ext_pos_id("bybit", "ETHUSDT", 1.0, ts)]),
    );
    let s = t.summary.expect("the summary is due now");
    assert_eq!(s.held, 2);
    assert_eq!(s.kinds, "PositionOnlyExternal x2");
}

/// A backwards core clock (an event-driven `now_ms` on a venue that resumed behind its own last
/// pass) re-arms rather than firing — a clock jump must not become per-pass noise.
#[test]
fn a_backwards_clock_rearms_instead_of_firing() {
    let mut a = HeldAnnouncer::default();
    let far = 10 * HELD_SUMMARY_INTERVAL_MS;
    a.observe("bybit", far, set([ext_pos_id("bybit", "BTCUSDT", 0.5, far)]));
    let t = a.observe("bybit", 0, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 0)]));
    assert!(t.silent(), "backwards clock says nothing");
    let t = a.observe("bybit", 1, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 1)]));
    assert!(t.silent(), "...and the clock is re-armed from the new reading");
}

/// Venues do not share held state: a bybit divergence cannot silence a binance one, and a
/// binance pass cannot clear bybit's set.
#[test]
fn two_venues_do_not_share_held_state() {
    let mut a = HeldAnnouncer::default();
    assert_eq!(
        a.observe("bybit", 1_000, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000)]))
            .newly_held
            .len(),
        1
    );
    let t = a.observe("binance", 1_000, set([ext_pos_id("binance", "BTCUSDT", 0.5, 1_000)]));
    assert_eq!(t.newly_held.len(), 1, "a different venue's identical shape is its own news");
    assert!(t.cleared.is_empty(), "and it does not clear bybit's");
    assert_eq!(a.held("bybit"), 1);
    assert_eq!(a.held("binance"), 1);
}

/// An operator confirm forgets the identity, so a divergence the confirm did NOT resolve
/// announces again on the next pass instead of re-appearing in silence.
#[test]
fn a_confirmed_divergence_reannounces_if_the_next_pass_still_reports_it() {
    let mut a = HeldAnnouncer::default();
    let id = ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000);
    assert_eq!(a.observe("bybit", 1_000, set([id.clone()])).newly_held.len(), 1);
    let ts = 1_000 + PASS_MS;
    assert!(a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts)])).silent());

    a.forget(&id);
    assert_eq!(a.held("bybit"), 0);

    let ts = 1_000 + 2 * PASS_MS;
    let t = a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts)]));
    assert_eq!(t.newly_held.len(), 1, "the confirm did not take — say so");
}

/// A `dedup_key`, where `resolve` supplies one, IS the identity: `vike_exec` has already
/// declared what makes that divergence the same one recurring.
#[test]
fn a_dedup_keyed_alert_keys_on_the_dedup_key() {
    let a = HeldId::new(
        "bybit",
        "bybit",
        DivergenceKind::OrphanLocalPosition,
        Some("position:BTCUSDT:BOTH"),
        "local bybit BTCUSDT BOTH position 0.5 has NO venue position row this pass",
        &[],
    );
    let b = HeldId::new(
        "bybit",
        "bybit",
        DivergenceKind::OrphanLocalPosition,
        Some("position:BTCUSDT:BOTH"),
        // A later pass phrases the detail differently — same divergence.
        "local bybit BTCUSDT BOTH position 0.6 has NO venue position row this pass",
        &[],
    );
    assert_eq!(a, b, "the dedup key decides, not the prose");
    let other = HeldId::new(
        "bybit",
        "bybit",
        DivergenceKind::OrphanLocalPosition,
        Some("position:ETHUSDT:BOTH"),
        "local bybit ETHUSDT BOTH position 1.0 has NO venue position row this pass",
        &[],
    );
    assert_ne!(a, other, "a different keyed instance is a different divergence");
}

/// ...and a keyed alert whose PAYLOAD changes is still ONE identity. This is what makes the
/// identity a strict generalization of the `(venue, kind, dedup_key)` match the held-alert
/// store used before: appending legs to a supplied key could SPLIT a row `resolve` meant to be
/// one (an `UnknownOrder`'s adoption fill, say), which would be a behaviour regression wearing
/// a logging change's clothes.
#[test]
fn a_dedup_keyed_alert_ignores_its_legs() {
    let bare =
        HeldId::new("bybit", "bybit", DivergenceKind::UnknownOrder, Some("v-9"), "unknown", &[]);
    let with_fill = HeldId::new(
        "bybit",
        "bybit",
        DivergenceKind::UnknownOrder,
        Some("v-9"),
        "unknown",
        &[ext_pos_leg("bybit", "BTCUSDT", 4.0, 77)],
    );
    assert_eq!(bare, with_fill, "the supplied key answers alone");
    assert_eq!(bare.instance, "v-9", "...and is the whole instance");
}

/// A `MissingFill` leg carrying the VENUE's own trade id.
fn missed_fill_id(trade_id: &'static str, qty: f64, ts: i64) -> HeldId {
    let leg = Event::Fill(FillEvent {
        trade_id: TradeId::from(trade_id),
        client_order_id: "EXT-bybit-v9".into(),
        venue: ustr::ustr("bybit"),
        symbol: ustr::ustr("BTCUSDT"),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: ustr::ustr(""),
        liquidity_side: LiquiditySide::Taker,
        ts,
        mark_price: None,
        position_side: PositionSide::Both,
    });
    HeldId::new("bybit", "bybit", DivergenceKind::MissingFill, None, "MissingFill", &[leg])
}

/// ⚠ THE OTHER DIRECTION of the identity choice, and the expensive one to get wrong. Two
/// genuinely different missed fills that agree on symbol, side and SIZE are told apart only by
/// the venue's trade id. Collapsing them would let an operator confirm one and silently lose
/// the other's fill — caught for real by
/// `crates/vike-core/tests/recon/recon_quarantine.rs`'s
/// `recurring_unknown_order_alert_dedupes_and_clears_after_confirm`, whose `t1`/`t2` differ in
/// nothing else.
#[test]
fn two_missed_fills_differing_only_in_trade_id_do_not_collapse() {
    assert_ne!(missed_fill_id("t1", 1.0, 5), missed_fill_id("t2", 1.0, 5));
}

/// ...while the SAME missed fill, re-diffed pass after pass because quarantine never folds it,
/// stays one identity — its venue trade id does not move even though its pass does.
#[test]
fn the_same_missed_fill_recurring_keeps_one_identity() {
    assert_eq!(missed_fill_id("t1", 1.0, 5), missed_fill_id("t1", 1.0, 5));
}

/// The classification is per KIND and both answers are load-bearing — stated directly so the
/// reason survives even if the two behavioural tests above are ever rewritten.
#[test]
fn trade_id_stability_is_classified_per_kind() {
    assert!(trade_ids_are_stable(DivergenceKind::MissingFill), "the venue's own execution id");
    assert!(trade_ids_are_stable(DivergenceKind::UnknownOrder), "pure fn of venue + order id");
    assert!(
        !trade_ids_are_stable(DivergenceKind::PositionOnlyExternal),
        "synth_position_legs bakes the pass clock into it"
    );
    assert!(!trade_ids_are_stable(DivergenceKind::PositionDrift), "…and into this one's too");
}

/// Two different KINDS on one venue never collapse, however similar their instance text.
#[test]
fn two_kinds_never_collapse() {
    let a = HeldId::new("bybit", "bybit", DivergenceKind::PositionOnlyExternal, None, "x", &[]);
    let b = HeldId::new("bybit", "bybit", DivergenceKind::PositionDrift, None, "x", &[]);
    assert_ne!(a, b);
}

// ---------------------------------------------------------------------------------------
// FIFTY ACCOUNTS OF ONE EXCHANGE — the ruled scale. The integration twin (a real core, real
// engines, a real driver) is `crates/vike-core/tests/recon/recon_per_account.rs`; these are
// the pure ones, and they are where the erasure is visible as ARITHMETIC.
// ---------------------------------------------------------------------------------------

/// The number the owner ruled the design must hold at: *"it can be 50 accounts per venue."*
const FIFTY: usize = 50;

/// `binance#A00` … `binance#A49` — what `vike_mount::account_route_key` renders for a labelled
/// account, and what that account's `ExecutionEngine::route_key` carries.
fn acct(i: usize) -> String {
    format!("binance#A{i:02}")
}

/// **THE MAJOR, as arithmetic.** One pass over fifty accounts of one exchange, each holding
/// its own divergence, and after the whole pass ALL FIFTY are still held — each under its own
/// account key.
///
/// ⚠ Two accounts cannot distinguish "merged" from "erased" and fifty can: keyed by venue,
/// `observe` replaces the venue's whole set with each leg's, so the pass ends with ONE account
/// held (the last) and 49 erased — and each leg's call reports the previous leg's set as
/// CLEARED, i.e. as the good-news "no longer reported" line, for divergences that are still
/// held and still waiting for somebody.
#[test]
fn fifty_accounts_of_one_venue_are_fifty_held_sets_not_one() {
    let mut a = HeldAnnouncer::default();
    let mut announced = 0usize;
    let mut cleared = 0usize;
    for i in 0..FIFTY {
        let t = a.observe(
            &acct(i),
            1_000,
            set([ext_pos_id_at("binance", &acct(i), "BTCUSDT", 0.5, 1_000)]),
        );
        announced += t.newly_held.len();
        cleared += t.cleared.len();
    }
    assert_eq!(announced, FIFTY, "each account's divergence is its own news");
    assert_eq!(cleared, 0, "NOTHING cleared — no account's backlog was erased by its neighbour");
    for i in 0..FIFTY {
        assert_eq!(a.held(&acct(i)), 1, "account {i} must still hold its own divergence");
    }
}

/// …and it STAYS held and STAYS quiet: sixty passes over fifty accounts announce the fifty
/// once and say nothing at all thereafter. Keyed by venue this is 50 WARNs + 50 "cleared"
/// INFOs per pass, forever — the measured 396-lines-a-day defect multiplied by the account
/// count, wearing the fix's own clothes.
#[test]
fn fifty_accounts_holding_a_steady_backlog_announce_once_each_and_then_go_quiet() {
    let mut a = HeldAnnouncer::default();
    let mut announced = 0usize;
    let mut cleared = 0usize;
    let mut noisy_legs = 0usize;
    for pass in 0..60i64 {
        let ts = 1_000 + pass * PASS_MS;
        for i in 0..FIFTY {
            let t = a.observe(
                &acct(i),
                ts,
                set([ext_pos_id_at("binance", &acct(i), "BTCUSDT", 0.5, ts)]),
            );
            announced += t.newly_held.len();
            cleared += t.cleared.len();
            if !t.silent() {
                noisy_legs += 1;
            }
        }
    }
    assert_eq!(announced, FIFTY, "fifty divergences, fifty announcements — not 50 per pass");
    assert_eq!(cleared, 0, "and not one spurious clear across 3,000 legs");
    assert_eq!(noisy_legs, FIFTY, "only the raising pass of each account says anything");
}

/// **The dedup key names an INSTRUMENT, never a book.** Fifty accounts all holding
/// `position:BTCUSDT:Both` are fifty identities, because the account is above the key. Without
/// it they are ONE, and the store's identity match then refreshes the first account's row with
/// the fiftieth account's payload while its `route_key` still points at the first — which is
/// how a confirm folds the wrong account's fills.
#[test]
fn fifty_accounts_sharing_one_dedup_key_are_fifty_identities() {
    let ids: BTreeSet<HeldId> = (0..FIFTY)
        .map(|i| {
            HeldId::new(
                "binance",
                &acct(i),
                DivergenceKind::OrphanLocalPosition,
                Some("position:BTCUSDT:BOTH"),
                "local binance BTCUSDT BOTH position 0.5 has NO venue position row this pass",
                &[],
            )
        })
        .collect();
    assert_eq!(ids.len(), FIFTY, "one identity per account, not one for the exchange");
    // …and each names its own account while every one of them names the same exchange.
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(id.venue, "binance", "the LABEL half stays canonical");
        assert_eq!(id.account, acct(i), "…and the KEY half names the book");
    }
}

/// A confirm on one of fifty accounts forgets THAT account's identity and leaves the other 49
/// untouched — so the next pass re-announces the one that did not resolve, and stays silent
/// about the rest.
#[test]
fn a_confirm_on_one_of_fifty_accounts_forgets_only_that_account() {
    let mut a = HeldAnnouncer::default();
    let id_of = |i: usize, ts: i64| ext_pos_id_at("binance", &acct(i), "BTCUSDT", 0.5, ts);
    for i in 0..FIFTY {
        a.observe(&acct(i), 1_000, set([id_of(i, 1_000)]));
    }
    a.forget(&id_of(7, 1_000));
    assert_eq!(a.held(&acct(7)), 0, "the confirmed account's row is gone");
    for i in (0..FIFTY).filter(|&i| i != 7) {
        assert_eq!(a.held(&acct(i)), 1, "account {i} is untouched by account 7's confirm");
    }

    let ts = 1_000 + PASS_MS;
    let mut reannounced = 0usize;
    for i in 0..FIFTY {
        reannounced += a.observe(&acct(i), ts, set([id_of(i, ts)])).newly_held.len();
    }
    assert_eq!(reannounced, 1, "only the account whose confirm did not take says anything");
}

/// The periodic backlog summary SURVIVES fifty accounts — it fires per account on its own
/// cadence. Keyed by venue it can never fire at all, because every leg of every pass changes
/// the venue's set and re-arms the clock.
#[test]
fn the_summary_still_fires_at_fifty_accounts() {
    let mut a = HeldAnnouncer::default();
    let mut summaries = 0usize;
    // Two summary intervals' worth of one-minute passes.
    let passes = (2 * HELD_SUMMARY_INTERVAL_MS) / PASS_MS;
    for pass in 0..passes {
        let ts = 1_000 + pass * PASS_MS;
        for i in 0..FIFTY {
            let t = a.observe(
                &acct(i),
                ts,
                set([ext_pos_id_at("binance", &acct(i), "BTCUSDT", 0.5, ts)]),
            );
            if t.summary.is_some() {
                summaries += 1;
            }
        }
    }
    assert_eq!(summaries, FIFTY, "one summary per account over the interval, and no more");
}

/// `render_kinds` counts per kind rather than listing rows.
#[test]
fn the_summary_names_what_is_waiting() {
    let held = set([
        ext_pos_id("bybit", "BTCUSDT", 0.5, 1),
        ext_pos_id("bybit", "ETHUSDT", 1.0, 1),
        HeldId::new("bybit", "bybit", DivergenceKind::MissingFill, None, "MissingFill", &[]),
    ]);
    assert_eq!(render_kinds(&held), "MissingFill x1, PositionOnlyExternal x2");
}
