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

/// Completeness vs the canonical roster: every [`crate::venues::VENUES`] entry has a NAMED
/// declared row, and the registry serves exactly it. Adding a venue to the roster fails here
/// until its row exists — the whole point of the roster. (A row's VALUE may equal the
/// `UNSUPPORTED` fallback — cTrader's does, deliberately — but it must be NAMED: the named
/// const is the proof the venue was classified, not forgotten.)
#[test]
fn every_roster_venue_has_a_declared_row() {
    // `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is
    // re-indented by rustfmt once a row ending in a trailing `//` comment is generated above
    // it, which defeats `--remove`. Gated by `crates/vike-ops/tests/new_venue_gate.rs`'s
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
    assert_eq!(rows.len(), crate::venues::VENUES.len(), "one declared row per roster venue");
    for &v in crate::venues::VENUES {
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
    // cTrader's `AMEND_ORDER_REQ` amend, audited from the adapter).
    for v in ["binance", "bybit", "okx", "aster", "hyperliquid", "ctrader"] {
        assert!(caps_for(v).allows_modify(), "{v} should allow modify");
    }
    // blocked: no native modify wired
    for v in ["deribit", "oanda", "ig", "fxcm", "dukascopy", "polymarket", "ibkr", "unknown"] {
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
    assert!(!c.backfill_bars && !c.backfill_ticks, "no vike-backfill cTrader collector");
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
/// unknown fallback, and the derived IBKR_CPAPI row) must stay `false` — a `true` row without
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
    // the derived cpapi backend row inherits IBKR's `false` (compared through the non-const
    // registry so the assertion isn't const-folded, per this module's test convention)
    assert_eq!(
        IBKR_CPAPI.supports_combo,
        caps_for("ibkr").supports_combo,
        "the cpapi backend row must inherit IBKR's combo stance"
    );
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
/// full five, and deribit's book-without-depth.
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
    // ibkr: ALL FIVE verbs, all real — `IbkrFeeds` (market_feed/mod.rs, #306) implements every
    // one over the ibapi socket session; `depth_pump` feeds the book AND the depth lane from
    // the same `reqMktDepth` ladder. This row said `LiveDataCaps::NONE` from 2026-07-14 until
    // the pump-spec cross-pin caught it.
    assert_eq!(
        IBKR.live_data,
        LiveDataCaps { bars: true, quotes: true, trades: true, book: true, depth: true }
    );
    assert_eq!(IBKR_CPAPI.live_data, IBKR.live_data, "the cpapi row inherits the feed");
    // oanda: the SPLIT-PLANE feed — quotes stream (the chunked-HTTP pricing stream), bars
    // poll the candles REST. No trade tape is published, and the pricing ladder is an
    // unsequenced top-of-book snapshot rather than a delta-synced book → book/depth false.
    assert_eq!(
        OANDA.live_data,
        LiveDataCaps { bars: true, quotes: true, trades: false, book: false, depth: false }
    );
    // deribit (split-plane I9): bars + quotes + trades + the LOSSLESS book lane; the
    // conflating DOM `depth` lane is the one verb not wired — `subscribe_depth` refuses
    // through this row (`vike_deribit::market_feed::Feeds`).
    assert_eq!(
        DERIBIT.live_data,
        LiveDataCaps { bars: true, quotes: true, trades: true, book: true, depth: false }
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
    assert_eq!(oanda.supported_tifs.len(), 5);
    assert!(oanda.supported_tifs.contains(&TimeInForce::Day));
    assert!(oanda.supported_tifs.contains(&TimeInForce::Gtd));
    // binance: the row stays the CONSERVATIVE spot∩perp intersection — the perp lane
    // additionally wires native GTD, but this registry cannot distinguish the lanes (one
    // "binance" key), so Gtd is deliberately NOT declared here; the per-lane truth is
    // venue_tif's "binance-perp" row (see the BINANCE row doc).
    for v in ["binance", "bybit", "okx"] {
        assert_eq!(
            caps_for(v).supported_tifs,
            &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok],
            "{v}: GTC/IOC/FOK declared (binance = lane intersection, perp-GTD lives in \
                 venue_tif)"
        );
    }
    assert_eq!(
        caps_for("deribit").supported_tifs,
        &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::Day],
        "deribit flipped: GTC default + IOC/FOK/Day mapped, GTD denied"
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
/// which compares these two fields to `backfill_caps(Source::Venue, v)` — a table with a
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
            ("oanda",       false, false),
            ("ig",          false, false),
            ("fxcm",        false, false),
            ("polymarket",  false, false),
            ("ctrader",     false, false),
            ("alpaca",      false, false),
            // vike:new-venue:row ("{venue}",  false, false), // TODO(new-venue: {venue}): flip when a collector lands
        ];
    assert_eq!(rows.len(), crate::venues::VENUES.len(), "one pinned row per roster venue");
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
    use TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
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
            ("binance",     MLS, &[StopLoss], &[Gtc, Ioc, Fok, Gtd],       &[Cross], 5),
            ("bybit",       MLS, &[StopLoss], &[Gtc, Ioc, Fok],            &[Cross], 0),
            ("okx",         MLS, &[StopLoss], &[Gtc, Ioc, Fok],            &[Cross, Isolated], 0),
            ("aster",       MLS, &[StopLoss], &[Gtc],                      &[Cross], 5),
            ("deribit",     ML,  &[],         &[Gtc, Ioc, Fok, Day],       &[Cross], 0),
            ("oanda",       MLS, &[StopLoss], ALL5,                        &[Cross], 0),
            ("alpaca",      &["market", "limit", "stop", "stop_limit"], &[StopLoss],
                &[Day, Gtc, Ioc, Fok, Gtd],                               &[Cross], 0),
            ("ig",          MLS, &[StopLoss], &[Gtc],                      &[Cross], 0),
            ("fxcm",        ML,  &[],         &[Gtc],                      &[Cross], 0),
            ("dukascopy",   ML,  &[],         &[Gtc],                      &[Cross], 0),
            ("polymarket",  ML,  &[],         ALL5,                        &[Cash],  0),
            ("ibkr",        MLS, &[StopLoss], &[Gtc, Day, Ioc, Fok, Gtd],  &[Cross], 0),
            ("ctrader",     MLS, &[StopLoss], &[Gtc],                      &[Cross], 0),
            ("hyperliquid", &["market", "limit", "stop", "stop_limit", "take_profit"],
                &[StopLoss, TakeProfit], ALL5,                            &[Cross], usize::MAX),
            // vike:new-venue:row ("{venue}",     ML,  &[],         &[Gtc],                      &[Cross], 0), // TODO(new-venue: {venue}): pin what the adapter really wires
        ];
    assert_eq!(rows.len(), crate::venues::VENUES.len(), "one pinned row per roster venue");
    for (venue, kinds, triggers, accepted, margins, max_batch) in rows {
        let c = caps_for(venue);
        assert_eq!(c.supported_order_kinds, *kinds, "{venue} supported_order_kinds");
        assert_eq!(c.trigger_types, *triggers, "{venue} trigger_types");
        assert_eq!(c.accepted_tifs, *accepted, "{venue} accepted_tifs");
        assert_eq!(c.margin_modes, *margins, "{venue} margin_modes");
        assert_eq!(c.max_batch, *max_batch, "{venue} max_batch");
        assert!(!c.supports_post_only, "{venue}: post-only is inexpressible via OrderRequest");
    }
}

/// Structural invariants across every roster venue: the admit set contains the honored set;
/// trigger declarations agree between the two axes; the batch cap agrees with the batch
/// flag; every declared kind is canonical vocabulary; a roster venue always wires SOME kind.
#[test]
fn expanded_axes_invariants_hold() {
    for &v in crate::venues::VENUES {
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
    for &v in crate::venues::VENUES {
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
/// bridge, by `crates/bridges/hyperliquid/src/symbology_tests.rs`'s
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

fn req(venue: &str, order_type: &str, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        client_order_id: "c-pf".to_string(),
        venue: venue.to_string(),
        symbol: "X".to_string(),
        side: 1,
        qty: 1.0,
        order_type: order_type.to_string(),
        price: Some(1.0),
        time_in_force: tif,
        ..Default::default()
    }
}

/// THE COMPAT LAW: nothing a venue accepts-and-honors today is refused. Every row's own
/// declared kinds/TIFs pass; the coercion venues' coerced TIFs pass; binance's perp-lane GTD
/// passes (lane union); non-limit paths never TIF-refuse; unknown venues, unknown kind
/// strings, combos and `margin_mode: None` all pass.
#[test]
fn preflight_passes_everything_accepted_today() {
    use TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
    for &v in crate::venues::VENUES {
        let c = caps_for(v);
        for k in c.supported_order_kinds {
            assert_eq!(preflight_order(&req(v, k, Gtc)), Ok(()), "{v}/{k}");
        }
        for t in c.accepted_tifs {
            assert_eq!(preflight_order(&req(v, "limit", *t)), Ok(()), "{v}/limit/{t:?}");
        }
        // market path carries no TIF axis anywhere — never TIF-refused
        for t in [Gtc, Ioc, Fok, Gtd, Day] {
            if c.supported_order_kinds.contains(&"market") {
                assert_eq!(preflight_order(&req(v, "market", t)), Ok(()), "{v}/market/{t:?}");
            }
        }
    }
    // the coercion venues' coerced TIFs (live behavior — a separate flip)
    assert_eq!(preflight_order(&req("polymarket", "limit", TimeInForce::Ioc)), Ok(()));
    assert_eq!(preflight_order(&req("hyperliquid", "limit", TimeInForce::Fok)), Ok(()));
    assert_eq!(preflight_order(&req("alpaca", "limit", TimeInForce::Gtd)), Ok(()));
    // binance lane union: perp GTD must NOT be refused at the core edge
    assert_eq!(preflight_order(&req("binance", "limit", TimeInForce::Gtd)), Ok(()));
    // unknown venue (paper/sim ids) — no declared row, never refused here
    assert_eq!(preflight_order(&req("sim", "limit", TimeInForce::Ioc)), Ok(()));
    // unknown kind string — no row can claim it, venue side owns it
    assert_eq!(preflight_order(&req("binance", "market_close", TimeInForce::Gtc)), Ok(()));
    // combo requests: the Combo lowering owns capability
    let mut combo = req("oanda", "limit", TimeInForce::Gtc);
    combo.combo_legs = vec![crate::orders::order::ComboLeg { symbol: "A".into(), ratio: 1 }];
    assert_eq!(preflight_order(&combo), Ok(()));
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
    // trigger kinds: deribit's `_ => market` coercion would fire a stop IMMEDIATELY
    let deny = preflight_order(&req("deribit", "stop", Gtc)).unwrap_err();
    assert_eq!(deny.to_string(), "TRIGGER_UNSUPPORTED: kind=stop venue=deribit");
    let deny = preflight_order(&req("binance", "take_profit", Gtc)).unwrap_err();
    assert_eq!(deny.to_string(), "TRIGGER_UNSUPPORTED: kind=take_profit venue=binance");
    // non-trigger kind: fxcm's shim now HAS a true-market placement, so `"market"` is honest
    // there and passes — fxcm's row was the LAST declared venue denying a non-trigger kind.
    assert_eq!(preflight_order(&req("fxcm", "market", Gtc)), Ok(()));
    // With that flip, `PreflightDeny::OrderKind` is unreachable through `preflight_order`:
    // every declared venue serves both non-trigger kinds, and an UNDECLARED venue is skipped
    // outright (no declared row can classify it). Both halves of that reasoning are asserted
    // below so a future row that drops "market"/"limit" re-arms the class instead of silently
    // shipping a lie; the variant's machine-readable rendering stays pinned directly.
    for &v in crate::venues::VENUES {
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
    // case-insensitivity: the kind is lowercased before classification
    assert!(preflight_order(&req("deribit", "STOP", Gtc)).is_err());
}

/// The allocation-free `eq_ignore_ascii_case` rewrite must match EXACTLY what the old
/// `to_ascii_lowercase()` + `==`/`.contains` comparisons matched — a mixed-case spelling of a
/// supported kind still passes, not just the all-caps case above.
#[test]
fn preflight_matches_mixed_case_spellings() {
    use TimeInForce::Gtc;
    // "Stop" (mixed case) is binance's native trigger kind — must still be ACCEPTED.
    assert_eq!(preflight_order(&req("binance", "Stop", Gtc)), Ok(()));
    // "MaRkEt" is a supported kind everywhere it's declared — must still be ACCEPTED.
    assert_eq!(preflight_order(&req("aster", "MaRkEt", Gtc)), Ok(()));
    // "Limit" (mixed case) still hits the TIF branch and is refused for aster's unaccepted IOC.
    let deny = preflight_order(&req("aster", "Limit", TimeInForce::Ioc)).unwrap_err();
    assert_eq!(deny.to_string(), "TIF_UNSUPPORTED: tif=Ioc venue=aster");
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
