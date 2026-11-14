use super::*;

/// The lanes whose reconcile figure is wallet cash, each read from its parser; a spot lane's spelling is not.
#[test]
fn balance_is_wallet_cash_on_the_measured_perpetual_lanes_only() {
    for (venue, symbol) in [
        ("binance", "BTCUSDT.P"),
        ("binance", "BTCUSDT_261225.P"),
        ("aster", "BTCUSDT.P"),
        ("bybit", "BTCUSDT"),
        ("okx", "BTC-USDT-SWAP"),
    ] {
        assert!(balance_is_wallet_cash(venue, symbol), "{venue} {symbol}");
    }
    for (venue, symbol) in [
        ("binance", "BTCUSDT"),
        ("aster", "BTCUSDT"),
        ("bybit", "BTCUSDT.S"),
        ("okx", "BTC-USDT"),
        // neither a spot pair nor a derivative in okx's spelling: not a measured lane
        ("okx", "BTCUSDT"),
        ("hyperliquid", "BTC"),
        ("deribit", "BTC-PERPETUAL"),
        ("ibkr", "AAPL"),
        ("alpaca", "AAPL"),
        ("polymarket", "x"),
        ("oanda", "EUR_USD"),
        ("ctrader", "EURUSD"),
        ("fxcm", "EUR/USD"),
        ("ig", "CS.D.EURUSD.MINI.IP"),
        ("dukascopy", "EURUSD"),
        ("no-such-venue", "BTCUSDT.P"),
    ] {
        assert!(!balance_is_wallet_cash(venue, symbol), "{venue} {symbol}");
    }
}

/// What this proves, precisely: [`capabilities_for`]'s match arms map each venue STRING to the const
/// this module names for it — a transposed-arm typo is loud here. It proves nothing about
/// whether a const's VALUES describe the adapter; the module doc lists the checks that do.
/// (Kept alongside `every_roster_venue_has_a_declared_row`, which subsumes it for the roster,
/// because this one names each pair literally and so reads as the registry's own table.)
#[test]
fn registry_maps_every_known_venue() {
    assert_eq!(capabilities_for("binance"), BINANCE);
    assert_eq!(capabilities_for("bybit"), BYBIT);
    assert_eq!(capabilities_for("okx"), OKX);
    assert_eq!(capabilities_for("deribit"), DERIBIT);
    assert_eq!(capabilities_for("oanda"), OANDA);
    assert_eq!(capabilities_for("ig"), IG);
    assert_eq!(capabilities_for("fxcm"), FXCM);
    assert_eq!(capabilities_for("dukascopy"), DUKASCOPY);
    assert_eq!(capabilities_for("polymarket"), POLYMARKET);
    assert_eq!(capabilities_for("aster"), ASTER);
    assert_eq!(capabilities_for("hyperliquid"), HYPERLIQUID);
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
        let rows: &[(&str, VenueCapabilities)] = &[
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
            .unwrap_or_else(|| panic!("no VenueCapabilities row declared for roster venue {v}"));
        assert_eq!(capabilities_for(v), *want, "{v}: registry must serve its declared row");
    }
}

#[test]
fn unknown_venue_is_conservative_unsupported() {
    let c = capabilities_for("nasdaq");
    assert_eq!(c, VenueCapabilities::UNSUPPORTED);
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
    assert_eq!(VenueCapabilities::default(), VenueCapabilities::UNSUPPORTED);
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
        assert!(capabilities_for(v).allows_modify(), "{v} should allow modify");
    }
    // blocked: no native modify wired
    for v in ["polymarket", "alpaca", "unknown"] {
        assert!(!capabilities_for(v).allows_modify(), "{v} must block modify");
    }
}

/// cTrader's audited row (audit of the #498 known-stale `UNSUPPORTED` placeholder): a native
/// amend (`supports_modify`) + a spot-quote/trendbar `DataClient`, everything else conservative.
/// Pins the exact matrix so a regression that re-widens or re-narrows a field trips here.
#[test]
fn ctrader_capabilities_reflect_the_adapter() {
    let c = capabilities_for("ctrader");
    assert_eq!(c, CTRADER);
    // the two flipped-from-UNSUPPORTED fields
    assert!(c.supports_modify, "ctrader wires a native AmendOrder");
    assert_eq!(
        c.live_data,
        DataClientCapabilities {
            bars: true,
            quotes: true,
            trades: false,
            book: false,
            depth: false
        },
        "ctrader DataClient serves trendbars + spot quotes only"
    );
    assert!(c.has_live_data());
    // the fields the adapter does NOT wire — must stay conservative
    assert!(!c.supports_native_batch, "no batch endpoint wired");
    assert!(!c.supports_reduce_only, "order_to_new_order sets no reduceOnly");
    assert!(!c.supports_combo);
    assert_eq!(c.supported_tifs, &[TimeInForce::Gtc], "no explicit TIF sent → GTC default");
    assert!(c.backfill_bars, "KlineSourceCtrader is the credentialed bars source");
    assert!(!c.backfill_ticks, "no cTrader tick collector: the tick request is not wired");
}

/// Only Binance + Aster wire a native batch endpoint; everyone else uses the fan-out default.
/// (Property assertions go through the non-const `capabilities_for` so they aren't const-folded — the
/// registry equals the consts, pinned by `registry_maps_every_known_venue`.)
#[test]
fn native_batch_is_binance_aster_hyperliquid() {
    // Adapters that wire a native batch endpoint (binance/aster POST .../batchOrders ≤5;
    // hyperliquid's msgpack action batch).
    assert!(capabilities_for("binance").supports_native_batch);
    assert!(capabilities_for("aster").supports_native_batch);
    assert!(capabilities_for("hyperliquid").supports_native_batch);
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
        assert!(!capabilities_for(v).supports_native_batch, "{v} must not claim native batch");
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
    assert!(capabilities_for("deribit").supports_combo, "deribit wires resolve-at-submit combos");
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
        assert!(!capabilities_for(v).supports_combo, "{v} must not claim combo support");
    }
}

/// reduce-only is a perp/options concept: the crypto perps + deribit build it; FX/CFD,
/// dukascopy (unverifiable), and polymarket do not.
#[test]
fn reduce_only_matches_derivatives_venues() {
    for v in ["binance", "bybit", "okx", "deribit"] {
        assert!(capabilities_for(v).supports_reduce_only, "{v} builds reduceOnly");
    }
    for v in ["oanda", "ig", "fxcm", "dukascopy", "polymarket"] {
        assert!(!capabilities_for(v).supports_reduce_only, "{v} does not wire reduceOnly");
    }
}

/// The live-data matrix: every verb each venue with a `DataClient` actually serves, exactly as
/// its feed implements it — the crypto depth-vs-book split, polymarket's bars-less shape, ibkr's
/// three (no L2 since docs/decisions/0140), and deribit's full five since the datahub began serving its DOM.
#[test]
fn live_data_matrix_matches_the_feeds() {
    // crypto perps: bars + trades + depth (bybit/okx trades live-verified 2026-07-20 via each
    // crate's `*_trades_feed_smoke` against the real `DataClient::subscribe_trades` path).
    assert_eq!(
        BINANCE.live_data,
        DataClientCapabilities {
            bars: true,
            quotes: false,
            trades: true,
            book: false,
            depth: true
        }
    );
    assert_eq!(
        BYBIT.live_data,
        DataClientCapabilities {
            bars: true,
            quotes: false,
            trades: true,
            book: false,
            depth: true
        }
    );
    assert_eq!(BYBIT.live_data, OKX.live_data);
    assert_eq!(BYBIT.live_data, BINANCE.live_data);
    // polymarket: quotes + trades + book, no bars, no depth
    assert_eq!(
        POLYMARKET.live_data,
        DataClientCapabilities {
            bars: false,
            quotes: true,
            trades: true,
            book: true,
            depth: false
        }
    );
    // ibkr: bars, quotes and trades on both backends of `DataClientBusSpecIbkr` (the socket one
    // over ibapi, the cpapi one over the CP Gateway `smd` topic); NO book and NO depth since docs/decisions/0140 —
    // the accounts hold no depth entitlement, and both clients refuse the two verbs off this row.
    // (This row said `DataClientCapabilities::NONE` from 2026-07-14 until the pump-spec cross-pin caught it,
    // then all five until 0140.)
    assert_eq!(
        IBKR.live_data,
        DataClientCapabilities {
            bars: true,
            quotes: true,
            trades: true,
            book: false,
            depth: false
        }
    );
    // oanda: the SPLIT-PLANE feed — quotes stream (the chunked-HTTP pricing stream), bars
    // poll the candles REST. No trade tape is published, and the pricing ladder is an
    // unsequenced top-of-book snapshot rather than a delta-synced book → book/depth false.
    assert_eq!(
        OANDA.live_data,
        DataClientCapabilities {
            bars: true,
            quotes: true,
            trades: false,
            book: false,
            depth: false
        }
    );
    // deribit (split-plane I9): bars + quotes + trades + the LOSSLESS book lane, and — since
    // 2026-10-04 — the conflating DOM `depth` lane, the top levels of that same `book.*` fold
    // (the depth lane of `vike_deribit::feed::DataClientSpecDeribit`). Both L2 lanes, the
    // one venue that serves both.
    assert_eq!(
        DERIBIT.live_data,
        DataClientCapabilities { bars: true, quotes: true, trades: true, book: true, depth: true }
    );
    // ig serves bars+quotes only — the dealer-venue shape: L1 with no ladder, no tape.
    assert_eq!(
        IG.live_data,
        DataClientCapabilities {
            bars: true,
            quotes: true,
            trades: false,
            book: false,
            depth: false
        }
    );
    // no live feed at all for these
    for v in ["fxcm", "dukascopy"] {
        assert!(!capabilities_for(v).has_live_data(), "{v} has no live DataClient");
    }
    // ...and every OTHER roster venue does have one. Stated as the complement so a venue can
    // never fall out of both lists. The non-circular tie for exactly this partition is
    // `venue_capabilities_cross_pin_the_pump_spec` in vike-bridge-core.
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
        assert!(capabilities_for(v).has_live_data(), "{v} has a live DataClient");
    }
}

/// TIF sets track the adapters: OANDA maps the full five; the flipped venues (tif step-2)
/// declare exactly the set their adapter maps 1:1 (everything else loud-denied at submit);
/// the unflipped GTC-hardcoders stay `&[Gtc]`.
#[test]
fn time_in_force_sets_reflect_what_the_adapter_wires() {
    let oanda = capabilities_for("oanda");
    assert_eq!(oanda.supported_tifs.len(), 5, "oanda maps the five lifetimes, not post-only");
    assert!(!oanda.supported_tifs.contains(&TimeInForce::PostOnly));
    assert!(oanda.supported_tifs.contains(&TimeInForce::Day));
    assert!(oanda.supported_tifs.contains(&TimeInForce::Gtd));
    // binance: the row stays the CONSERVATIVE spot∩perp intersection — the perp lane
    // additionally wires native GTD, but this registry cannot distinguish the lanes (one
    // "binance" key), so Gtd is deliberately NOT declared here; the per-lane truth is
    // venue_time_in_force's "binance-perp" row (see the BINANCE row doc).
    for v in ["binance", "bybit", "okx"] {
        assert_eq!(
            capabilities_for(v).supported_tifs,
            &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::PostOnly],
            "{v}: GTC/IOC/FOK/PostOnly declared (binance = lane intersection, perp-GTD lives in \
                 venue_time_in_force)"
        );
    }
    assert_eq!(
        capabilities_for("deribit").supported_tifs,
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
        assert_eq!(capabilities_for(v).supported_tifs, &[TimeInForce::Gtc], "{v} is GTC-only");
    }
    assert_eq!(
        capabilities_for("polymarket").supported_tifs,
        &[TimeInForce::Gtc, TimeInForce::Fok]
    );
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
/// **The non-circular check is `venue_capabilities_cross_pin_the_backfill_table` in vike-backfill**,
/// which compares these two fields to `backfill_capabilities(BackfillProvider::Venue, v)` — a table with a
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
            // bars from the keyless public candle archive (`KlineSourceFxcm`); no ticks anywhere.
            ("fxcm",        true,  false),
            ("polymarket",  false, false),
            // bid-side bars through the datahub's CREDENTIALED lane (`KlineSourceCtrader`); no ticks.
            ("ctrader",     true,  false),
            ("alpaca",      false, false),
            // vike:new-venue:row ("{venue}",  false, false), // TODO(new-venue: {venue}): flip when a collector lands
        ];
    assert_eq!(rows.len(), vike_model::VENUES.len(), "one pinned row per roster venue");
    for (v, bars, ticks) in rows {
        let c = capabilities_for(v);
        assert_eq!(c.backfill_bars, *bars, "{v} backfill_bars");
        assert_eq!(c.backfill_ticks, *ticks, "{v} backfill_ticks");
    }
}

// ── expanded axes (w2-task-5) ────────────────────────────────────────────────────────────

/// The NEW-axis matrix, pinned verbatim per venue (the tif/margin matrix-test discipline):
/// (venue, order_kinds, trigger_types, accepted_tifs, margin_modes, max_batch). Any drift in
/// a row is loud here; each bridge's own `capabilities_test` additionally ties its row to the
/// adapter code the value was read from.
#[test]
fn expanded_axes_matrix_is_pinned() {
    use MarginMode::{Cash, Cross, Isolated};
    use TimeInForce::{AtClose, AtOpen, Day, Fok, Gtc, Gtd, Ioc, PostOnly};
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
    // A blank line apart where neighbouring rows flip in separate commits, so each cherry-picks alone.
    #[rustfmt::skip]
        let rows: &[Row] = &[
            ("binance",     &["market", "limit", "stop", "stop_limit", "take_profit"],
                &[StopLoss, TakeProfit], &[Gtc, Ioc, Fok, Gtd, PostOnly],  &[Cross], 5),

            ("bybit",       &["market", "limit", "stop", "stop_limit", "take_profit"],
                &[StopLoss, TakeProfit], &[Gtc, Ioc, Fok, PostOnly],       &[Cross], 0),

            ("okx",         &["market", "limit", "stop", "stop_limit", "take_profit", "trailing"],
                &[StopLoss, TakeProfit], &[Gtc, Ioc, Fok, PostOnly],       &[Cross, Isolated], 0),
            ("aster",       MLS, &[StopLoss], &[Gtc],                      &[Cross], 5),
            ("deribit",     &["market", "limit", "stop", "stop_limit", "take_profit", "market_to_limit", "trailing"],
                &[StopLoss, TakeProfit], &[Gtc, Ioc, Fok, Day, PostOnly], &[Cross], 0),

            ("oanda",       &["market", "limit", "stop", "take_profit"], &[StopLoss, TakeProfit],
                ALL5,                                                      &[Cross], 0),
            ("alpaca",      &["market", "limit", "stop", "stop_limit"], &[StopLoss],
                &[Day, Gtc, Ioc, Fok, Gtd],                               &[Cross], 0),
            ("ig",          MLS, &[StopLoss], &[Gtc],                      &[Cross], 0),
            ("fxcm",        MLS, &[StopLoss], &[Gtc],                      &[Cross], 0),
            ("dukascopy",   &["market", "limit", "stop", "stop_limit", "take_profit"],
                &[StopLoss, TakeProfit], &[Gtc],                           &[Cross], 0),

            ("polymarket",  ML,  &[],         ALL5,                        &[Cash],  0),
            ("ibkr",        &["market", "limit", "stop", "stop_limit", "trailing_take_profit", "pegged"], &[StopLoss],
                &[Gtc, Day, Ioc, Fok, Gtd, AtOpen, AtClose],               &[Cross], 0),
            ("ctrader",     &["market", "limit", "stop", "stop_limit"], &[StopLoss],
                &[Gtc],                                                    &[Cross], 0),
            ("hyperliquid", &["market", "limit", "stop", "stop_limit", "take_profit"],
                &[StopLoss, TakeProfit], &[Gtc, Ioc, Fok, Gtd, Day, PostOnly], &[Cross], usize::MAX),
            // vike:new-venue:row ("{venue}",     ML,  &[],         &[Gtc],                      &[Cross], 0), // TODO(new-venue: {venue}): pin what the adapter really wires
        ];
    assert_eq!(rows.len(), vike_model::VENUES.len(), "one pinned row per roster venue");
    for (venue, kinds, triggers, accepted, margins, max_batch) in rows {
        let c = capabilities_for(venue);
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
        let c = capabilities_for(v);
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
        let c = capabilities_for(v);
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
/// IG places a reduce-only MARKET only (`crates/bridges/ig/src/execution/mod.rs`'s `CLOSE_MARKET_ONLY`).
/// Dukascopy was the second until its sidecar took a reduce-only LIMIT as the native take-profit
/// (`StrategyBridge.java`'s `handleStop` with `Protection.TAKE_PROFIT`): its client refuses one only for a jar that
/// predates that, which a static row cannot say. Bybit's are not refusals but UNPROVEN kinds: no reduce-only
/// stop-limit or take-profit was ever placed and the venue's docs do not state the flag for them (the `BYBIT` row's
/// doc), so the row leaves them out until a demo placement proves them. Every other roster venue's adapter carries the flag onto the
/// wire, routes it (FXCM, cTrader) or ignores it, and refuses none for its kind, so its
/// `reduce_only_kinds` IS its `supported_order_kinds`.
/// Dukascopy refuses a reduce-only stop-limit and take-profit by name: the venue holds both as ENTRY
/// orders, which open a position when hit (`crates/bridges/dukascopy/src/execution/mod.rs`'s `submit`).
const REDUCE_ONLY_EXCEPTIONS: &[(&str, &[&str])] = &[
    ("ig", &["market"]),
    ("bybit", &["market", "limit", "stop"]),
    ("dukascopy", &["market", "limit", "stop"]),
];

/// `reduce_only_kinds` per venue class: the measured exception is pinned VERBATIM, and every
/// other roster venue lists exactly what it accepts (PRESERVE: nothing that works today is refused).
/// A new venue passes with a row equal to its kinds and joins the exceptions only with a measured
/// adapter refusal.
#[test]
fn reduce_only_kinds_are_the_kind_set_except_where_the_adapter_refuses_by_name() {
    for &v in vike_model::VENUES {
        let c = capabilities_for(v);
        match REDUCE_ONLY_EXCEPTIONS.iter().find(|(event, _)| *event == v) {
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
        let c = capabilities_for(v);
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
    // Bybit: a reduce-only stop-limit is unproven (never placed), so it is refused; the entry stop-limit and
    // the reduce-only stop, limit and market close pass.
    let deny = preflight_order(&reduce("bybit", "stop_limit")).unwrap_err();
    assert_eq!(deny.to_string(), "REDUCE_ONLY_UNSUPPORTED: kind=stop_limit venue=bybit");
    assert_eq!(preflight_order(&req("bybit", "stop_limit", Gtc)), Ok(()), "an entry is no exit");
    for kind in ["market", "limit", "stop"] {
        assert_eq!(preflight_order(&reduce("bybit", kind)), Ok(()), "bybit/{kind}");
    }
    // Every other venue: each kind it accepts passes as a reduce-only order too.
    for &v in vike_model::VENUES {
        if REDUCE_ONLY_EXCEPTIONS.iter().any(|(event, _)| *event == v) {
            continue;
        }
        for k in capabilities_for(v).supported_order_kinds {
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
///   else (the cases are counted: IG's limit and stop, bybit's unproven stop-limit, and dukascopy's stop-limit
///   and if-touched ENTRY orders, which the venue never holds reduce-only, per TIF and margin).
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
    let want = [
        ("bybit", "stop_limit"),
        ("bybit", "take_profit"),
        ("dukascopy", "stop_limit"),
        ("dukascopy", "take_profit"),
        ("ig", "limit"),
        ("ig", "stop"),
    ]
    .map(|(v, k)| (v.to_string(), k.to_string()));
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
    use vike_model::orders::order::{OrderBracket, build_bracket};
    for &v in vike_model::VENUES {
        let spec = OrderBracket {
            venue: v.to_string(),
            symbol: "X".to_string(),
            side: 1,
            qty: 1.0,
            entry_price: Some(100.0),
            stop_loss: 95.0,
            take_profit: 110.0,
            account: None,
            order_client_id: None,
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

/// The legs of a ladder on `venue`: a market entry, a stop for its whole size, two targets; the exits
/// reduce-only and linked OUO (`build_ladder`).
fn ladder_legs(venue: &str) -> Vec<OrderRequest> {
    use vike_model::orders::order::{LadderSpec, LadderTarget, build_ladder, build_order_list};
    let spec = LadderSpec {
        venue: venue.to_string(),
        symbol: "X".to_string(),
        side: 1,
        qty: 3.0,
        entry_price: None,
        stop_loss: 95.0,
        targets: vec![
            LadderTarget { price: 110.0, qty: 1.0 },
            LadderTarget { price: 120.0, qty: 2.0 },
        ],
        account: None,
    };
    let order_client_ids: Vec<String> = ["e", "s", "t1", "t2"].map(String::from).to_vec();
    build_order_list(&build_ladder(&spec).expect("a ladder"), &order_client_ids).expect("its legs")
}

fn whole_exit_why(venue: &str) -> &'static str {
    WHOLE_EXIT_VENUES.iter().find(|w| w.venue == venue).expect("a whole-exit row").why
}

/// **A ladder's whole answer, per roster venue, DERIVED from its row** and pinned here, so a row that
/// moves says so: IG and polymarket cannot rest the reduce-only stop (polymarket has no stop at all),
/// alpaca cannot change a resting order (`venue_amend_mode`'s `REFUSED_VENUES`), and the three
/// whole-exit venues are the pinned exception. The refused leg is the stop, leg 1.
#[test]
fn order_list_refusal_of_a_ladder_is_derived_per_roster_venue() {
    let got: Vec<(String, Option<String>)> = vike_model::VENUES
        .iter()
        .map(|&v| (v.to_string(), order_list_refusal(v, &ladder_legs(v))))
        .collect();
    let whole = |v: &str| Some(format!("OUO_WHOLE_EXIT: leg=1 venue={v}: {}", whole_exit_why(v)));
    let want: Vec<(String, Option<String>)> = [
        ("binance", None),
        ("bybit", None),
        ("okx", None),
        ("deribit", None),
        ("oanda", None),
        ("ig", Some("REDUCE_ONLY_UNSUPPORTED: leg=1 kind=stop venue=ig".to_string())),
        ("fxcm", whole("fxcm")),
        ("dukascopy", whole("dukascopy")),
        (
            "polymarket",
            Some("REDUCE_ONLY_UNSUPPORTED: leg=1 kind=stop venue=polymarket".to_string()),
        ),
        ("ibkr", None),
        ("ctrader", whole("ctrader")),
        (
            "alpaca",
            Some(
                "OUO_AMEND_REFUSED: leg=1 kind=stop venue=alpaca: the venue cannot change this \
                 kind of resting order, so a sibling's fill could not shrink it"
                    .to_string(),
            ),
        ),
        ("aster", None),
        ("hyperliquid", None),
    ]
    .into_iter()
    .map(|(v, w)| (v.to_string(), w))
    .collect();
    assert_eq!(got, want);
    // An unknown venue (a paper or sim id) is not judged, as the leg preflight's compat law says.
    assert_eq!(order_list_refusal("sim", &ladder_legs("sim")), None);
}

/// **The whole-exit table, both ways.** Its rows are the three ruled venues, each a roster venue with its
/// words, its bridge's words and its reopen condition. On each, the SAME list linked OCO goes (the row alone
/// would let it: the exception is not redundant) and linked OUO is refused whole, naming its first OUO leg.
/// On every other roster venue the table refuses nothing: an OUO list's answer is the derived one.
#[test]
fn the_whole_exit_table_refuses_an_ouo_list_on_its_venues_and_nowhere_else() {
    use vike_model::orders::order::ContingencyType::{Oco, Ouo};
    let names: Vec<&str> = WHOLE_EXIT_VENUES.iter().map(|w| w.venue).collect();
    assert_eq!(names, ["fxcm", "dukascopy", "ctrader"]);
    for w in WHOLE_EXIT_VENUES {
        assert!(vike_model::VENUES.contains(&w.venue), "{}: not a roster venue", w.venue);
        for (field, text) in
            [("why", w.why), ("bridge_words", w.bridge_words), ("reopen", w.reopen)]
        {
            assert!(!text.trim().is_empty(), "{}: empty {field}", w.venue);
        }
    }
    // The ladder's exits linked by `contingency`; the entry stays their OTO parent.
    let linked = |v: &str, contingency| -> Vec<OrderRequest> {
        ladder_legs(v)
            .into_iter()
            .map(|o| match o.contingency_type {
                Some(Ouo) => OrderRequest { contingency_type: Some(contingency), ..o },
                _ => o,
            })
            .collect()
    };
    for &v in vike_model::VENUES {
        let oco = order_list_refusal(v, &linked(v, Oco));
        let ouo = order_list_refusal(v, &linked(v, Ouo));
        if names.contains(&v) {
            assert_eq!(oco, None, "{v}: the row alone lets the list go");
            assert_eq!(
                ouo,
                Some(format!("OUO_WHOLE_EXIT: leg=1 venue={v}: {}", whole_exit_why(v)))
            );
        } else {
            assert!(
                ouo.as_deref().is_none_or(|w| !w.starts_with("OUO_WHOLE_EXIT")),
                "{v}: refused by the table it is not in: {ouo:?}"
            );
        }
    }
}

/// **An OUO leg the node would replace cancel-first must shrink IN PLACE.** Two reduce-only limit targets linked
/// OUO, on every roster venue: polymarket is the one venue this rule refuses (no native amend, so a resting limit
/// changes only by cancel-then-new); alpaca is refused any amend and IG any reduce-only limit, by the other rules.
/// A reduce-only STOP may be cancel-then-new, being replaced new-first: binance, okx, aster and bybit move no resting
/// stop in place (`venue_amend_mode`'s `NO_NATIVE_KIND`) and their ladders still go. An ENTRY stop is replaced
/// cancel-first, so linked OUO it is refused there.
#[test]
fn an_ouo_leg_replaced_cancel_first_is_refused_where_the_venue_cannot_amend_it_in_place() {
    use TimeInForce::Gtc;
    use vike_model::orders::order::ContingencyType::{Oco, Ouo};
    let target = |v: &str, link: &str, contingency| OrderRequest {
        side: -1,
        reduce_only: true,
        contingency_type: Some(contingency),
        linked_order_ids: vec![link.to_string()],
        ..req(v, "limit", Gtc)
    };
    let pair = |v: &str, contingency| [target(v, "b", contingency), target(v, "a", contingency)];
    let refused: Vec<&str> = vike_model::VENUES
        .iter()
        .copied()
        .filter(|&v| {
            order_list_refusal(v, &pair(v, Ouo))
                .is_some_and(|w| w.starts_with("OUO_TARGET_AMEND_NOT_IN_PLACE"))
        })
        .collect();
    assert_eq!(refused, ["polymarket"], "the venues this rule refuses an OUO target on");
    assert_eq!(
        order_list_refusal("polymarket", &pair("polymarket", Ouo)).as_deref(),
        Some(
            "OUO_TARGET_AMEND_NOT_IN_PLACE: leg=0 kind=limit venue=polymarket: the venue changes \
             this kind of resting order only by cancelling it first, so a sibling's fill could \
             not shrink it in place"
        )
    );
    assert_eq!(
        order_list_refusal("polymarket", &pair("polymarket", Oco)),
        None,
        "OCO resizes nothing"
    );
    // okx and bybit amend a stop in place since their demo flips (#3043): only these are cancel-first
    for v in ["binance", "aster"] {
        assert_eq!(amend_mode_for(v, &OrderType::Stop), Some(AmendMode::CancelThenNew), "{v}");
        assert_eq!(order_list_refusal(v, &ladder_legs(v)), None, "{v}: its stop goes new-first");
        let entry_stop = OrderRequest {
            contingency_type: Some(Ouo),
            trigger_price: Some(1.0),
            ..req(v, "stop", Gtc)
        };
        let want = format!("OUO_TARGET_AMEND_NOT_IN_PLACE: leg=0 kind=stop venue={v}:");
        let got = order_list_refusal(v, &[entry_stop]);
        assert!(got.as_deref().is_some_and(|w| w.starts_with(&want)), "{v}: cancel-first: {got:?}");
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
        let capabilities = capabilities_for(v);
        let support = crate::venue_margin_support::venue_margin_support(v);
        for m in capabilities.margin_modes {
            assert!(
                support.offered_modes.contains(m),
                "{v}: order-honored mode {m:?} must be offered by the venue"
            );
        }
        assert!(
            support.offered_modes.contains(&capabilities.default_margin_mode),
            "{v}: the mode vike sends by default must be one the venue offers"
        );
    }
}

/// TODAY'S REALITY, relocated verbatim from the margin table when `default_margin_mode` moved
/// here: every venue vike trades perps on produces Cross; polymarket produces Cash. This pins
/// the current wire behavior.
///
/// Only okx's row is tied to adapter CODE (see
/// `crates/bridges/okx/src/execution/rest/margin_mode_tests.rs`'s `okx_default_margin_mode_matches_the_td_mode_builder`) —
/// it is the one adapter that emits a margin field at all. Every other row records the
/// account-side default that rules because the adapter sends nothing, which no test here can
/// prove.
///
/// ⚠ Read `hyperliquid`'s row below with the field's own "the default for assets that HAVE a
/// choice" caveat: 9 of its 232 assets are isolated-only and rule Isolated. This test pins the
/// per-VENUE declaration (which is what the field is); the per-ASSET narrowing is pinned in the
/// bridge, by `crates/bridges/hyperliquid/src/instrument/symbology_tests.rs`'s
/// `effective_margin_mode_is_per_asset_not_per_venue` — which asserts the two DISAGREE for an
/// isolated-only asset, so flipping this row to make them agree would break that test loudly.
#[test]
fn default_margin_mode_reflects_current_behavior() {
    for v in ["binance", "aster", "okx", "bybit", "hyperliquid", "deribit"] {
        assert_eq!(
            capabilities_for(v).default_margin_mode,
            MarginMode::Cross,
            "{v} is cross today"
        );
    }
    assert_eq!(capabilities_for("polymarket").default_margin_mode, MarginMode::Cash);
    // FX/equity also resolve to Cross (shared account margin)
    for v in ["oanda", "ig", "fxcm", "dukascopy", "alpaca", "ibkr", "ctrader"] {
        assert_eq!(capabilities_for(v).default_margin_mode, MarginMode::Cross, "{v}");
    }
    // The unknown-venue fallback keeps the same conservative value.
    assert_eq!(VenueCapabilities::UNSUPPORTED.default_margin_mode, MarginMode::Cross);
}

// ── preflight (w2-task-5) ────────────────────────────────────────────────────────────────

/// `order_type` goes through the STORED door (`OrderType::from_stored`): a canonical word is its
/// variant, anything else is `Unrecognized` verbatim.
fn req(venue: &str, order_type: &str, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        order_client_id: "c-pf".to_string(),
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
        let c = capabilities_for(v);
        for k in c.supported_order_kinds {
            assert_eq!(preflight_order(&req(v, k, Gtc)), Ok(()), "{v}/{k}");
        }
        for t in c.accepted_tifs {
            // a lifetime the adapter would run as another one is admitted by `accepted_tifs` (the
            // compatibility law) but refused by name now that one can be NAMED: see
            // `a_lifetime_the_adapter_would_coerce_is_refused_by_name`
            if matches!(
                crate::venue_time_in_force::venue_time_in_force(v, *t),
                crate::venue_time_in_force::TimeInForceOutcome::Coerced { .. }
            ) {
                continue;
            }
            assert_eq!(preflight_order(&req(v, "limit", *t)), Ok(()), "{v}/limit/{t:?}");
        }
        // market path carries no TIF axis anywhere — never TIF-refused (post-only and the auction
        // lifetimes are the exceptions: see `post_only_is_refused_by_name_and_only_on_a_limit_order`
        // and `an_auction_lifetime_is_judged_on_a_market_order_and_admitted_where_the_row_wires_it`)
        for &t in
            TimeInForce::ALL.iter().filter(|t| **t != TimeInForce::PostOnly && !t.is_auction())
        {
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
    combo.combo_legs = vec![vike_model::orders::order::OrderComboLeg {
        symbol: "A".into(),
        ratio: 1,
        limit: None,
    }];
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
        let c = capabilities_for(v);
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

/// IBKR has no post-only order: refused by name, not left to the venue's own rejection. A
/// good-till-date order passes since the socket wires its date and the paper smoke took it.
#[test]
fn ibkr_refuses_post_only_by_name_and_admits_good_till_date() {
    assert_eq!(
        preflight_order(&req("ibkr", "limit", TimeInForce::PostOnly))
            .expect_err("ibkr")
            .to_string(),
        "TIF_UNSUPPORTED: tif=PostOnly venue=ibkr"
    );
    for tif in
        [TimeInForce::Gtc, TimeInForce::Day, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::Gtd]
    {
        assert_eq!(preflight_order(&req("ibkr", "limit", tif)), Ok(()), "{tif:?}");
    }
}

/// Dukascopy's trigger kinds: a `stop` is the stop-loss of an open position (`StrategyBridge.java`'s
/// `handleStop`), a `stop_limit` and a `take_profit` the venue's own conditional ENTRY orders
/// (`TriggerEntry.java`), so all three pass the core edge, and the two entry kinds are refused
/// reduce-only (they would OPEN a position when hit).
#[test]
fn dukascopy_places_a_stop_loss_and_refuses_every_other_trigger_kind() {
    use TimeInForce::Gtc;
    for kind in ["stop", "stop_limit", "take_profit"] {
        assert_eq!(preflight_order(&req("dukascopy", kind, Gtc)), Ok(()), "{kind}");
    }
    for kind in ["stop_limit", "take_profit"] {
        let exit = OrderRequest { reduce_only: true, ..req("dukascopy", kind, Gtc) };
        assert_eq!(
            preflight_order(&exit),
            Err(PreflightDeny::ReduceOnly { kind: kind.into(), venue: "dukascopy".into() }),
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
    // aster: its shared family builder (decision 0186) builds no take-profit
    let deny = preflight_order(&req("aster", "take_profit", Gtc)).unwrap_err();
    assert_eq!(deny.to_string(), "TRIGGER_UNSUPPORTED: kind=take_profit venue=aster");
    // non-trigger kind: fxcm's shim now HAS a true-market placement, so `"market"` is honest
    // there and passes — fxcm's row was the LAST declared venue denying a non-trigger kind.
    assert_eq!(preflight_order(&req("fxcm", "market", Gtc)), Ok(()));
    // fxcm's stop-loss RESTS at the venue since 2026-10-26 (bound to one trade, `vike_fxcm::execution::exit`): `stop` passes the
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
        let kinds = capabilities_for(v).supported_order_kinds;
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

/// Deribit's native trigger orders (`vike_deribit::execution::rest`'s `order_params_on`) are declared, so
/// the preflight lets all three trigger kinds through and the bridge, not the core edge, judges
/// the rest (an option, a missing trigger price). This used to be the refused example.
#[test]
fn deribit_declares_its_native_trigger_orders() {
    use TimeInForce::Gtc;
    for kind in ["stop", "stop_limit", "take_profit"] {
        assert_eq!(preflight_order(&req("deribit", kind, Gtc)), Ok(()), "deribit/{kind}");
    }
    assert_eq!(
        capabilities_for("deribit").trigger_types,
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
    combo.combo_legs = vec![vike_model::orders::order::OrderComboLeg {
        symbol: "A".into(),
        ratio: 1,
        limit: None,
    }];
    assert_eq!(
        preflight_order(&combo),
        Err(PreflightDeny::OrderKind { kind: "stop_market".into(), venue: "deribit".into() })
    );
    // an empty word is not "market" either (the type's default is; a stored "" is not)
    assert!(preflight_order(&req("binance", "", Gtc)).is_err());
}

/// `DataClientCapabilities::supports` projects each verb onto its field (the seam the vike-data
/// `require_live_verb` helper drives refusals off).
#[test]
fn live_verb_projection_matches_fields() {
    let d = DataClientCapabilities {
        bars: true,
        quotes: false,
        trades: true,
        book: false,
        depth: true,
    };
    assert!(d.supports(DataClientVerb::Bars));
    assert!(!d.supports(DataClientVerb::Quotes));
    assert!(d.supports(DataClientVerb::Trades));
    assert!(!d.supports(DataClientVerb::Book));
    assert!(d.supports(DataClientVerb::Depth));
    for v in [
        DataClientVerb::Bars,
        DataClientVerb::Quotes,
        DataClientVerb::Trades,
        DataClientVerb::Book,
        DataClientVerb::Depth,
    ] {
        assert!(!DataClientCapabilities::NONE.supports(v));
    }
}

/// The label ↔ `DataClientVerb` maps are inverse on the four tick verbs, and `Bars` — whose stream label
/// is an interval — has none.
#[test]
fn the_stream_label_maps_are_inverse_and_bars_has_no_label() {
    use vike_model::StreamLabel;
    for &label in StreamLabel::ALL {
        assert_eq!(DataClientVerb::from(label).stream_label(), Some(label), "{label:?}");
    }
    assert_eq!(DataClientVerb::Bars.stream_label(), None);

    // Compile-time tie: a new `DataClientVerb` fails to build this match until it is decided whether it
    // is a tick lane (and so has a label) or not.
    fn is_tick_verb(verb: DataClientVerb) -> bool {
        match verb {
            DataClientVerb::Bars => false,
            DataClientVerb::Quotes
            | DataClientVerb::Trades
            | DataClientVerb::Book
            | DataClientVerb::Depth => true,
        }
    }
    for verb in [
        DataClientVerb::Bars,
        DataClientVerb::Quotes,
        DataClientVerb::Trades,
        DataClientVerb::Book,
        DataClientVerb::Depth,
    ] {
        assert_eq!(verb.stream_label().is_some(), is_tick_verb(verb), "{verb:?}");
    }
}

/// The auction lifetimes at the core edge: judged on a MARKET order (and a market-to-limit, IB's `AUC`
/// at the open) as on a limit order (a market-on-open the adapter cannot express would execute NOW),
/// admitted where the row's `accepted_tifs` lists one and refused by name on every other roster venue
/// that places the kind. A stop with one is left to the doors' lifetime rule, as every non-limit
/// lifetime always was; a paper/sim venue id is not judged here (its book refuses what it cannot hold).
/// ibkr is the one row that wires both (its w2 auction smoke).
#[test]
fn an_auction_lifetime_is_judged_on_a_market_order_and_admitted_where_the_row_wires_it() {
    let wiring: Vec<&str> = vike_model::VENUES
        .iter()
        .copied()
        .filter(|v| capabilities_for(v).accepted_tifs.contains(&TimeInForce::AtOpen))
        .collect();
    assert_eq!(wiring, ["ibkr"]);
    for &v in vike_model::VENUES {
        let c = capabilities_for(v);
        for t in [TimeInForce::AtOpen, TimeInForce::AtClose] {
            let wired = c.accepted_tifs.contains(&t);
            for kind in ["market", "limit", "market_to_limit"] {
                if !c.supported_order_kinds.contains(&kind) {
                    continue;
                }
                let got = preflight_order(&req(v, kind, t));
                if wired {
                    assert_eq!(got, Ok(()), "{v}/{kind}: the row wires {t:?}");
                    continue;
                }
                let deny = got.expect_err(v);
                assert_eq!(
                    deny,
                    PreflightDeny::IbapiTimeInForce { tif: t, venue: v.to_string() },
                    "{v}/{kind}"
                );
                assert!(deny.to_string().starts_with("TIF_UNSUPPORTED: "), "{deny}");
            }
            if c.supported_order_kinds.contains(&"stop") {
                let stop = OrderRequest { trigger_price: Some(1.0), ..req(v, "stop", t) };
                assert_eq!(
                    preflight_order(&stop),
                    Ok(()),
                    "{v}: a stop's lifetime is not judged here"
                );
            }
        }
    }
    assert_eq!(preflight_order(&req("sim", "market", TimeInForce::AtOpen)), Ok(()));
}

/// The display axis of the ROW: an iceberg passes where the row wires a visible size, and everywhere else
/// the row refuses it by name (`DISPLAY_QTY_UNSUPPORTED`, never rested at its full visible size; the core
/// emulates it first where `emulation_of` says so), a paper tier
/// not relaxing it (the paper book shows nothing to anyone, but the row is the live adapter's truth);
/// an order that shows its whole size is unchanged, and a paper/sim venue id is not judged. Which
/// venues wire one is each bridge's `capabilities_test` to pin.
#[test]
fn a_display_quantity_is_refused_where_the_row_does_not_wire_one() {
    for &v in vike_model::VENUES {
        let iceberg =
            OrderRequest { display_quantity: Some(0.25), ..req(v, "limit", TimeInForce::Gtc) };
        if capabilities_for(v).supports_display_quantity {
            assert_eq!(preflight_order(&iceberg), Ok(()), "{v}");
            continue;
        }
        let deny = PreflightDeny::DisplayQuantity { venue: v.to_string() };
        assert_eq!(deny.to_string(), format!("DISPLAY_QTY_UNSUPPORTED: venue={v}"), "the reason");
        let want = Err(deny);
        assert_eq!(preflight_order(&iceberg), want, "{v}");
        assert_eq!(preflight_order_at(&iceberg, v, Some(vike_model::AccountTier::Paper)), want);
        assert_eq!(preflight_order(&req(v, "limit", TimeInForce::Gtc)), Ok(()), "{v}: no display");
    }
    // `capabilities_for` of an unknown venue is the fallback row (the call dodges a constant assertion)
    assert!(!capabilities_for("no-such-venue").supports_display_quantity);
    let sim =
        OrderRequest { display_quantity: Some(0.25), ..req("sim", "limit", TimeInForce::Gtc) };
    assert_eq!(preflight_order(&sim), Ok(()));
}

/// A `market_to_limit` passes where the row lists it (deribit's `market_limit` and ibkr's `MTL` are
/// their bridges' to build) and is refused by name on every other roster venue, as the plain kind it
/// is (not a trigger).
#[test]
fn a_market_to_limit_is_refused_by_name_until_a_bridge_builds_it() {
    for &v in vike_model::VENUES {
        let mtl = OrderRequest { price: None, ..req(v, "market_to_limit", TimeInForce::Gtc) };
        let want = if capabilities_for(v).supported_order_kinds.contains(&"market_to_limit") {
            Ok(())
        } else {
            Err(PreflightDeny::OrderKind { kind: "market_to_limit".into(), venue: v.to_string() })
        };
        assert_eq!(preflight_order(&mtl), want, "{v}");
    }
}

/// The three trailing kinds (the trailing take-profit of the attributes wave, IB `TRAIL MIT` / `TRAIL LIT`,
/// included) are TRIGGER kinds: a row that does not list one refuses it by name as a trigger
/// (`TRIGGER_UNSUPPORTED`, never coerced into the market order a `_ =>` would make), a reduce-only one and a
/// paper tier's included, and a row that lists one (its bridge built it and its smoke ran) passes it.
/// Which rows list one is `expanded_axes_matrix_is_pinned`'s, one row per venue.
#[test]
fn a_trailing_kind_is_refused_by_name_on_every_row() {
    for kind in ["trailing", "trailing_limit", "trailing_take_profit"] {
        for &v in vike_model::VENUES {
            let lists = capabilities_for(v).supported_order_kinds.contains(&kind);
            let order = OrderRequest {
                price: None,
                trail: Some(1.0),
                limit_offset: (kind == "trailing_limit").then_some(0.5),
                reduce_only: true,
                ..req(v, kind, TimeInForce::Gtc)
            };
            let want = if lists {
                Ok(())
            } else {
                Err(PreflightDeny::Trigger { kind: kind.into(), venue: v.to_string() })
            };
            assert_eq!(preflight_order(&order), want, "{v}/{kind}");
            let paper = preflight_order_at(&order, v, Some(vike_model::AccountTier::Paper));
            assert_eq!(paper, want, "{v}/{kind} on a paper tier");
        }
    }
}

/// Only ibkr's row lists `"pegged"` (`ORDER_KINDS`' doc): by the ROW a pegged order is an
/// `ORDER_KIND_UNSUPPORTED` refusal on every other roster venue and every tier, and passes ibkr's (the
/// core emulates it first where `emulation_of` says so).
#[test]
fn a_pegged_order_is_refused_by_name_on_every_other_row() {
    let peg = vike_model::OrderPeg { anchor: vike_model::OrderPegAnchor::Mid, offset: 0.0 };
    for &v in vike_model::VENUES {
        let ibkr = v == "ibkr";
        assert_eq!(capabilities_for(v).supported_order_kinds.contains(&"pegged"), ibkr, "{v}");
        let order =
            OrderRequest { price: None, peg: Some(peg), ..req(v, "pegged", TimeInForce::Gtc) };
        let refused = Err(PreflightDeny::OrderKind { kind: "pegged".into(), venue: v.to_string() });
        let want = if ibkr { Ok(()) } else { refused };
        assert_eq!(preflight_order(&order), want, "{v}");
        let paper = preflight_order_at(&order, v, Some(vike_model::AccountTier::Paper));
        assert_eq!(paper, want, "{v} on a paper tier");
    }
}

/// The ROUTE TABLE (decision 0275): for every roster venue and tier, a pegged order on each anchor, a
/// visible size and a hidden order resolve to native, emulated or refused exactly as `emulation_of`'s doc
/// says, and an emulated parent is judged at accept as its plain-limit child (`emulated_child`).
#[test]
fn the_route_of_every_peg_and_visible_size_is_native_emulated_or_refused() {
    use vike_model::AccountTier::{Demo, Live, Paper};
    use vike_model::{OrderPeg, OrderPegAnchor};
    let refused = |v: &str| EMULATION_REFUSED_VENUES.iter().any(|(r, _)| *r == v);
    assert_eq!(
        EMULATION_REFUSED_VENUES.iter().map(|(v, _)| *v).collect::<Vec<_>>(),
        ["polymarket", "aster"]
    );
    for &v in vike_model::VENUES {
        for tier in [None, Some(Paper), Some(Demo), Some(Live)] {
            for anchor in OrderPegAnchor::ALL {
                let peg = Some(OrderPeg { anchor, offset: 0.0 });
                let order = OrderRequest { price: None, peg, ..req(v, "pegged", TimeInForce::Gtc) };
                let native = v == "ibkr" && anchor == OrderPegAnchor::Mid && tier != Some(Paper);
                let amends = crate::venue_amend_mode::amend_mode(
                    v,
                    crate::venue_amend_mode::AmendKind::Limit,
                ) == crate::venue_amend_mode::AmendMode::Native;
                let want = (!native && amends && !refused(v)).then_some(OrderEmulationKind::Chase);
                assert_eq!(emulation_of(&order, v, tier), want, "{v}/{anchor:?}/{tier:?}");
                // No emulation for a lifetime that rests nothing.
                let ioc = OrderRequest { time_in_force: TimeInForce::Ioc, ..order.clone() };
                assert_eq!(emulation_of(&ioc, v, tier), None, "{v}: ioc");
            }
            let shown =
                OrderRequest { display_quantity: Some(0.25), ..req(v, "limit", TimeInForce::Gtc) };
            let want = (!capabilities_for(v).supports_display_quantity && !refused(v))
                .then_some(OrderEmulationKind::Iceberg);
            assert_eq!(emulation_of(&shown, v, tier), want, "{v}/display/{tier:?}");
            let hidden = OrderRequest { display_quantity: Some(0.0), ..shown.clone() };
            assert_eq!(
                emulation_of(&hidden, v, tier),
                None,
                "{v}: a hidden order is never emulated"
            );
            let fok = OrderRequest { time_in_force: TimeInForce::Fok, ..shown.clone() };
            assert_eq!(emulation_of(&fok, v, tier), None, "{v}: fok");
            let leg = OrderRequest { order_list_id: Some("L".into()), ..shown.clone() };
            assert_eq!(emulation_of(&leg, v, tier), None, "{v}: a list leg");
            let plain = req(v, "limit", TimeInForce::Gtc);
            assert_eq!(emulation_of(&plain, v, tier), None, "{v}: a plain limit");
        }
        // An emulated parent is judged as its child: a plain limit the row takes.
        let child = emulated_child(&OrderRequest {
            display_quantity: Some(0.25),
            ..req(v, "limit", TimeInForce::Gtc)
        });
        assert_eq!(
            (child.order_type.clone(), child.display_quantity, child.peg),
            (OrderType::Limit, None, None)
        );
        assert!(child.order_client_id.is_empty(), "each child is minted");
        let judged = OrderRequest { order_client_id: "c".into(), ..child };
        assert_eq!(preflight_order(&judged), Ok(()), "{v}: the child is a plain limit");
    }
    // Off the roster (a sim or test id): no book holds a peg, a visible size goes to its own book.
    let peg = Some(OrderPeg { anchor: OrderPegAnchor::Best, offset: 0.0 });
    let sim_peg = OrderRequest { price: None, peg, ..req("sim", "pegged", TimeInForce::Gtc) };
    assert_eq!(emulation_of(&sim_peg, "sim", None), Some(OrderEmulationKind::Chase));
    let sim_shown =
        OrderRequest { display_quantity: Some(0.25), ..req("sim", "limit", TimeInForce::Gtc) };
    assert_eq!(emulation_of(&sim_shown, "sim", None), None);
    assert_eq!(OrderEmulationKind::Chase.as_word(), "chase");
    assert_eq!(OrderEmulationKind::Iceberg.as_word(), "iceberg");
}

/// A venue that trails by a PERCENT (binance's perpetual `callbackRate`, aster's) or in basis points
/// (binance spot's `trailingDelta`) never lists a trailing kind, on any tier: a vike trail is a distance in
/// price, and the two are different orders once the market moves (the `BINANCE` and `ASTER` rows' docs).
/// Its percent trail is the other axis, `percent_trail_kinds`
/// (`a_percent_trail_is_judged_on_its_own_axis_apart_from_the_price_trail`), which never opens this one.
#[test]
fn a_percent_trail_venue_never_lists_a_trailing_kind() {
    for venue in ["binance", "aster"] {
        for kind in ["trailing", "trailing_limit"] {
            assert!(
                !capabilities_for(venue).supported_order_kinds.contains(&kind),
                "{venue}/{kind}"
            );
            let order = OrderRequest {
                price: None,
                trail: Some(1.0),
                ..req(venue, kind, TimeInForce::Gtc)
            };
            let want = Err(PreflightDeny::Trigger { kind: kind.into(), venue: venue.to_string() });
            assert_eq!(preflight_order(&order), want, "{venue}/{kind}");
            let paper = preflight_order_at(&order, venue, Some(vike_model::AccountTier::Paper));
            assert_eq!(paper, want, "{venue}/{kind} on a paper tier");
        }
    }
}

// ── order attributes and percent trails (the attributes wave) ─────────────────────────────

/// The rows that list an ORDER ATTRIBUTE word (`VenueCapabilities::order_attributes`), verbatim: every other
/// roster venue lists none (a new venue's scaffold row is empty, so it passes on arrival). A row opens
/// behind its bridge's smoke, one commit per venue (the playbook), each under its own comment.
#[rustfmt::skip]
const LISTED_ATTRIBUTES: &[(&str, &[&str])] = &[
    // ibkr (its attributes smoke; `release`, IB's native good-after-time, its own commit)
    ("ibkr", &["all_or_none", "sweep_to_fill", "discretionary", "hidden", "adjust", "release"]),
];

/// The rows that list a PERCENT trail (`VenueCapabilities::percent_trail_kinds`), verbatim, as
/// [`LISTED_ATTRIBUTES`].
#[rustfmt::skip]
const LISTED_PERCENT_TRAILS: &[(&str, &[&str])] = &[
    // binance (its spot trailing and perp algo smokes)
    ("binance", &["trailing"]),

    // okx (its trailing smoke)
    ("okx", &["trailing"]),

    // ibkr (its attributes smoke)
    ("ibkr", &["trailing", "trailing_take_profit"]),
];

/// The two axes of the attributes wave, pinned per roster venue against the two tables above; every
/// word a row lists is vocabulary (an attribute word of [`ORDER_ATTRIBUTES`], a TRAILING kind).
#[test]
fn the_attribute_and_percent_trail_axes_are_pinned() {
    fn listed(table: &[(&str, &'static [&'static str])], v: &str) -> &'static [&'static str] {
        table.iter().find(|(tv, _)| *tv == v).map_or(&[], |(_, words)| *words)
    }
    for &v in vike_model::VENUES {
        let c = capabilities_for(v);
        assert_eq!(c.order_attributes, listed(LISTED_ATTRIBUTES, v), "{v} order_attributes");
        assert_eq!(
            c.percent_trail_kinds,
            listed(LISTED_PERCENT_TRAILS, v),
            "{v} percent_trail_kinds"
        );
        for w in c.order_attributes {
            assert!(ORDER_ATTRIBUTES.contains(w), "{v}: `{w}` is no attribute word");
        }
        for k in c.percent_trail_kinds {
            assert!(OrderType::from_stored(k).is_trailing(), "{v}: `{k}` is no trailing kind");
        }
    }
    for (v, _) in LISTED_ATTRIBUTES.iter().chain(LISTED_PERCENT_TRAILS) {
        assert!(vike_model::VENUES.contains(v), "{v} is no roster venue");
    }
}

/// The request that carries exactly the attribute `word` on `venue`: a GTC limit, which every row places
/// (an adjustment rides a stop). A word with no request here panics, so a new attribute word cannot
/// reach a row without being judged by the tests below.
fn attributed(venue: &str, word: &str) -> OrderRequest {
    use vike_model::{OrderAttributes, OrderRelease, OrderStopAdjust, OrderStopAdjustTo};
    let limit = req(venue, "limit", TimeInForce::Gtc);
    let with = |attributes: OrderAttributes| OrderRequest {
        attributes: Some(attributes),
        ..req(venue, "limit", TimeInForce::Gtc)
    };
    match word {
        "all_or_none" => with(OrderAttributes { all_or_none: true, ..Default::default() }),
        "sweep_to_fill" => with(OrderAttributes { sweep_to_fill: true, ..Default::default() }),
        "discretionary" => {
            with(OrderAttributes { discretionary_amount: Some(0.5), ..Default::default() })
        }
        "hidden" => OrderRequest { display_quantity: Some(0.0), ..limit },
        "adjust" => OrderRequest {
            price: None,
            trigger_price: Some(1.0),
            adjust: Some(OrderStopAdjust { at: 2.0, to: OrderStopAdjustTo::Stop { stop: 1.5 } }),
            ..req(venue, "stop", TimeInForce::Gtc)
        },
        "release" => OrderRequest { release: Some(OrderRelease { at: 1, price: None }), ..limit },
        other => panic!("no request carries the attribute word `{other}`"),
    }
}

/// THE ROW TEST of the attribute axis: every attribute word is refused BY NAME on every roster row that
/// does not list it (`ATTRIBUTE_UNSUPPORTED: venue=<v> attribute=<word>`, never sent as the plain order
/// with the attribute dropped), a paper tier included (the axis is the adapter's wire, not a reduce-only
/// refusal), and passes where the row lists it. A kind the row does not place is refused first, by its
/// kind (polymarket places no stop, so its adjustment never reaches this axis). `release` refuses
/// nothing anywhere: where a row lacks it the core holds the release. A paper/sim id is not judged.
#[test]
fn every_attribute_word_is_refused_by_name_where_the_row_does_not_list_it() {
    for &v in vike_model::VENUES {
        let c = capabilities_for(v);
        for &word in ORDER_ATTRIBUTES {
            let order = attributed(v, word);
            let kind = order.order_type.as_str();
            let want = if !c.supported_order_kinds.contains(&kind) {
                Err(PreflightDeny::Trigger { kind: kind.into(), venue: v.to_string() })
            } else if word == "release" || c.order_attributes.contains(&word) {
                Ok(())
            } else {
                let deny = PreflightDeny::Attribute { attribute: word, venue: v.to_string() };
                assert_eq!(
                    deny.to_string(),
                    format!("ATTRIBUTE_UNSUPPORTED: venue={v} attribute={word}"),
                    "the reason"
                );
                Err(deny)
            };
            assert_eq!(preflight_order(&order), want, "{v}/{word}");
            let paper = preflight_order_at(&order, v, Some(vike_model::AccountTier::Paper));
            assert_eq!(paper, want, "{v}/{word} on a paper tier");
        }
    }
    for &word in ORDER_ATTRIBUTES {
        assert_eq!(preflight_order(&attributed("sim", word)), Ok(()), "sim/{word}");
    }
}

/// A display of ZERO is the `hidden` attribute's, never the visible-slice axis': a row that shows a
/// slice (binance spot's `icebergQty`, deribit's `display_amount`, each with a visible minimum) still
/// refuses a hidden order unless it lists `hidden`, and a row that shows none refuses it as hidden, not
/// as `DISPLAY_QTY_UNSUPPORTED`.
#[test]
fn a_display_of_zero_is_judged_by_hidden_and_not_by_the_display_axis() {
    for &v in vike_model::VENUES {
        let c = capabilities_for(v);
        let hidden =
            OrderRequest { display_quantity: Some(0.0), ..req(v, "limit", TimeInForce::Gtc) };
        let want = if c.order_attributes.contains(&"hidden") {
            Ok(())
        } else {
            Err(PreflightDeny::Attribute { attribute: "hidden", venue: v.to_string() })
        };
        assert_eq!(preflight_order(&hidden), want, "{v}");
        if c.supports_display_quantity {
            let slice = OrderRequest { display_quantity: Some(0.25), ..hidden };
            assert_eq!(preflight_order(&slice), Ok(()), "{v}: a slice is the display axis'");
        }
    }
    assert!(
        capabilities_for("binance").supports_display_quantity
            && capabilities_for("deribit").supports_display_quantity
    );
}

/// A PERCENT trail is judged on its own axis (`percent_trail_kinds`), never on the kind axis: a row's
/// price trail and its percent trail open apart, in both directions, and some roster row always lists
/// one without the other (okx's and deribit's price trails today; once binance's percent trail opens,
/// binance, whose price trail never does). The reduce-only axis is not consulted for a percent order (a
/// row lists a kind there only where its reduce-only exit works too), and a paper tier changes nothing.
#[test]
fn a_percent_trail_is_judged_on_its_own_axis_apart_from_the_price_trail() {
    let mut apart = 0;
    for &v in vike_model::VENUES {
        let c = capabilities_for(v);
        for kind in ["trailing", "trailing_limit", "trailing_take_profit"] {
            let refused = Err(PreflightDeny::Trigger { kind: kind.into(), venue: v.to_string() });
            let percent_listed = c.percent_trail_kinds.contains(&kind);
            let price_listed = c.supported_order_kinds.contains(&kind);
            apart += usize::from(percent_listed != price_listed);
            for reduce_only in [false, true] {
                let price = OrderRequest {
                    price: None,
                    trail: Some(1.0),
                    limit_offset: (kind == "trailing_limit").then_some(0.5),
                    reduce_only,
                    ..req(v, kind, TimeInForce::Gtc)
                };
                let percent =
                    OrderRequest { trail: None, trail_percent: Some(1.0), ..price.clone() };
                let want = if percent_listed { Ok(()) } else { refused.clone() };
                let at = format!("{v}/{kind} by a percent, reduce-only {reduce_only}");
                assert_eq!(preflight_order(&percent), want, "{at}");
                let paper = preflight_order_at(&percent, v, Some(vike_model::AccountTier::Paper));
                assert_eq!(paper, want, "{at} on a paper tier");
                if !reduce_only || c.reduce_only_kinds.contains(&kind) {
                    let want = if price_listed { Ok(()) } else { refused.clone() };
                    assert_eq!(preflight_order(&price), want, "{v}/{kind} by a price");
                }
            }
        }
    }
    assert!(
        apart > 0,
        "no roster row lists a price trail without its percent trail or the reverse"
    );
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

/// Wave 6: every roster venue refuses an option exercise, naming itself; deribit, ibkr and alpaca
/// with their own reasons, the rest because no option trades there. An id off the roster is `None`.
#[test]
fn every_roster_venue_refuses_an_option_exercise_in_its_own_words() {
    for &venue in vike_model::VENUES {
        let why = exercise_refusal(venue).unwrap_or_else(|| panic!("{venue}: no answer"));
        assert!(why.contains(venue), "{venue} names itself: {why}");
    }
    for (venue, words) in [
        ("deribit", "European"),
        ("ibkr", "decision 0234"),
        ("alpaca", "deferred"),
        ("binance", "trades no option"),
    ] {
        let why = exercise_refusal(venue).unwrap_or_default();
        assert!(why.contains(words), "{venue}: {why}");
    }
    assert_eq!(exercise_refusal("sim"), None, "an id off the roster");
}

/// The bridges' kind floor is the row's `supported_order_kinds`, kind by kind on every roster venue,
/// and its words name the venue and the kind; an unrecognised kind and a venue off the roster refuse.
#[test]
fn the_unbuilt_kind_floor_is_the_rows_supported_kinds() {
    let of = |kind: OrderType| OrderRequest { order_type: kind, ..Default::default() };
    for &venue in vike_model::VENUES {
        for kind in OrderType::KNOWN {
            let word = kind.as_str();
            let built = capabilities_for(venue).supported_order_kinds.contains(&word);
            match unbuilt_kind_refusal(venue, &of(kind.clone())) {
                None => assert!(built, "{venue} {word}: the row does not list it"),
                Some(why) => {
                    assert!(!built, "{venue} {word}: the row lists it");
                    assert!(why.contains(venue) && why.contains(word), "{venue} {word}: {why}");
                }
            }
        }
        assert!(
            unbuilt_kind_refusal(venue, &of(OrderType::Unrecognized("iceberg".into()))).is_some()
        );
    }
    assert_eq!(
        unbuilt_kind_refusal("bybit", &of(OrderType::Trailing)).as_deref(),
        Some("bybit builds no `trailing` order; refused, nothing was sent")
    );
    assert!(unbuilt_kind_refusal("sim", &of(OrderType::Market)).is_some(), "an id off the roster");
}
