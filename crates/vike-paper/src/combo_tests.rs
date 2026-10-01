use super::*;
use vike_model::{ComboSpec, build_combo};

/// A bar carrying an explicit bid/ask for `symbol` — combos price legs off the quote side they
/// would actually trade, so the spread has to be expressible.
fn quoted(ts: i64, symbol: &str, bid: f64, ask: f64) -> Bar {
    Bar {
        ts,
        open: bid,
        high: ask,
        low: bid,
        close: (bid + ask) / 2.0,
        volume: 0.0,
        funding: None,
        bid: Some(bid),
        ask: Some(ask),
        symbol: Some(symbol.to_string()),
    }
}

fn client() -> PaperExecutionClient {
    PaperExecutionClient::new("deribit", "COMBO", 0.0, 0.0, 0.0)
}

/// A two-leg vertical: buy `NEAR`, sell `FAR`, one unit.
fn spread(side: i32, net_limit: Option<f64>) -> ComboSpec {
    ComboSpec {
        venue: "deribit".into(),
        side,
        qty: 1.0,
        legs: vec![
            ComboLeg { symbol: "NEAR".into(), ratio: 1 },
            ComboLeg { symbol: "FAR".into(), ratio: -1 },
        ],
        net_limit,
        time_in_force: TimeInForce::Gtc,
    }
}

fn submit_combo(c: &mut PaperExecutionClient, coid: &str, spec: &ComboSpec) {
    c.submit(&build_combo(spec, coid).expect("valid combo spec"));
}

fn fills_of(c: &PaperExecutionClient) -> Vec<PaperFill> {
    c.fills.lock().unwrap().clone()
}

fn drain(c: &mut PaperExecutionClient) -> Vec<Event> {
    std::iter::from_fn(|| c.poll_events()).collect()
}

/// A DEBIT spread (net > 0): buying it fills once the achievable net reaches the limit, and
/// each leg fills at its OWN price — the net is only the trigger.
#[test]
fn buying_a_debit_spread_fills_every_leg_at_its_own_price() {
    let mut c = client();
    // buy NEAR at its ask, sell FAR at its bid => net = 100 - 94 = 6, limit 6 => crosses.
    submit_combo(&mut c, "cb", &spread(1, Some(6.0)));
    c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    assert!(fills_of(&c).is_empty(), "one leg marked is not enough to fill anything");
    c.on_bar(&quoted(1, "FAR", 94.0, 95.0));

    let fills = fills_of(&c);
    assert_eq!(fills.len(), 2, "both legs filled");
    assert_eq!((fills[0].side, fills[0].qty, fills[0].px), (1, 1.0, 100.0), "buy NEAR at ask");
    assert_eq!((fills[1].side, fills[1].qty, fills[1].px), (-1, 1.0, 94.0), "sell FAR at bid");
    // and crucially NOT one blended fill at the net
    assert!(fills.iter().all(|f| f.px != 6.0), "no synthetic fill at the net price");
}

/// The defining property: a combo fills ALL legs or NONE. One tick short of the limit leaves
/// the whole structure resting — not one leg done and one working.
#[test]
fn a_combo_short_of_its_limit_fills_no_leg_at_all() {
    let mut c = client();
    submit_combo(&mut c, "cb", &spread(1, Some(5.0))); // needs net <= 5
    c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    c.on_bar(&quoted(1, "FAR", 94.0, 95.0)); // net = 6 > 5
    assert!(fills_of(&c).is_empty(), "net has not crossed => zero legs fill");
    assert_eq!(c.combos.len(), 1, "the whole combo rests on");

    // FAR bid rises to 95 => net = 5, exactly at the limit. BOTH legs print at ts=2 — under
    // the default freshness bound (0 = same-ts) a lone FAR print could not trigger against
    // NEAR's ts=1 mark (see `a_leg_that_stops_printing_stops_the_combo`).
    c.on_bar(&quoted(2, "NEAR", 99.0, 100.0));
    c.on_bar(&quoted(2, "FAR", 95.0, 96.0));
    assert_eq!(fills_of(&c).len(), 2, "all legs fill together, on the same bar");
    assert!(c.combos.is_empty(), "and the combo leaves the book");
}

/// A CREDIT spread's net is NEGATIVE. Buying one must work with a negative limit — nothing on
/// this path may clamp, abs() or sign-assume.
#[test]
fn buying_a_credit_spread_works_with_a_negative_net() {
    let mut c = client();
    // buy NEAR@10 / sell FAR@25 => net = 10 - 25 = -15 (a credit). Limit -15 => crosses.
    submit_combo(&mut c, "cr", &spread(1, Some(-15.0)));
    c.on_bar(&quoted(1, "NEAR", 9.0, 10.0));
    c.on_bar(&quoted(1, "FAR", 25.0, 26.0));
    let fills = fills_of(&c);
    assert_eq!(fills.len(), 2, "a negative net limit fills exactly like a positive one");
    assert_eq!(fills[0].px, 10.0);
    assert_eq!(fills[1].px, 25.0);
}

/// A credit combo that is not yet rich enough does NOT fill: proof the negative-side comparison
/// is a real ordered test, not "negative => always crosses".
#[test]
fn a_credit_combo_still_respects_its_limit() {
    let mut c = client();
    submit_combo(&mut c, "cr", &spread(1, Some(-20.0))); // wants net <= -20
    c.on_bar(&quoted(1, "NEAR", 9.0, 10.0));
    c.on_bar(&quoted(1, "FAR", 25.0, 26.0)); // net = -15, which is > -20
    assert!(fills_of(&c).is_empty(), "-15 does not cross a -20 buy limit");
}

/// SELLING a debit spread: the sell side flips every leg AND flips the crossing direction
/// (`net >= limit`), and each leg trades the opposite side of the book.
#[test]
fn selling_a_debit_spread_flips_every_leg_and_the_crossing_test() {
    let mut c = client();
    // sell the spread: sell NEAR at its BID (99), buy FAR at its ASK (95) => net = 99 - 95 = 4.
    submit_combo(&mut c, "sd", &spread(-1, Some(4.0))); // sell fills when net >= 4
    c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
    let fills = fills_of(&c);
    assert_eq!(fills.len(), 2);
    assert_eq!((fills[0].side, fills[0].px), (-1, 99.0), "the +1 leg SELLS at the bid");
    assert_eq!((fills[1].side, fills[1].px), (1, 95.0), "the -1 leg BUYS at the ask");
}

/// A combo is ONE order downstream: one coid, exactly one terminal event, with the earlier legs
/// wrapped as partials — the `ManagedOrder` FSM law.
#[test]
fn a_combo_emits_one_terminal_event_for_its_single_coid() {
    let mut c = client();
    submit_combo(&mut c, "one", &spread(1, Some(6.0)));
    c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
    let evs = drain(&mut c);

    let bare: Vec<&FillEvent> = evs
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(bare.len(), 2, "one bare Fill per leg (Account folds these)");
    assert_eq!(bare[0].symbol.as_str(), "NEAR", "fills carry the LEG symbol");
    assert_eq!(bare[1].symbol.as_str(), "FAR");
    assert!(bare.iter().all(|f| f.client_order_id == "one"), "all under the one combo coid");

    let partials = evs.iter().filter(|e| matches!(e, Event::OrderPartiallyFilled(_))).count();
    let terminal = evs.iter().filter(|e| matches!(e, Event::OrderFilled(_))).count();
    assert_eq!(partials, 1, "legs 0..n-1 wrap as partials");
    assert_eq!(terminal, 1, "exactly ONE terminal event for the combo");
}

/// Leg RATIOS scale each leg's quantity (`|ratio| x units`) and weight its price in the net.
#[test]
fn ratios_scale_leg_quantities_and_weight_the_net() {
    let mut c = client();
    // a 1x2 ratio spread, 3 units: buy 1xNEAR, sell 2xFAR => net = 100 - 2*94 = -88.
    let spec = ComboSpec {
        venue: "deribit".into(),
        side: 1,
        qty: 3.0,
        legs: vec![
            ComboLeg { symbol: "NEAR".into(), ratio: 1 },
            ComboLeg { symbol: "FAR".into(), ratio: -2 },
        ],
        net_limit: Some(-88.0),
        time_in_force: TimeInForce::Gtc,
    };
    submit_combo(&mut c, "rr", &spec);
    c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
    let fills = fills_of(&c);
    assert_eq!(fills.len(), 2);
    assert_eq!((fills[0].qty, fills[0].side), (3.0, 1), "|1| x 3 units");
    assert_eq!((fills[1].qty, fills[1].side), (6.0, -1), "|-2| x 3 units");
}

/// A combo MARKET (no net limit) has no crossing trigger — it fills as soon as every leg
/// carries a FRESH mark (the freshness gate applies to market combos too; see
/// `a_filled_combos_marks_never_resurrect_for_a_later_combo` for the stale side).
#[test]
fn a_market_combo_fills_once_every_leg_is_marked() {
    let mut c = client();
    submit_combo(&mut c, "mk", &spread(1, None));
    c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    assert!(fills_of(&c).is_empty(), "still missing a leg mark");
    c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
    assert_eq!(fills_of(&c).len(), 2, "no crossing test — fills on the mark");
    assert!(fills_of(&c).iter().all(|f| !f.is_maker), "a market combo books taker");
}

/// Slippage is applied per leg, adversely to that LEG's own side (the buy leg pays up, the sell
/// leg receives less) — not to the net. And the net LIMIT binds PRE-slippage: the trigger
/// compares raw quotes (raw net 6 == limit 6 fills), so the EXECUTED net lands through the
/// limit by slippage × gross — the documented convention (see `fill_combos`), identical to the
/// single-symbol path (`BarFillModel` triggers on raw bar prices, `emit_fill` slips after) and
/// to LEAN's `ComboLimitFill`. This test PINS that convention: if slippage ever moves into the
/// trigger, the fill here disappears and this fails — make that change deliberately.
#[test]
fn slippage_is_adverse_per_leg_side() {
    let mut c = PaperExecutionClient::new("deribit", "COMBO", 0.01, 0.0, 0.0);
    submit_combo(&mut c, "sl", &spread(1, Some(6.0)));
    c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
    let fills = fills_of(&c);
    assert_eq!(fills.len(), 2, "raw net 6 == limit => fills, slippage NOT in the trigger");
    assert_eq!(fills[0].px, 100.0 * 1.01, "the BUY leg slips up");
    assert_eq!(fills[1].px, 94.0 * 0.99, "the SELL leg slips down");
    // executed net = 101 - 93.06 = 7.94: through the 6 limit by slippage × gross (1.94), the
    // pre-slippage-binding law made visible.
    let executed_net = fills[0].px - fills[1].px;
    assert!((executed_net - 7.94).abs() < 1e-9, "executed net {executed_net} != 7.94");
    assert!(executed_net > 6.0, "the executed net is through the pre-slippage limit");
}

/// A resting combo cancels as ONE order — all legs at once, one cancel event.
#[test]
fn cancel_removes_the_whole_combo() {
    let mut c = client();
    submit_combo(&mut c, "cx", &spread(1, Some(6.0)));
    c.cancel("cx");
    assert!(c.combos.is_empty(), "the whole combo left the book");
    let cancels = drain(&mut c).iter().filter(|e| matches!(e, Event::OrderCanceled(_))).count();
    assert_eq!(cancels, 1, "one order, one cancel");
    c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
    assert!(fills_of(&c).is_empty(), "a canceled combo never fills");
}

/// A resting combo honours an expiring TIF, and expires as one order (no leg survives).
#[test]
fn a_combo_expires_as_one_order() {
    let mut c = client();
    let mut req = build_combo(&spread(1, Some(6.0)), "ex").expect("valid");
    req.time_in_force = TimeInForce::Gtd;
    req.gtd_expiry = Some(1_000);
    c.submit(&req);
    c.on_bar(&quoted(1_000, "NEAR", 99.0, 100.0));
    let expiries = drain(&mut c).iter().filter(|e| matches!(e, Event::OrderExpired(_))).count();
    assert_eq!(expiries, 1, "the combo expires exactly once");
    assert!(c.combos.is_empty(), "no leg left resting");
}

/// OFF-PATH BYTE-IDENTITY: an ordinary (non-combo) order records NO combo state at all, so the
/// combo path cannot perturb the r7-gated single-symbol semantics.
#[test]
fn a_non_combo_order_records_no_combo_state() {
    let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    let req = OrderRequest {
        client_order_id: "plain".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "market".into(),
        ..Default::default()
    };
    assert!(req.combo_legs.is_empty(), "the default request is not a combo");
    c.submit(&req);
    assert!(c.combos.is_empty(), "no combo recorded");
    c.on_bar(&Bar {
        ts: 1,
        open: 100.0,
        high: 101.0,
        low: 99.0,
        close: 100.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    });
    assert!(c.leg_marks.is_empty(), "no marks recorded on a combo-free run");
    assert_eq!(c.pending.len(), 0, "and the ordinary order filled as before");
    assert_eq!(fills_of(&c).len(), 1);
}

/// A bar with no explicit bid/ask falls back to its close for both sides — combos still work
/// on plain OHLC series.
#[test]
fn legs_fall_back_to_close_without_a_quote() {
    let mut c = client();
    submit_combo(&mut c, "fb", &spread(1, Some(6.0)));
    for (sym, close) in [("NEAR", 100.0), ("FAR", 94.0)] {
        c.on_bar(&Bar {
            ts: 1,
            open: close,
            high: close,
            low: close,
            close,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(sym.to_string()),
        });
    }
    let fills = fills_of(&c);
    assert_eq!(fills.len(), 2, "net = 100 - 94 = 6 crosses the 6 limit off closes alone");
    assert_eq!(fills[0].px, 100.0);
    assert_eq!(fills[1].px, 94.0);
}

/// `MultiPaperExecutionClient` has no book for a combo (empty symbol, legs across instruments),
/// so it REJECTS terminally rather than dropping or half-routing it — the dead-path rule.
#[test]
fn the_multi_router_rejects_a_combo_terminally() {
    let mut multi = MultiPaperExecutionClient::new();
    multi.add_book(PaperExecutionClient::new("deribit", "NEAR", 0.0, 0.0, 0.0));
    multi.submit(&build_combo(&spread(1, Some(6.0)), "mc").expect("valid"));
    let evs: Vec<Event> = std::iter::from_fn(|| multi.poll_events()).collect();
    assert!(
        evs.iter().any(|e| matches!(e, Event::OrderRejected(r) if r.client_order_id == "mc")),
        "an unroutable combo terminalizes as OrderRejected, never vanishes: {evs:?}"
    );
}

// ---- leg-mark freshness discipline (adversarial-review MAJOR fix) ----

/// RESURRECTION is dead: a filled combo's marks are dropped with it, so a later combo on the
/// same legs can NEVER fill off prices recorded during the earlier combo's life — no matter
/// how many bars pass in between and regardless of which symbol's bar triggers the pass
/// (`fill_combos` runs on every bar). It fills only once EVERY leg has printed fresh.
#[test]
fn a_filled_combos_marks_never_resurrect_for_a_later_combo() {
    let mut c = client();
    submit_combo(&mut c, "a", &spread(1, Some(6.0)));
    c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
    assert_eq!(fills_of(&c).len(), 2, "combo A fills");
    assert!(c.leg_marks.is_empty(), "the book emptied => every mark dropped with it");

    // many bars pass with NO combo resting — nothing is recorded (and nothing lingers)
    for ts in [100_000, 200_000, 300_000] {
        c.on_bar(&quoted(ts, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(ts, "FAR", 94.0, 95.0));
    }
    assert!(c.leg_marks.is_empty(), "no combo resting => no marks accumulate");

    // a combo MARKET on the same legs, ~1000 bars after A's life. The next bar — ANY
    // symbol — must not fill it off A-era (or idle-era) prices.
    submit_combo(&mut c, "b", &spread(1, None));
    c.on_bar(&quoted(1_000_000, "OTHER", 1.0, 2.0));
    assert_eq!(fills_of(&c).len(), 2, "no fill off dead marks");
    c.on_bar(&quoted(1_000_000, "NEAR", 99.0, 100.0));
    assert_eq!(fills_of(&c).len(), 2, "one fresh leg is not enough — FAR has not printed");
    c.on_bar(&quoted(1_000_000, "FAR", 94.0, 95.0));
    assert_eq!(fills_of(&c).len(), 4, "fills exactly when every leg has a FRESH print");
}

/// WITHIN-LIFE SKEW is dead: a resting combo whose leg goes dark stops triggering — the other
/// leg's prints cannot cross against the dark leg's old mark, because `A(t) + B(t−N)` is a net
/// that never coexisted. Under the default bound (0 = same-ts) one stale pass is already too
/// many.
#[test]
fn a_leg_that_stops_printing_stops_the_combo() {
    let mut c = client();
    submit_combo(&mut c, "sk", &spread(1, Some(5.0))); // needs net <= 5
    c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    c.on_bar(&quoted(1, "FAR", 94.0, 95.0)); // net = 100 - 94 = 6 > 5 — rests
    assert!(fills_of(&c).is_empty());

    // NEAR goes dark; FAR keeps printing at a bid that WOULD cross against NEAR's ts=1 mark
    // (100 - 95 = 5 <= 5). It must never trigger.
    for ts in 2..8 {
        c.on_bar(&quoted(ts, "FAR", 95.0, 96.0));
        assert!(fills_of(&c).is_empty(), "no fill off a dark leg (pass ts {ts})");
    }
    assert_eq!(c.combos.len(), 1, "the whole combo just rests");

    // NEAR prints again — on the pass where both marks share the current ts, it may trigger.
    c.on_bar(&quoted(8, "NEAR", 99.0, 100.0));
    c.on_bar(&quoted(8, "FAR", 95.0, 96.0));
    assert_eq!(fills_of(&c).len(), 2, "coexisting fresh legs => the combo fills");
}

/// `combo_mark_staleness_ms` mirrors the engine's `max_price_staleness_ms` convention: strict
/// `age > bound`, so a mark exactly at the bound is still fresh and one past it is not.
#[test]
fn a_widened_staleness_bound_admits_recent_marks_and_refuses_older_ones() {
    // bound 10: NEAR marked at ts=1, FAR triggers the pass at ts=11 — age 10 == bound, fresh.
    let mut c = client();
    c.combo_mark_staleness_ms = 10;
    submit_combo(&mut c, "w1", &spread(1, Some(6.0)));
    c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    c.on_bar(&quoted(11, "FAR", 94.0, 95.0));
    assert_eq!(fills_of(&c).len(), 2, "age == bound is fresh (strict >)");

    // same shape, FAR at ts=12 — NEAR's age 11 > 10, stale => the whole combo rests.
    let mut c = client();
    c.combo_mark_staleness_ms = 10;
    submit_combo(&mut c, "w2", &spread(1, Some(6.0)));
    c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    c.on_bar(&quoted(12, "FAR", 94.0, 95.0));
    assert!(fills_of(&c).is_empty(), "age > bound is stale — no leg fills");
    assert_eq!(c.combos.len(), 1);
}

// ---- contingency links on combos are rejected loudly (adversarial-review fix) ----

/// A combo carrying ANY contingency link (OTO parent, OCO links, or an explicit contingency
/// type) is terminally REJECTED at submit: the hold/cascade machinery enforces against the
/// single-symbol book only, and silently recording unenforced links is the failure mode this
/// guards. No `OrderAccepted`, nothing rests, nothing ever fills.
#[test]
fn a_combo_carrying_contingency_links_is_rejected_terminally() {
    type Mutator = fn(&mut OrderRequest);
    let cases: Vec<(&str, Mutator)> = vec![
        ("parent_order_id", |r| r.parent_order_id = Some("entry".into())),
        ("linked_order_ids", |r| r.linked_order_ids = vec!["sib".into()]),
        ("contingency_type", |r| r.contingency_type = Some("oco".into())),
    ];
    for (field, mutate) in cases {
        let mut c = client();
        let mut req = build_combo(&spread(1, Some(6.0)), "cl").expect("valid");
        mutate(&mut req);
        c.submit(&req);
        let evs = drain(&mut c);
        assert!(
            evs.iter().any(|e| matches!(e, Event::OrderRejected(r) if r.client_order_id == "cl")),
            "{field}: a linked combo terminalizes as OrderRejected: {evs:?}"
        );
        assert!(
            !evs.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
            "{field}: never accepted"
        );
        assert!(c.combos.is_empty(), "{field}: nothing rests");
        assert!(c.contingency.is_empty(), "{field}: no unenforceable link is recorded");
        assert!(c.expiry.is_empty(), "{field}: no deadline is recorded");
        c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
        c.on_bar(&quoted(1, "FAR", 94.0, 95.0));
        assert!(fills_of(&c).is_empty(), "{field}: a rejected combo never fills");
    }
}

/// The one contingency direction that IS enforced against the combo book: a PLAIN order
/// OCO-linked to a resting combo cancels the combo — as ONE order, all legs at once — when the
/// plain order fills. (The combo itself carries no links, so it was legitimately accepted.)
#[test]
fn a_plain_oco_fill_cancels_its_resting_combo_sibling() {
    let mut c = client();
    submit_combo(&mut c, "cb", &spread(1, Some(6.0)));
    let plain = OrderRequest {
        client_order_id: "pl".into(),
        venue: "deribit".into(),
        symbol: "COMBO".into(),
        side: 1,
        qty: 1.0,
        order_type: "market".into(),
        linked_order_ids: vec!["cb".into()],
        ..Default::default()
    };
    c.submit(&plain);
    // the plain market order fills on the first bar (single-symbol pass), which OCO-cancels
    // the combo BEFORE fill_combos runs — even though this same bar marks NEAR.
    c.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    assert!(c.combos.is_empty(), "the combo sibling left the book with all its legs");
    let evs = drain(&mut c);
    assert!(
        evs.iter().any(|e| matches!(e, Event::OrderCanceled(x)
                if x.client_order_id == "cb" && x.reason.as_str() == "oco")),
        "one OrderCanceled(oco) for the combo: {evs:?}"
    );
    // and it can never fill afterwards, even once both legs print
    c.on_bar(&quoted(2, "NEAR", 99.0, 100.0));
    c.on_bar(&quoted(2, "FAR", 94.0, 95.0));
    assert_eq!(fills_of(&c).len(), 1, "only the plain order's own fill exists");
    assert!(c.leg_marks.is_empty(), "no combo resting => marks were dropped");
}
