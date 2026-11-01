use super::*;

/// What this proves, precisely: [`caps_for`]'s match arms map each venue STRING to the const
/// this module names for it — a transposed-arm typo is loud here. It proves nothing about
/// whether a const's VALUES describe the adapter; the module doc lists the checks that do.
/// (Kept alongside `every_roster_venue_has_a_declared_row`, which subsumes it for the roster,
/// because this one names each pair literally and so reads as the registry's own table.)
#[test]
fn registry_maps_every_known_venue() {
    assert_eq!(caps_for("binance"), BINANCE);
    assert_eq!(caps_for("bybit"), BYBIT);
    assert_eq!(caps_for("okx"), OKX);
    assert_eq!(caps_for("deribit"), DERIBIT);
    assert_eq!(caps_for("oanda"), OANDA);
    assert_eq!(caps_for("ig"), IG);
    assert_eq!(caps_for("fxcm"), FXCM);
    assert_eq!(caps_for("dukascopy"), DUKASCOPY);
    assert_eq!(caps_for("polymarket"), POLYMARKET);
    assert_eq!(caps_for("aster"), ASTER);
    assert_eq!(caps_for("hyperliquid"), HYPERLIQUID);
}

/// Completeness vs the canonical roster: every [`vike_model::VENUES`] entry has a NAMED
/// declared row, and the registry serves exactly it. Adding a venue to the roster fails here
/// until its row exists — the whole point of the roster. (A row's VALUE may equal the
/// `UNSUPPORTED` fallback — cTrader's does, deliberately — but it must be NAMED: the named
/// const is the proof the venue was classified, not forgotten.)
#[test]
fn every_roster_venue_has_a_declared_row() {
    // `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is
    // re-indented by rustfmt once a row ending in a trailing `//` comment is generated above
    // it, which defeats `--remove`. Gated by `crates/vike-ops/tests/venues/new_venue_gate/rustfmt_rule.rs`'s
    // `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
    #[rustfmt::skip]
        let rows: &[(&str, VenueCaps)] = &[
            ("binance", BINANCE),
            ("bybit", BYBIT),
            ("okx", OKX),
            ("deribit", DERIBIT),
            ("oanda", OANDA),
            ("ig", IG),
            ("fxcm", FXCM),
            ("dukascopy", DUKASCOPY),
            ("polymarket", POLYMARKET),
            ("ibkr", IBKR),
            ("ctrader", CTRADER),
            ("alpaca", ALPACA),
            ("aster", ASTER),
            ("hyperliquid", HYPERLIQUID),
            // vike:new-venue:row ("{venue}", {VENUE}), // TODO(new-venue: {venue}): declared-row completeness
        ];
    assert_eq!(rows.len(), vike_model::VENUES.len(), "one declared row per roster venue");
    for &v in vike_model::VENUES {
        let (_, want) = rows
            .iter()
            .find(|(rv, _)| *rv == v)
            .unwrap_or_else(|| panic!("no VenueCaps row declared for roster venue {v}"));
        assert_eq!(caps_for(v), *want, "{v}: registry must serve its declared row");
    }
}

#[test]
fn unknown_venue_is_conservative_unsupported() {
    let c = caps_for("nasdaq");
    assert_eq!(c, VenueCaps::UNSUPPORTED);
    assert!(!c.supports_modify);
    assert!(!c.supports_native_batch);
    assert!(!c.supports_reduce_only);
    assert!(c.supported_tifs.is_empty());
    assert!(c.accepted_tifs.is_empty());
    assert!(c.supported_order_kinds.is_empty());
    assert!(c.reduce_only_kinds.is_empty());
    assert!(c.trigger_types.is_empty());
    assert!(!c.supports_post_only);
    assert!(c.margin_modes.is_empty());
    assert_eq!(c.default_margin_mode, MarginMode::Cross);
    assert_eq!(c.max_batch, 0);
    assert!(!c.has_live_data());
    assert!(!c.backfill_bars && !c.backfill_ticks);
}

#[test]
fn default_is_unsupported() {
    assert_eq!(VenueCaps::default(), VenueCaps::UNSUPPORTED);
}

/// The modify-gate logic: it BLOCKS (returns false) for every venue whose adapter has no native
/// modify, and ALLOWS the venues that wire one. This is the exact decision the GUI wires.
#[test]
fn modify_gate_blocks_when_unsupported() {
    // allowed: the venues with a verified native amend (crypto perps + HL cancel-replace +
    // cTrader's `AMEND_ORDER_REQ` amend, OANDA's order REPLACE, audited from the adapter, + IG's
    // price-only `PUT /workingorders/otc/{dealId}`, + IBKR's re-place under the same orderId, Dukascopy's
    // `setOpenPrice`/`setRequestedAmount`, + Deribit's `private/edit`, + FXCM's price-only `EditOrder`).
    for v in [
        "binance",
        "bybit",
        "okx",
        "aster",
        "hyperliquid",
        "ctrader",
        "oanda",
        "ig",
        "ibkr",
        "dukascopy",
        "deribit",
        "fxcm",
    ] {
        assert!(caps_for(v).allows_modify(), "{v} should allow modify");
    }
    // blocked: no native modify wired
    for v in ["polymarket", "alpaca", "unknown"] {
        assert!(!caps_for(v).allows_modify(), "{v} must block modify");
    }
}

/// cTrader's audited row (audit of the #498 known-stale `UNSUPPORTED` placeholder): a native
/// amend (`supports_modify`) + a spot-quote/trendbar `DataClient`, everything else conservative.
/// Pins the exact matrix so a regression that re-widens or re-narrows a field trips here.
#[test]
fn ctrader_caps_reflect_the_adapter() {
    let c = caps_for("ctrader");
    assert_eq!(c, CTRADER);
    // the two flipped-from-UNSUPPORTED fields
    assert!(c.supports_modify, "ctrader wires a native AmendOrder");
    assert_eq!(
        c.live_data,
        LiveDataCaps { bars: true, quotes: true, trades: false, book: false, depth: false },
        "ctrader DataClient serves trendbars + spot quotes only"
    );
    assert!(c.has_live_data());
    // the fields the adapter does NOT wire — must stay conservative
    assert!(!c.supports_native_batch, "no batch endpoint wired");
    assert!(!c.supports_reduce_only, "order_to_new_order sets no reduceOnly");
    assert!(!c.supports_combo);
    assert_eq!(c.supported_tifs, &[TimeInForce::Gtc], "no explicit TIF sent → GTC default");
    assert!(c.backfill_bars, "CtraderKlines is the credentialed bars source");
    assert!(!c.backfill_ticks, "no cTrader tick collector: the tick request is not wired");
}

/// Only Binance + Aster wire a native batch endpoint; everyone else uses the fan-out default.
/// (Property assertions go through the non-const `caps_for` so they aren't const-folded — the
/// registry equals the consts, pinned by `registry_maps_every_known_venue`.)
#[test]
fn native_batch_is_binance_aster_hyperliquid() {
    // Adapters that wire a native batch endpoint (binance/aster POST .../batchOrders ≤5;
    // hyperliquid's msgpack action batch).
    assert!(caps_for("binance").supports_native_batch);
    assert!(caps_for("aster").supports_native_batch);
    assert!(caps_for("hyperliquid").supports_native_batch);
    for v in [
        "bybit",
        "okx",
        "deribit",
        "oanda",
        "ig",
        "fxcm",
        "dukascopy",
        "polymarket",
        "ibkr",
        "alpaca",
    ] {
        assert!(!caps_for(v).supports_native_batch, "{v} must not claim native batch");
    }
}

/// Combo support is DERIBIT ONLY — the deliberate live-gate flip: its adapter resolves
/// `combo_legs` at submit (`resolve_combo_order`: create_combo → orientation solve →
/// buy/sell on the combo id), which no other adapter does. Every OTHER roster venue (and the
/// unknown fallback) must stay `false` — a `true` row without
/// the resolve step would put an EMPTY-symbol order on a live wire (`build_combo` leaves
/// `symbol` for the adapter). Pinned per-venue so an accidental flip anywhere trips here.
#[test]
fn combo_support_is_deribit_only() {
    assert!(caps_for("deribit").supports_combo, "deribit wires resolve-at-submit combos");
    for v in [
        "binance",
        "bybit",
        "okx",
        "oanda",
        "ig",
        "fxcm",
        "dukascopy",
        "polymarket",
        "ibkr",
        "ctrader",
        "alpaca",
        "aster",
        "hyperliquid",
        "unknown",
    ] {
        assert!(!caps_for(v).supports_combo, "{v} must not claim combo support");
    }
}

/// reduce-only is a perp/options concept: the crypto perps + deribit build it; FX/CFD,
/// dukascopy (unverifiable), and polymarket do not.
#[test]
fn reduce_only_matches_derivatives_venues() {
    for v in ["binance", "bybit", "okx", "deribit"] {
        assert!(caps_for(v).supports_reduce_only, "{v} builds reduceOnly");
    }
    for v in ["oanda", "ig", "fxcm", "dukascopy", "polymarket"] {
        assert!(!caps_for(v).supports_reduce_only, "{v} does not wire reduceOnly");
    }
}

/// The live-data matrix: every verb each venue with a `DataClient` actually serves, exactly as
/// its feed implements it — the crypto depth-vs-book split, polymarket's bars-less shape, ibkr's
/// full five, and deribit's full five since the datahub began serving its DOM.
#[test]
fn live_data_matrix_matches_the_feeds() {
    // crypto perps: bars + trades + depth (bybit/okx trades live-verified 2026-07-20 via each
    // crate's `*_trades_feed_smoke` against the real `DataClient::subscribe_trades` path).
    assert_eq!(
        BINANCE.live_data,
        LiveDataCaps { bars: true, quotes: false, trades: true, book: false, depth: true }
    );
    assert_eq!(
        BYBIT.live_data,
        LiveDataCaps { bars: true, quotes: false, trades: true, book: false, depth: true }
    );
    assert_eq!(BYBIT.live_data, OKX.live_data);
    assert_eq!(BYBIT.live_data, BINANCE.live_data);
    // polymarket: quotes + trades + book, no bars, no depth
    assert_eq!(
        POLYMARKET.live_data,
        LiveDataCaps { bars: false, quotes: true, trades: true, book: true, depth: false }
    );
    // ibkr: bars, quotes and trades on both backends (`IbkrFeeds` over ibapi, the cpapi data
    // client over the CP Gateway `smd` topic); NO book and NO depth since docs/decisions/0140 —
    // the accounts hold no depth entitlement, and both clients refuse the two verbs off this row.
    // (This row said `LiveDataCaps::NONE` from 2026-07-14 until the pump-spec cross-pin caught it,
    // then all five until 0140.)
    assert_eq!(
        IBKR.live_data,
        LiveDataCaps { bars: true, quotes: true, trades: true, book: false, depth: false }
    );
    // oanda: the SPLIT-PLANE feed — quotes stream (the chunked-HTTP pricing stream), bars
    // poll the candles REST. No trade tape is published, and the pricing ladder is an
    // unsequenced top-of-book snapshot rather than a delta-synced book → book/depth false.
    assert_eq!(
        OANDA.live_data,
        LiveDataCaps { bars: true, quotes: true, trades: false, book: false, depth: false }
    );
    // deribit (split-plane I9): bars + quotes + trades + the LOSSLESS book lane, and — since
    // 2026-10-04 — the conflating DOM `depth` lane, the top levels of that same `book.*` fold
    // (`vike_deribit::market_feed::Feeds::subscribe_depth`). Both L2 lanes, like ibkr.
    assert_eq!(
        DERIBIT.live_data,
        LiveDataCaps { bars: true, quotes: true, trades: true, book: true, depth: true }
    );
    // ig serves bars+quotes only — the dealer-venue shape: L1 with no ladder, no tape.
    assert_eq!(
        IG.live_data,
        LiveDataCaps { bars: true, quotes: true, trades: false, book: false, depth: false }
    );
    // no live feed at all for these
    for v in ["fxcm", "dukascopy"] {
        assert!(!caps_for(v).has_live_data(), "{v} has no live DataClient");
    }
    // ...and every OTHER roster venue does have one. Stated as the complement so a venue can
    // never fall out of both lists. The non-circular tie for exactly this partition is
    // `venue_caps_cross_pin_the_pump_spec` in vike-bridge-core.
    for v in [
        "binance",
        "bybit",
        "okx",
        "aster",
        "hyperliquid",
        "deribit",
        "polymarket",
        "alpaca",
        "ctrader",
        "ibkr",
        "oanda",
        "ig",
    ] {
        assert!(caps_for(v).has_live_data(), "{v} has a live DataClient");
    }
}

/// TIF sets track the adapters: OANDA maps the full five; the flipped venues (tif step-2)
/// declare exactly the set their adapter maps 1:1 (everything else loud-denied at submit);
/// the unflipped GTC-hardcoders stay `&[Gtc]`.
#[test]
fn tif_sets_reflect_what_the_adapter_wires() {
    let oanda = caps_for("oanda");
    assert_eq!(oanda.supported_tifs.len(), 5, "oanda maps the five lifetimes, not post-only");
    assert!(!oanda.supported_tifs.contains(&TimeInForce::PostOnly));
    assert!(oanda.supported_tifs.contains(&TimeInForce::Day));
    assert!(oanda.supported_tifs.contains(&TimeInForce::Gtd));
    // binance: the row stays the CONSERVATIVE spot∩perp intersection — the perp lane
    // additionally wires native GTD, but this registry cannot distinguish the lanes (one
    // "binance" key), so Gtd is deliberately NOT declared here; the per-lane truth is
    // venue_tif's "binance-perp" row (see the BINANCE row doc).
    for v in ["binance", "bybit", "okx"] {
        assert_eq!(
            caps_for(v).supported_tifs,
            &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::PostOnly],
            "{v}: GTC/IOC/FOK/PostOnly declared (binance = lane intersection, perp-GTD lives in \
                 venue_tif)"
        );
    }
    assert_eq!(
        caps_for("deribit").supported_tifs,
        &[
            TimeInForce::Gtc,
            TimeInForce::Ioc,
            TimeInForce::Fok,
            TimeInForce::Day,
            TimeInForce::PostOnly
        ],
        "deribit flipped: GTC default + IOC/FOK/Day/PostOnly mapped, GTD denied"
    );
    for v in ["ig", "fxcm", "dukascopy", "aster"] {
        assert_eq!(caps_for(v).supported_tifs, &[TimeInForce::Gtc], "{v} is GTC-only");
    }
    assert_eq!(caps_for("polymarket").supported_tifs, &[TimeInForce::Gtc, TimeInForce::Fok]);
}

/// The backfill axes, pinned per roster venue.
///
/// ⚠ This test was called `backfill_matches_vike_backfill` and matched NOTHING in
/// vike-backfill — it asserted a hand-written list, and the list was WRONG: it required
/// `!backfill_bars` for **deribit**, which had a real `deribit_backfill` bin from #1030,
/// and required `!backfill_bars` for dukascopy, whose collector resamples bars from the ticks
/// it stores. Three more rows were wrong and simply absent from the list (hyperliquid, ibkr,
/// and dukascopy's `bar`), so nothing failed. Renamed to say what it does.
///
/// **The non-circular check is `venue_caps_cross_pin_the_backfill_table` in vike-backfill**,
/// which compares these two fields to `backfill_caps(FillSource::Venue, v)` — a table with a
/// filesystem-existence gate and a source-scanning shape gate over the real collector modules.
/// It lives THERE because vike-backfill depends on vike-model and not the reverse.
#[test]
fn backfill_matrix_is_pinned() {
    // (venue, bars, ticks) — one row per roster venue, verbatim.
    #[rustfmt::skip]
        let rows: &[(&str, bool, bool)] = &[
            ("binance",     true,  false),
            ("bybit",       true,  false),
            ("okx",         true,  false),
            ("aster",       true,  false),
            // klines collectors that landed AFTER this table was written; each row said `false`
            // until the vike-backfill cross-pin was added.
            ("deribit",     true,  false),
            ("hyperliquid", true,  false),
            // ⚠ `ibkr` was `true` here too (2026-07-15, #311) until docs/decisions/0094 deleted
            // `ibkr_backfill` and its planner (2026-09-28), measured unused.
            ("ibkr",        false, false),
            // the ONLY venue-direct tick source (.bi5 quotes) — and its bars are a resample of
            // them, which this field's doc settles as `backfill_bars: true`.
            ("dukascopy",   true,  true),
            // bars through the datahub's CREDENTIALED lane (docs/decisions/0097); `false` until it.
            ("oanda",       true,  false),
            // bars through the datahub's CREDENTIALED lane (docs/decisions/0194); no ticks.
            ("ig",          true,  false),
            // bars from the keyless public candle archive (`FxcmKlines`); no ticks anywhere.
            ("fxcm",        true,  false),
            ("polymarket",  false, false),
            // bid-side bars through the datahub's CREDENTIALED lane (`CtraderKlines`); no ticks.
            ("ctrader",     true,  false),
            ("alpaca",      false, false),
            // vike:new-venue:row ("{venue}",  false, false), // TODO(new-venue: {venue}): flip when a collector lands
        ];
    assert_eq!(rows.len(), vike_model::VENUES.len(), "one pinned row per roster venue");
    for (v, bars, ticks) in rows {
        let c = caps_for(v);
        assert_eq!(c.backfill_bars, *bars, "{v} backfill_bars");
        assert_eq!(c.backfill_ticks, *ticks, "{v} backfill_ticks");
    }
}

// ── expanded axes (w2-task-5) ────────────────────────────────────────────────────────────

/// The NEW-axis matrix, pinned verbatim per venue (the tif/margin matrix-test discipline):
/// (venue, order_kinds, trigger_types, accepted_tifs, margin_modes, max_batch). Any drift in
/// a row is loud here; each bridge's own `caps_test` additionally ties its row to the
/// adapter code the value was read from.
#[test]
fn expanded_axes_matrix_is_pinned() {
    use MarginMode::{Cash, Cross, Isolated};
    use TimeInForce::{Day, Fok, Gtc, Gtd, Ioc, PostOnly};
    use TriggerType::{StopLoss, TakeProfit};
    const ML: &[&str] = &["market", "limit"];
    const MLS: &[&str] = &["market", "limit", "stop"];
    const ALL5: &[TimeInForce] = &[Gtc, Ioc, Fok, Gtd, Day];
    // (venue, order_kinds, trigger_types, accepted_tifs, margin_modes, max_batch)
    type Row = (
        &'static str,
        &'static [&'static str],
        &'static [TriggerType],
        &'static [TimeInForce],
        &'static [MarginMode],
        usize,
    );
    #[rustfmt::skip]
        let rows: &[Row] = &[
            ("binance",     MLS, &[StopLoss], &[Gtc, Ioc, Fok, Gtd, PostOnly], &[Cross], 5),
            ("bybit",       MLS, &[StopLoss], &[Gtc, Ioc, Fok, PostOnly],  &[Cross], 0),
            ("okx",         MLS, &[StopLoss], &[Gtc, Ioc, Fok, PostOnly],  &[Cross, Isolated], 0),
            ("aster",       MLS, &[StopLoss], &[Gtc],                      &[Cross], 5),
            ("deribit",     &["market", "limit", "stop", "stop_limit", "take_profit"],
                &[StopLoss, TakeProfit], &[Gtc, Ioc, Fok, Day, PostOnly], &[Cross], 0),
            ("oanda",       MLS, &[StopLoss], ALL5,                        &[Cross], 0),
            ("alpaca",      &["market", "limit", "stop", "stop_limit"], &[StopLoss],
                &[Day, Gtc, Ioc, Fok, Gtd],                               &[Cross], 0),
            ("ig",          MLS, &[StopLoss], &[Gtc],                      &[Cross], 0),
            ("fxcm",        MLS, &[StopLoss], &[Gtc],                      &[Cross], 0),
            ("dukascopy",   MLS, &[StopLoss], &[Gtc],                      &[Cross], 0),
            ("polymarket",  ML,  &[],         ALL5,                        &[Cash],  0),
            ("ibkr",        MLS, &[StopLoss], &[Gtc, Day, Ioc, Fok],       &[Cross], 0),
            ("ctrader",     MLS, &[StopLoss], &[Gtc],                      &[Cross], 0),
            ("hyperliquid", &["market", "limit", "stop", "stop_limit", "take_profit"],
                &[StopLoss, TakeProfit], &[Gtc, Ioc, Fok, Gtd, Day, PostOnly], &[Cross], usize::MAX),
            // vike:new-venue:row ("{venue}",     ML,  &[],         &[Gtc],                      &[Cross], 0), // TODO(new-venue: {venue}): pin what the adapter really wires
        ];
    assert_eq!(rows.len(), vike_model::VENUES.len(), "one pinned row per roster venue");
    for (venue, kinds, triggers, accepted, margins, max_batch) in rows {
        let c = caps_for(venue);
        assert_eq!(c.supported_order_kinds, *kinds, "{venue} supported_order_kinds");
        assert_eq!(c.trigger_types, *triggers, "{venue} trigger_types");
        assert_eq!(c.accepted_tifs, *accepted, "{venue} accepted_tifs");
        assert_eq!(c.margin_modes, *margins, "{venue} margin_modes");
        assert_eq!(c.max_batch, *max_batch, "{venue} max_batch");
        assert_eq!(c.supports_post_only, accepted.contains(&PostOnly), "{venue} post-only");
    }
}

/// `supports_post_only` is not a second fact: it is exactly "the honored TIF set lists `PostOnly`"
/// (so the published flag, the preflight and the wire row cannot disagree), and the roster's
/// post-only venues are the five that map it on the wire. Polymarket has the venue-side flag but
/// no proof (NO orders there), so it is refused by name.
#[test]
fn post_only_is_the_supported_tifs_row() {
    let mut yes = Vec::new();
    for &v in vike_model::VENUES {
        let c = caps_for(v);
        assert_eq!(c.supports_post_only, c.supported_tifs.contains(&TimeInForce::PostOnly), "{v}");
        if c.supports_post_only {
            yes.push(v);
        }
    }
    assert_eq!(yes, ["binance", "bybit", "okx", "deribit", "hyperliquid"]);
}

/// Structural invariants across every roster venue: the admit set contains the honored set;
/// trigger declarations agree between the two axes; the batch cap agrees with the batch
/// flag; every declared kind is canonical vocabulary; a roster venue always wires SOME kind.
#[test]
fn expanded_axes_invariants_hold() {
    for &v in vike_model::VENUES {
        let c = caps_for(v);
        for t in c.supported_tifs {
            assert!(c.accepted_tifs.contains(t), "{v}: accepted_tifs must contain {t:?}");
        }
        let kinds_have_trigger = c.supported_order_kinds.iter().any(|k| TRIGGER_KINDS.contains(k));
        assert_eq!(
            kinds_have_trigger,
            !c.trigger_types.is_empty(),
            "{v}: trigger_types ⇔ a trigger kind in supported_order_kinds"
        );
        assert_eq!(c.max_batch > 0, c.supports_native_batch, "{v}: max_batch ⇔ native batch");
        for k in c.supported_order_kinds {
            assert!(ORDER_KINDS.contains(k), "{v}: non-canonical order kind {k}");
        }
        assert!(!c.supported_order_kinds.is_empty(), "{v}: must wire at least one kind");
        assert!(!c.margin_modes.is_empty(), "{v}: must honor at least its default mode");
        // The default vike SENDS must be a mode an explicit request could also name. Both
        // sides are this table's own fields, so this is an INTRA-table invariant now — it
        // used to be half of the cross-pin below, back when the default lived in the other
        // table.
        assert!(
            c.margin_modes.contains(&c.default_margin_mode),
            "{v}: default_margin_mode must be request-honorable"
        );
    }
}

/// The one venue whose adapter refuses a reduce-only kind by NAME, measured from the code:
/// IG places a reduce-only MARKET only (`crates/bridges/ig/src/exec.rs`'s `CLOSE_MARKET_ONLY`).
/// Dukascopy was the second until its sidecar took a reduce-only LIMIT as the native take-profit
/// (`StrategyBridge.java`'s `handleStop` with `Protection.TAKE_PROFIT`): its client refuses one only for a jar that
/// predates that, which a static row cannot say. Every other roster venue's adapter carries the flag onto the
/// wire, routes it (FXCM, cTrader) or ignores it, and refuses none for its kind, so its
/// `reduce_only_kinds` IS its `supported_order_kinds`.
const REDUCE_ONLY_EXCEPTIONS: &[(&str, &[&str])] = &[("ig", &["market"])];

/// `reduce_only_kinds` per venue class: the measured exception is pinned VERBATIM, and every
/// other roster venue lists exactly what it accepts (PRESERVE: nothing that works today is refused).
/// A new venue passes with a row equal to its kinds and joins the exceptions only with a measured
/// adapter refusal.
#[test]
fn reduce_only_kinds_are_the_kind_set_except_where_the_adapter_refuses_by_name() {
    for &v in vike_model::VENUES {
        let c = caps_for(v);
        match REDUCE_ONLY_EXCEPTIONS.iter().find(|(ev, _)| *ev == v) {
            Some((_, kinds)) => assert_eq!(c.reduce_only_kinds, *kinds, "{v} reduce_only_kinds"),
            None => assert_eq!(
                c.reduce_only_kinds, c.supported_order_kinds,
                "{v}: no adapter refusal was measured, so every kind it accepts stays accepted"
            ),
        }
    }
}

/// Structural invariants of the axis: a reduce-only kind is a kind the venue accepts at all, and
/// the reduce-only MARKET is never refused. A flatten, a market-exit, a margin call and a
/// liquidation are reduce-only markets; a row that dropped `market` would trap the operator in a
/// position at the edge.
#[test]
fn reduce_only_kinds_are_a_subset_of_the_kinds_and_keep_the_market_close() {
    for &v in vike_model::VENUES {
        let c = caps_for(v);
        for k in c.reduce_only_kinds {
            assert!(
                c.supported_order_kinds.contains(k),
                "{v}: reduce-only {k} is not a kind it accepts"
            );
        }
        assert!(
            c.reduce_only_kinds.contains(&"market"),
            "{v}: the reduce-only market close must pass"
        );
    }
}

/// The edge, per class: a reduce-only order passes exactly when its kind is in the row, the same
/// order WITHOUT the flag keeps passing (a Dukascopy entry limit is an ordinary order), and the
/// refusal reads `REDUCE_ONLY_UNSUPPORTED: kind=<kind> venue=<venue>`.
#[test]
fn preflight_refuses_a_reduce_only_kind_the_adapter_refuses_and_nothing_else() {
    use TimeInForce::Gtc;
    let reduce =
        |venue: &str, kind: &str| OrderRequest { reduce_only: true, ..req(venue, kind, Gtc) };
    // Dukascopy: the take-profit (a reduce-only limit, the venue's own price on the open order), the stop-loss and the
    // market close all pass; a jar that predates the take-profit is refused by the client, after this edge.
    assert_eq!(preflight_order(&reduce("dukascopy", "limit")), Ok(()));
    assert_eq!(preflight_order(&reduce("dukascopy", "stop")), Ok(()));
    assert_eq!(preflight_order(&reduce("dukascopy", "market")), Ok(()));
    assert_eq!(
        preflight_order(&req("dukascopy", "limit", Gtc)),
        Ok(()),
        "an entry limit is no exit"
    );
    // IG: reduce-only is a market close; a resting reduce-only stop or limit is refused.
    for kind in ["limit", "stop"] {
        let deny = preflight_order(&reduce("ig", kind)).unwrap_err();
        assert_eq!(deny.to_string(), format!("REDUCE_ONLY_UNSUPPORTED: kind={kind} venue=ig"));
    }
    assert_eq!(preflight_order(&reduce("ig", "market")), Ok(()));
    assert_eq!(preflight_order(&req("ig", "stop", Gtc)), Ok(()), "an entry stop is no exit");
    // Every other venue: each kind it accepts passes as a reduce-only order too.
    for &v in vike_model::VENUES {
        if REDUCE_ONLY_EXCEPTIONS.iter().any(|(ev, _)| *ev == v) {
            continue;
        }
        for k in caps_for(v).supported_order_kinds {
            assert_eq!(preflight_order(&reduce(v, k)), Ok(()), "{v}/{k} reduce-only");
        }
    }
    // The flag never overrides the kind check: a kind the venue lacks is still refused as itself.
    assert_eq!(
        preflight_order(&reduce("polymarket", "stop")),
        Err(PreflightDeny::Trigger { kind: "stop".into(), venue: "polymarket".into() })
    );
    // An undeclared (paper/sim) venue and a combo are never judged here.
    assert_eq!(preflight_order(&reduce("sim", "limit")), Ok(()));
}

/// **A stated `paper` tier skips the reduce-only axis and NO other, on every venue row.** The matrix:
/// every roster venue x every order kind x reduce-only on/off x every TIF x every margin request x
/// every tier.
///
/// - demo, live and a tier nobody stated (`None`) are judged by the whole row, identically to
///   `preflight_order` (PRESERVE: v0.1.96's verdicts);
/// - paper's verdict is the verdict of the SAME order without the flag, so the kind, TIF and margin
///   axes still bite on a paper book (the simulator models those trading rules);
/// - the relaxation is real exactly where the row refuses a reduce-only kind by name, and nowhere
///   else (the cases are counted: IG's limit and stop, per TIF and margin).
#[test]
fn a_stated_paper_tier_skips_the_reduce_only_axis_and_no_other() {
    use MarginMode::{Cash, Cross, Isolated};
    use vike_model::AccountTier::{Demo, Live, Paper};
    let mut relaxed: Vec<(String, String)> = Vec::new();
    for &v in vike_model::VENUES {
        for kind in ORDER_KINDS {
            for reduce_only in [false, true] {
                for &tif in TimeInForce::ALL {
                    for margin_mode in [None, Some(Cross), Some(Isolated), Some(Cash)] {
                        let r = OrderRequest { reduce_only, margin_mode, ..req(v, kind, tif) };
                        let whole_row = preflight_order(&r);
                        let at = |tier| preflight_order_at(&r, v, tier);
                        let case =
                            format!("{v}/{kind}/reduce_only={reduce_only}/{tif:?}/{margin_mode:?}");
                        assert_eq!(
                            at(None),
                            whole_row,
                            "{case}: an unstated tier is the whole row"
                        );
                        assert_eq!(at(Some(Demo)), whole_row, "{case}: demo is the whole row");
                        assert_eq!(at(Some(Live)), whole_row, "{case}: live is the whole row");

                        let flagless = OrderRequest { reduce_only: false, ..r.clone() };
                        let paper = at(Some(Paper));
                        assert_eq!(
                            paper,
                            preflight_order_at(&flagless, v, None),
                            "{case}: paper is judged by every axis but the reduce-only one"
                        );
                        if paper != whole_row {
                            assert!(
                                matches!(whole_row, Err(PreflightDeny::ReduceOnly { .. })),
                                "{case}: paper may differ only where the row refuses by reduce-only: {whole_row:?}"
                            );
                            relaxed.push((v.to_string(), kind.to_string()));
                        }
                    }
                }
            }
        }
    }
    relaxed.sort();
    relaxed.dedup();
    let want = [("ig", "limit"), ("ig", "stop")].map(|(v, k)| (v.to_string(), k.to_string()));
    assert_eq!(
        relaxed, want,
        "the venues and kinds a paper book is let through where the row refuses"
    );
}

/// The bracket, per class (the legs `build_bracket` returns): it is refused where an exit cannot
/// rest (IG: both exits) and passes where both exits can (Dukascopy since its take-profit is native), for every
/// other roster venue that accepts a stop at all. The refused leg is the first in leg order.
#[test]
fn a_bracket_is_judged_leg_by_leg_against_the_reduce_only_kinds() {
    use vike_model::orders::order::{BracketSpec, build_bracket};
    for &v in vike_model::VENUES {
        let spec = BracketSpec {
            venue: v.to_string(),
            symbol: "X".to_string(),
            side: 1,
            qty: 1.0,
            entry_price: Some(100.0),
            stop_loss: 95.0,
            take_profit: 110.0,
            account: None,
        };
        let verdicts: Vec<Result<(), PreflightDeny>> =
            build_bracket(&spec, "e", "s", "t").iter().map(preflight_order).collect();
        let (entry, stop_loss, take_profit) = (&verdicts[0], &verdicts[1], &verdicts[2]);
        assert_eq!(entry, &Ok(()), "{v}: the entry is an ordinary order");
        match v {
            "ig" => {
                assert!(
                    matches!(stop_loss, Err(PreflightDeny::ReduceOnly { kind, .. }) if kind == "stop")
                );
                assert!(
                    matches!(take_profit, Err(PreflightDeny::ReduceOnly { kind, .. }) if kind == "limit")
                );
            }
            // A venue with no native stop refuses the stop-loss as a TRIGGER kind, as it always did.
            "polymarket" => assert!(matches!(stop_loss, Err(PreflightDeny::Trigger { .. }))),
            _ => {
                assert_eq!(stop_loss, &Ok(()), "{v}: PRESERVE, its stop-loss still passes");
                assert_eq!(take_profit, &Ok(()), "{v}: PRESERVE, its take-profit still passes");
            }
        }
    }
}

/// The cross-table contract with [`crate::venue_margin_support`], in one sentence: **what we
/// SEND is a subset of what they OFFER.**
///
/// The two tables have different subjects — this one is what vike's adapters do, that one is
/// what the exchange makes available — and the only thing that can be checked BETWEEN them is
/// containment:
/// - every mode an order may REQUEST here (`margin_modes`) is a mode the venue offers, and
/// - the mode vike sends by DEFAULT (`default_margin_mode`) is likewise one the venue offers.
///
/// The second used to be stated against this table alone ("the default is request-honorable"),
/// which is now an intra-table invariant asserted in `expanded_axes_invariants_hold`. Stated
/// against the OFFER table it says something that assertion cannot: vike does not default to a
/// mode the exchange never exposed.
#[test]
fn margin_modes_cross_pin_venue_margin_support() {
    for &v in vike_model::VENUES {
        let caps = caps_for(v);
        let support = crate::venue_margin_support::venue_margin_support(v);
        for m in caps.margin_modes {
            assert!(
                support.offered_modes.contains(m),
                "{v}: order-honored mode {m:?} must be offered by the venue"
            );
        }
        assert!(
            support.offered_modes.contains(&caps.default_margin_mode),
            "{v}: the mode vike sends by default must be one the venue offers"
        );
    }
}

/// TODAY'S REALITY, relocated verbatim from the margin table when `default_margin_mode` moved
/// here: every venue vike trades perps on produces Cross; polymarket produces Cash. This pins
/// the current wire behavior.
///
/// Only okx's row is tied to adapter CODE (see
/// `crates/bridges/okx/src/margin_mode_tests.rs`'s `okx_default_margin_mode_matches_the_td_mode_builder`) —
/// it is the one adapter that emits a margin field at all. Every other row records the
/// account-side default that rules because the adapter sends nothing, which no test here can
/// prove.
///
/// ⚠ Read `hyperliquid`'s row below with the field's own "the default for assets that HAVE a
/// choice" caveat: 9 of its 232 assets are isolated-only and rule Isolated. This test pins the
/// per-VENUE declaration (which is what the field is); the per-ASSET narrowing is pinned in the
/// bridge, by `crates/bridges/hyperliquid/src/base/symbology_tests.rs`'s
/// `effective_margin_mode_is_per_asset_not_per_venue` — which asserts the two DISAGREE for an
/// isolated-only asset, so flipping this row to make them agree would break that test loudly.
#[test]
fn default_margin_mode_reflects_current_behavior() {
    for v in ["binance", "aster", "okx", "bybit", "hyperliquid", "deribit"] {
        assert_eq!(caps_for(v).default_margin_mode, MarginMode::Cross, "{v} is cross today");
    }
    assert_eq!(caps_for("polymarket").default_margin_mode, MarginMode::Cash);
    // FX/equity also resolve to Cross (shared account margin)
    for v in ["oanda", "ig", "fxcm", "dukascopy", "alpaca", "ibkr", "ctrader"] {
        assert_eq!(caps_for(v).default_margin_mode, MarginMode::Cross, "{v}");
    }
    // The unknown-venue fallback keeps the same conservative value.
    assert_eq!(VenueCaps::UNSUPPORTED.default_margin_mode, MarginMode::Cross);
}

// ── preflight (w2-task-5) ────────────────────────────────────────────────────────────────

/// `order_type` goes through the STORED door (`OrderType::from_stored`): a canonical word is its
/// variant, anything else is `Unrecognized` verbatim.
fn req(venue: &str, order_type: &str, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        client_order_id: "c-pf".to_string(),
        venue: venue.to_string(),
        symbol: "X".to_string(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::from_stored(order_type),
        price: Some(1.0),
        time_in_force: tif,
        ..Default::default()
    }
}

/// THE COMPAT LAW: nothing a venue accepts-and-honors today is refused. Every row's own
/// declared kinds/TIFs pass; the coercion venues' coerced TIFs pass; binance's perp-lane GTD
/// passes (lane union); non-limit paths never TIF-refuse; unknown venues, combos and
/// `margin_mode: None` all pass. (An UNRECOGNISED kind no longer passes: see
/// `preflight_refuses_an_unrecognised_kind_on_every_venue`.)
#[test]
fn preflight_passes_everything_accepted_today() {
    use TimeInForce::Gtc;
    for &v in vike_model::VENUES {
        let c = caps_for(v);
        for k in c.supported_order_kinds {
            assert_eq!(preflight_order(&req(v, k, Gtc)), Ok(()), "{v}/{k}");
        }
        for t in c.accepted_tifs {
            // a lifetime the adapter would run as another one is admitted by `accepted_tifs` (the
            // compatibility law) but refused by name now that one can be NAMED: see
            // `a_lifetime_the_adapter_would_coerce_is_refused_by_name`
            if matches!(
                crate::venue_tif::venue_tif(v, *t),
                crate::venue_tif::TifOutcome::Coerced { .. }
            ) {
                continue;
            }
            assert_eq!(preflight_order(&req(v, "limit", *t)), Ok(()), "{v}/limit/{t:?}");
        }
        // market path carries no TIF axis anywhere — never TIF-refused (post-only is the one
        // exception: see `post_only_is_refused_by_name_and_only_on_a_limit_order`)
        for &t in TimeInForce::ALL.iter().filter(|t| **t != TimeInForce::PostOnly) {
            if c.supported_order_kinds.contains(&"market") {
                assert_eq!(preflight_order(&req(v, "market", t)), Ok(()), "{v}/market/{t:?}");
            }
        }
    }
    // (the coercion venues' coerced TIFs were admitted here until a lifetime could be NAMED by an
    // operator: they are refused by name now, `a_lifetime_the_adapter_would_coerce_is_refused_by_name`)
    // binance lane union: perp GTD must NOT be refused at the core edge
    assert_eq!(preflight_order(&req("binance", "limit", TimeInForce::Gtd)), Ok(()));
    // unknown venue (paper/sim ids) — no declared row, never refused here
    assert_eq!(preflight_order(&req("sim", "limit", TimeInForce::Ioc)), Ok(()));
    // combo requests: the Combo lowering owns capability
    let mut combo = req("oanda", "limit", TimeInForce::Gtc);
    combo.combo_legs = vec![vike_model::orders::order::ComboLeg { symbol: "A".into(), ratio: 1 }];
    assert_eq!(preflight_order(&combo), Ok(()));
}

/// POST-ONLY at the core edge: a venue whose row maps it takes a limit order, every other venue
/// refuses it BY NAME (`TIF_UNSUPPORTED: tif=PostOnly venue=<v>`, never a plain GTC limit that
/// could cross), and a post-only order that is not a limit order is refused on EVERY venue,
/// including the ones that support it (`POST_ONLY_NEEDS_LIMIT`).
#[test]
fn post_only_is_refused_by_name_and_only_on_a_limit_order() {
    use TimeInForce::PostOnly;
    for &v in vike_model::VENUES {
        let c = caps_for(v);
        let limit = preflight_order(&req(v, "limit", PostOnly));
        if c.supports_post_only {
            assert_eq!(limit, Ok(()), "{v} maps post-only");
        } else {
            assert_eq!(
                limit.expect_err(v).to_string(),
                format!("TIF_UNSUPPORTED: tif=PostOnly venue={v}"),
                "{v} has no post-only order: refused by name"
            );
        }
        for kind in ["market", "stop"] {
            if c.supported_order_kinds.contains(&kind) {
                assert_eq!(
                    preflight_order(&req(v, kind, PostOnly)).expect_err(v).to_string(),
                    format!("POST_ONLY_NEEDS_LIMIT: kind={kind} venue={v}"),
                    "{v}/{kind}"
                );
            }
        }
    }
    // an unknown venue (paper/sim ids) is never judged here
    assert_eq!(preflight_order(&req("sim", "limit", PostOnly)), Ok(()));
}

/// A lifetime the adapter would RUN AS ANOTHER ONE (the table's `Coerced` rows) is refused by name:
/// the order that rests is not the one that was asked for (Hyperliquid's `fok` would be an `ioc` that
/// may part-fill; its `gtd` and `day` would rest forever). The default `gtc` is never coerced, so
/// nothing that worked is refused; every coerced row is a lifetime nothing in the tree ever sent.
#[test]
fn a_lifetime_the_adapter_would_coerce_is_refused_by_name() {
    use TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
    for (venue, tif, becomes) in [
        ("hyperliquid", Fok, "Ioc"),
        ("hyperliquid", Gtd, "Gtc"),
        ("hyperliquid", Day, "Gtc"),
        ("polymarket", Ioc, "Fok"),
        ("polymarket", Day, "Gtc"),
        ("alpaca", Gtd, "Gtc"),
    ] {
        assert_eq!(
            preflight_order(&req(venue, "limit", tif)).expect_err(venue).to_string(),
            format!("TIF_COERCED: tif={tif:?} would run as {becomes} venue={venue}"),
            "{venue}/{tif:?}"
        );
    }
    // the honest rows and the default are untouched, and a market order has no lifetime to coerce
    assert_eq!(preflight_order(&req("hyperliquid", "limit", Gtc)), Ok(()));
    assert_eq!(preflight_order(&req("hyperliquid", "limit", Ioc)), Ok(()));
    assert_eq!(preflight_order(&req("polymarket", "limit", Fok)), Ok(()));
    assert_eq!(preflight_order(&req("polymarket", "limit", Gtd)), Ok(()));
    assert_eq!(preflight_order(&req("hyperliquid", "market", Fok)), Ok(()));
}

/// IBKR cannot send a good-till-date order (the date is never wired): refused by name, not left to
/// the venue's own rejection.
#[test]
fn ibkr_refuses_good_till_date_and_post_only_by_name() {
    for tif in [TimeInForce::Gtd, TimeInForce::PostOnly] {
        assert_eq!(
            preflight_order(&req("ibkr", "limit", tif)).expect_err("ibkr").to_string(),
            format!("TIF_UNSUPPORTED: tif={tif:?} venue=ibkr")
        );
    }
    for tif in [TimeInForce::Gtc, TimeInForce::Day, TimeInForce::Ioc, TimeInForce::Fok] {
        assert_eq!(preflight_order(&req("ibkr", "limit", tif)), Ok(()), "{tif:?}");
    }
}

/// Dukascopy's one trigger kind is the stop-loss of an open position (`StrategyBridge.java`'s `handleStop`):
/// a `stop` reaches the sidecar, every other trigger kind is refused at the core edge, with the words the
/// operator reads (`--type stop` used to answer `TRIGGER_UNSUPPORTED: kind=stop venue=dukascopy`).
#[test]
fn dukascopy_places_a_stop_loss_and_refuses_every_other_trigger_kind() {
    use TimeInForce::Gtc;
    assert_eq!(preflight_order(&req("dukascopy", "stop", Gtc)), Ok(()));
    for kind in ["stop_limit", "take_profit"] {
        assert_eq!(
            preflight_order(&req("dukascopy", kind, Gtc)),
            Err(PreflightDeny::Trigger { kind: kind.into(), venue: "dukascopy".into() }),
            "{kind}"
        );
    }
}

/// The refusals, category by category, with the machine-readable reason strings pinned
/// verbatim (`CATEGORY_CONDITION: key=value`).
#[test]
fn preflight_refuses_the_silent_lie_class() {
    use TimeInForce::{Fok, Gtc, Ioc};
    // TIF: aster/ig would silently rest GTC (the flip this task ships)
    let deny = preflight_order(&req("aster", "limit", Ioc)).unwrap_err();
    assert_eq!(deny.to_string(), "TIF_UNSUPPORTED: tif=Ioc venue=aster");
    let deny = preflight_order(&req("ig", "limit", Fok)).unwrap_err();
    assert_eq!(deny.to_string(), "TIF_UNSUPPORTED: tif=Fok venue=ig");
    // TIF: a flipped venue's venue-side deny now fires earlier, at the core edge
    let deny = preflight_order(&req("bybit", "limit", TimeInForce::Gtd)).unwrap_err();
    assert_eq!(deny.to_string(), "TIF_UNSUPPORTED: tif=Gtd venue=bybit");
    // binance Day: refused on BOTH lanes venue-side → refused here too
    assert!(preflight_order(&req("binance", "limit", TimeInForce::Day)).is_err());
    // trigger kinds: a venue with no native trigger order refuses a stop (polymarket's CLOB has
    // only limit orders on outcome tokens, so no adapter of it will ever place one)
    let deny = preflight_order(&req("polymarket", "stop", Gtc)).unwrap_err();
    assert_eq!(deny.to_string(), "TRIGGER_UNSUPPORTED: kind=stop venue=polymarket");
    let deny = preflight_order(&req("binance", "take_profit", Gtc)).unwrap_err();
    assert_eq!(deny.to_string(), "TRIGGER_UNSUPPORTED: kind=take_profit venue=binance");
    // non-trigger kind: fxcm's shim now HAS a true-market placement, so `"market"` is honest
    // there and passes — fxcm's row was the LAST declared venue denying a non-trigger kind.
    assert_eq!(preflight_order(&req("fxcm", "market", Gtc)), Ok(()));
    // fxcm's stop-loss RESTS at the venue since 2026-10-26 (bound to one trade, `vike_fxcm::exit`): `stop` passes the
    // core edge, and the trigger kinds the venue's `S` / `L` cannot express still do not.
    assert_eq!(preflight_order(&req("fxcm", "stop", Gtc)), Ok(()));
    let deny = preflight_order(&req("fxcm", "take_profit", Gtc)).unwrap_err();
    assert_eq!(deny.to_string(), "TRIGGER_UNSUPPORTED: kind=take_profit venue=fxcm");
    let deny = preflight_order(&req("fxcm", "stop_limit", Gtc)).unwrap_err();
    assert_eq!(deny.to_string(), "TRIGGER_UNSUPPORTED: kind=stop_limit venue=fxcm");
    // With that flip, `PreflightDeny::OrderKind` is reachable through `preflight_order` ONLY
    // for an `Unrecognized` kind (`preflight_refuses_an_unrecognised_kind_on_every_venue`):
    // every declared venue serves both non-trigger kinds, and an UNDECLARED venue is skipped
    // outright (no declared row can classify it). Both halves of that reasoning are asserted
    // below so a future row that drops "market"/"limit" re-arms the class instead of silently
    // shipping a lie; the variant's machine-readable rendering stays pinned directly.
    for &v in vike_model::VENUES {
        let kinds = caps_for(v).supported_order_kinds;
        assert!(kinds.contains(&"market") && kinds.contains(&"limit"), "{v}: non-trigger kinds");
    }
    assert_eq!(preflight_order(&req("nasdaq", "market", Gtc)), Ok(()), "undeclared venue skipped");
    let deny = PreflightDeny::OrderKind { kind: "market".into(), venue: "nasdaq".into() };
    assert_eq!(deny.to_string(), "ORDER_KIND_UNSUPPORTED: kind=market venue=nasdaq");
    // margin: an explicit Isolated request no adapter honors (binance ignores the field)
    let mut iso = req("binance", "limit", Gtc);
    iso.margin_mode = Some(MarginMode::Isolated);
    let deny = preflight_order(&iso).unwrap_err();
    assert_eq!(deny.to_string(), "MARGIN_MODE_UNSUPPORTED: mode=Isolated venue=binance");
    // margin: okx honors Isolated per order (passes) but denies Cash (spot-only mode)
    let mut okx_iso = req("okx", "limit", Gtc);
    okx_iso.margin_mode = Some(MarginMode::Isolated);
    assert_eq!(preflight_order(&okx_iso), Ok(()));
    let mut okx_cash = req("okx", "limit", Gtc);
    okx_cash.margin_mode = Some(MarginMode::Cash);
    let deny = preflight_order(&okx_cash).unwrap_err();
    assert_eq!(deny.to_string(), "MARGIN_MODE_UNSUPPORTED: mode=Cash venue=okx");
    // a STORED upper-case word is not ours: `Unrecognized`, refused (the input door folds case)
    assert!(preflight_order(&req("deribit", "STOP", Gtc)).is_err());
}

/// Deribit's native trigger orders (`vike_deribit::client`'s `order_params_on`) are declared, so
/// the preflight lets all three trigger kinds through and the bridge, not the core edge, judges
/// the rest (an option, a missing trigger price). This used to be the refused example.
#[test]
fn deribit_declares_its_native_trigger_orders() {
    use TimeInForce::Gtc;
    for kind in ["stop", "stop_limit", "take_profit"] {
        assert_eq!(preflight_order(&req("deribit", kind, Gtc)), Ok(()), "deribit/{kind}");
    }
    assert_eq!(
        caps_for("deribit").trigger_types,
        &[TriggerType::StopLoss, TriggerType::TakeProfit],
        "stop-loss and take-profit, both native"
    );
}

/// Case folding lives in the INPUT door now, not in the preflight: a mixed-case operator word
/// parses to its canonical kind (`OrderType::parse`) and is judged exactly as the lowercase word
/// was, while the same text arriving through the STORED door is `Unrecognized` and refused.
#[test]
fn preflight_matches_mixed_case_spellings() {
    use TimeInForce::Gtc;
    let parsed = |venue: &str, word: &str, tif: TimeInForce| OrderRequest {
        order_type: OrderType::parse(word).expect("a canonical word in another case"),
        ..req(venue, "market", tif)
    };
    // "Stop" (mixed case) is binance's native trigger kind — must still be ACCEPTED.
    assert_eq!(preflight_order(&parsed("binance", "Stop", Gtc)), Ok(()));
    // "MaRkEt" is a supported kind everywhere it's declared — must still be ACCEPTED.
    assert_eq!(preflight_order(&parsed("aster", "MaRkEt", Gtc)), Ok(()));
    // "Limit" (mixed case) still hits the TIF branch and is refused for aster's unaccepted IOC.
    let deny = preflight_order(&parsed("aster", "Limit", TimeInForce::Ioc)).unwrap_err();
    assert_eq!(deny.to_string(), "TIF_UNSUPPORTED: tif=Ioc venue=aster");
    // the same text through the stored door is not ours
    let deny = preflight_order(&req("binance", "Stop", Gtc)).unwrap_err();
    assert_eq!(deny.to_string(), "ORDER_KIND_UNSUPPORTED: kind=Stop venue=binance");
}

/// An UNRECOGNISED kind (only the stored door mints one) is refused on EVERY venue — a roster
/// venue, an undeclared paper/sim id and a combo alike — with its text verbatim. It used to pass
/// ("no row can claim it, venue side owns it"), and the venue side coerced it: alpaca, oanda,
/// ibkr, ig and the binance perp lane sent an unknown kind as a MARKET order.
#[test]
fn preflight_refuses_an_unrecognised_kind_on_every_venue() {
    use TimeInForce::Gtc;
    for &v in vike_model::VENUES.iter().chain(["sim", "nasdaq"].iter()) {
        let deny = preflight_order(&req(v, "market_close", Gtc)).unwrap_err();
        assert_eq!(
            deny.to_string(),
            format!("ORDER_KIND_UNSUPPORTED: kind=market_close venue={v}")
        );
    }
    let mut combo = req("deribit", "stop_market", Gtc);
    combo.combo_legs = vec![vike_model::orders::order::ComboLeg { symbol: "A".into(), ratio: 1 }];
    assert_eq!(
        preflight_order(&combo),
        Err(PreflightDeny::OrderKind { kind: "stop_market".into(), venue: "deribit".into() })
    );
    // an empty word is not "market" either (the type's default is; a stored "" is not)
    assert!(preflight_order(&req("binance", "", Gtc)).is_err());
}

/// `LiveDataCaps::supports` projects each verb onto its field (the seam the vike-data
/// `require_live_verb` helper drives refusals off).
#[test]
fn live_verb_projection_matches_fields() {
    let d = LiveDataCaps { bars: true, quotes: false, trades: true, book: false, depth: true };
    assert!(d.supports(LiveVerb::Bars));
    assert!(!d.supports(LiveVerb::Quotes));
    assert!(d.supports(LiveVerb::Trades));
    assert!(!d.supports(LiveVerb::Book));
    assert!(d.supports(LiveVerb::Depth));
    for v in [LiveVerb::Bars, LiveVerb::Quotes, LiveVerb::Trades, LiveVerb::Book, LiveVerb::Depth] {
        assert!(!LiveDataCaps::NONE.supports(v));
    }
}

/// The capability rows are written in strings; the type is the vocabulary. `ORDER_KINDS` is
/// every recognised `OrderType`'s word, in declaration order, and `TRIGGER_KINDS` exactly the
/// ones `OrderType::is_trigger` names, so a row's string and the request's kind cannot drift.
#[test]
fn order_kinds_are_the_known_order_types() {
    let known: Vec<&str> = OrderType::KNOWN.iter().map(OrderType::as_str).collect();
    assert_eq!(ORDER_KINDS, known.as_slice());
    let triggers: Vec<&str> =
        OrderType::KNOWN.iter().filter(|k| k.is_trigger()).map(OrderType::as_str).collect();
    assert_eq!(TRIGGER_KINDS, triggers.as_slice());
}
