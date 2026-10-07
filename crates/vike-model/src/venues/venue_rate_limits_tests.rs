use super::*;

/// Every roster venue paired with the row(s) it declares. The completeness test below asserts
    /// this covers [`crate::venues::VENUES`] exactly.
    ///
    /// `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is
    /// re-indented by rustfmt once a row ending in a trailing `//` comment is generated above it,
    /// which defeats `--remove`. Gated by `crates/vike-ops/tests/venues/new_venue_gate/rustfmt_rule.rs`'s
    /// `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
    #[rustfmt::skip]
    const MATRIX: &[(&str, Market, RateLimits)] = &[
        ("binance", Market::Spot, BINANCE_SPOT),
        ("binance", Market::Perp, BINANCE_PERP),
        ("aster", Market::Spot, ASTER_SPOT),
        ("aster", Market::Perp, ASTER_PERP),
        ("bybit", Market::Spot, BYBIT),
        ("okx", Market::Spot, OKX),
        ("deribit", Market::Spot, DERIBIT),
        ("polymarket", Market::Spot, POLYMARKET),
        ("hyperliquid", Market::Spot, HYPERLIQUID),
        ("oanda", Market::Spot, OANDA),
        ("ig", Market::Spot, IG),
        ("fxcm", Market::Spot, FXCM),
        ("dukascopy", Market::Spot, DUKASCOPY),
        ("ibkr", Market::Spot, IBKR),
        ("ctrader", Market::Spot, CTRADER),
        ("alpaca", Market::Spot, ALPACA),
        // vike:new-venue:row ("{venue}", Market::Spot, {VENUE}), // TODO(new-venue: {venue}): a market-SPLIT venue declares two rows
    ];

/// The ORDER meter, pinned VERBATIM as `(venue, market, published, admitted, window_secs)`.
///
/// These are the numbers `crates/bridges/*/src/ratelimit.rs` built its `RateGate`s from before
/// this table existed — the `admitted` column IS the old `SPOT_ORDERS`/`PERP_ORDERS`/`ORDER_OPS`
/// const, and the `window_secs` column IS the old `ORDERS_WINDOW`/`ORDER_WINDOW`. Any drift is
/// loud here, which is what makes "this is a MOVE, not a retune" checkable rather than asserted.
#[test]
fn order_meters_are_pinned() {
    #[rustfmt::skip]
        let rows: &[(&str, Market, usize, usize, u64)] = &[
            // binance: ORDERS metered per 10s, spot and perp budgets differ 3x
            ("binance", Market::Spot,  100,   90, 10),
            ("binance", Market::Perp,  300,  270, 10),
            // aster: the same counter metered per MINUTE, and a 12x spot/perp split
            ("aster",   Market::Spot,  100,   90, 60),
            ("aster",   Market::Perp, 1200, 1080, 60),
            // bybit/okx/deribit: one pool, one row
            ("bybit",   Market::Spot,   20,   18,  1),
            ("okx",     Market::Spot,   60,   50,  2),
            ("deribit", Market::Spot,    5,    5,  1),   // matching-engine, NO margin
        ];
    for (venue, market, published, admitted, window_secs) in rows {
        let m = rate_limits_for(venue, *market).orders;
        assert_eq!(m.published(), *published, "{venue} {market:?} published");
        assert_eq!(m.admitted(), *admitted, "{venue} {market:?} admitted");
        assert_eq!(m.window(), Duration::from_secs(*window_secs), "{venue} {market:?} window");
    }
}

/// The WS-send meter, pinned verbatim: `(venue, published_or_none, admitted, window_secs)`.
/// `None` in the published column is [`Meter::Unpublished`] — the venue documents no WS rate and
/// the number is vike's own runaway brake.
#[test]
fn ws_send_meters_are_pinned() {
    #[rustfmt::skip]
        let rows: &[(&str, Option<usize>, usize, u64)] = &[
            ("binance",    Some(5),    4,    1),
            ("aster",      Some(10),   8,    1),
            ("deribit",    Some(20),  18,    1),   // the non-matching-engine credit pool
            ("okx",        Some(480), 440, 3600),  // 480 subscribe/unsubscribe/login per hour
            ("bybit",      None,      30,   10),   // no documented WS rate: a runaway catcher
            ("polymarket", None,      30,   10),   // ditto
        ];
    for (venue, published, admitted, window_secs) in rows {
        let m = rate_limits_for(venue, Market::Spot).ws_sends;
        assert_eq!(m.published_cap(), *published, "{venue} ws published");
        assert_eq!(m.admitted(), *admitted, "{venue} ws admitted");
        assert_eq!(m.window(), Duration::from_secs(*window_secs), "{venue} ws window");
    }
    // The perp rows carry the SAME WS numbers — the WS handshake is per-connection, not
    // per-market, on both split venues.
    for venue in ["binance", "aster"] {
        let (spot, perp) =
            (rate_limits_for(venue, Market::Spot), rate_limits_for(venue, Market::Perp));
        assert_eq!(spot.ws_sends, perp.ws_sends, "{venue} ws is per-connection, not per-market");
    }
}

/// The shared per-IP REST WEIGHT pool, pinned verbatim — and pinned as EXCLUSIVE, because the
/// axis's whole risk is a future row duplicating a number that already lives elsewhere.
///
/// Hyperliquid's literals ARE `vike_hyperliquid::transport`'s former `IP_WEIGHT_PER_MIN` /
/// `RATE_WINDOW` consts, so a drift here changes live HL pacing and fails loudly. Every other
/// roster venue is [`Meter::Ungated`] — which says vike gates no such pool there, NOT that the
/// venue meters none: binance/aster's `REQUEST_WEIGHT` is the same class of budget and is
/// declared once, under `history`. A row that "fills in" 6000 here would give that fact two
/// homes; this test is what refuses it.
#[test]
fn rest_ip_weight_meters_are_pinned() {
    for market in [Market::Spot, Market::Perp] {
        let m = rate_limits_for("hyperliquid", market).rest_ip_weight;
        assert_eq!(m.published(), 1200, "HL publishes 1200 weight/min per IP");
        assert_eq!(m.admitted(), 1200, "the gate sits AT the cap — no margin today");
        assert_eq!(m.window(), Duration::from_secs(60), "a rolling MINUTE");
    }
    for &v in crate::venues::VENUES {
        if v == "hyperliquid" {
            continue;
        }
        for market in [Market::Spot, Market::Perp] {
            let m = rate_limits_for(v, market).rest_ip_weight;
            assert_eq!(m, Meter::Ungated, "{v} {market:?}: no vike-gated shared REST pool");
            assert_eq!(m.published_cap(), None, "{v} {market:?}");
        }
    }
    // The binance/aster weight ceilings stay where they were — one fact, one home.
    assert_eq!(BINANCE_SPOT.history.ceiling_per_min(), 6000);
    assert_eq!(ASTER_PERP.history.ceiling_per_min(), 2400);
}

/// The paged-history budget, pinned verbatim. `Weighted` rows carry the MEASURED 2026-08-04
/// ceilings and the `page_delay`/`weight_soft_limit` that `vike_binance::data`'s and
/// `vike_aster::data`'s `KlineSpec`s used to hold as local consts.
#[test]
fn history_budgets_are_pinned() {
    #[rustfmt::skip]
        let weighted: &[(&str, Market, u64, u64, u64)] = &[
            // (venue, market, ceiling_per_min, soft_limit, page_delay_ms)
            ("binance", Market::Spot, 6000, 4800,  50),
            ("binance", Market::Perp, 2400, 2000, 150),
            ("aster",   Market::Spot, 6000, 4800, 125),  // sapi's own, measured 2026-08-05
            ("aster",   Market::Perp, 2400, 2000, 150),
        ];
    for (venue, market, ceiling, soft, delay_ms) in weighted {
        let h = rate_limits_for(venue, *market).history;
        assert_eq!(h.ceiling_per_min(), *ceiling, "{venue} {market:?} ceiling");
        assert_eq!(h.soft_limit(), *soft, "{venue} {market:?} soft limit");
        assert_eq!(h.page_delay(), Duration::from_millis(*delay_ms), "{venue} {market:?} delay");
    }
    // The three venues that publish NO weight budget: a fixed 200ms page delay is the whole
    // pacing story, and there is deliberately no ceiling to read.
    for venue in ["bybit", "okx", "deribit"] {
        let h = rate_limits_for(venue, Market::Spot).history;
        assert_eq!(h, History::Unweighted { page_delay: Duration::from_millis(200) }, "{venue}");
        assert_eq!(h.published_ceiling_per_min(), None, "{venue} publishes no weight ceiling");
    }
    // ...and the venues vike pages nothing from.
    for venue in [
        "polymarket",
        "hyperliquid",
        "oanda",
        "ig",
        "fxcm",
        "dukascopy",
        "ibkr",
        "ctrader",
        "alpaca",
    ] {
        assert_eq!(rate_limits_for(venue, Market::Spot).history, History::NotPaged, "{venue}");
    }
}

/// Completeness: every registry venue resolves to its declared const, so the pinned tables above
/// are authoritative.
#[test]
fn registry_maps_every_known_venue() {
    for (venue, market, want) in MATRIX {
        assert_eq!(rate_limits_for(venue, *market), *want, "{venue} {market:?}");
    }
}

/// Completeness vs the canonical roster: every [`crate::venues::VENUES`] entry has a NAMED
/// declared row, and the registry serves exactly it for BOTH markets. Adding a venue to the
/// roster fails here until its rows exist — the whole point of the playbook.
///
/// A row equal to [`RateLimits::NOT_DECLARED`] still counts as declared: the named row IS the
/// declaration that the venue was classified (vike gates nothing here) rather than forgotten,
/// exactly as `venue_margin_support`' FX rows name `VenueMarginSupport::UNKNOWN`. That the seven such rows are
/// NAMED and not the registry's `_` fall-through is the sibling test below.
#[test]
fn every_roster_venue_has_a_declared_row() {
    for &v in crate::venues::VENUES {
        let rows: Vec<(&str, Market, RateLimits)> =
            MATRIX.iter().copied().filter(|(rv, _, _)| *rv == v).collect();
        assert!(!rows.is_empty(), "no RateLimits row declared for roster venue {v}");
        for (_, market, want) in rows.iter().copied() {
            assert_eq!(rate_limits_for(v, market), want, "{v}: registry must serve its row");
        }
        // A market-UNIFORM venue's single row answers both markets — a venue is never
        // half-declared, whatever `MATRIX` happens to list.
        if rows.len() == 1 {
            let want = rows[0].2;
            for market in [Market::Spot, Market::Perp] {
                assert_eq!(rate_limits_for(v, market), want, "{v} {market:?}");
            }
        } else {
            assert_eq!(rows.len(), 2, "{v}: a split venue declares exactly spot + perp");
        }
    }
    assert_eq!(MATRIX.len(), crate::venues::VENUES.len() + 2, "14 venues, 2 of them split");
}

/// The registry's `_` fallback arm is unreachable for a roster venue: every `NOT_DECLARED` row a
/// roster venue gets is a NAMED const. Without this, deleting a venue's arm would look identical
/// to declaring it empty.
///
/// ⚠ [`HYPERLIQUID`] used to be in this list — it was the row that declared its own omission.
/// It now carries a real [`RateLimits::rest_ip_weight`] budget, so it is a DECLARED venue and
/// the `declared ^ is_named` sweep below is what would catch it drifting back.
#[test]
fn undeclared_roster_venues_are_named_rows_not_fall_through() {
    #[rustfmt::skip]
        let named: &[(&str, RateLimits)] = &[
            ("oanda", OANDA), ("ig", IG), ("fxcm", FXCM),
            ("dukascopy", DUKASCOPY), ("ibkr", IBKR), ("ctrader", CTRADER), ("alpaca", ALPACA),
            // vike:new-venue:row ("{venue}", {VENUE}), // TODO(new-venue: {venue}): DELETE this row once the venue declares real numbers
        ];
    for (venue, row) in named {
        assert_eq!(*row, RateLimits::NOT_DECLARED, "{venue} declares nothing today");
        for market in [Market::Spot, Market::Perp] {
            assert_eq!(rate_limits_for(venue, market), *row, "{venue} {market:?}");
        }
    }
    // Every roster venue is either in `named` or carries real numbers — no third state.
    for &v in crate::venues::VENUES {
        let declared = rate_limits_for(v, Market::Spot) != RateLimits::NOT_DECLARED;
        let is_named = named.iter().any(|(nv, _)| *nv == v);
        assert!(declared ^ is_named, "{v} must be exactly one of declared / explicitly empty");
    }
}

/// Only binance and aster are market-split; every other roster venue serves ONE row for both
/// markets. Left implicit this is the kind of thing that silently rots when a venue gains a
/// second host.
#[test]
fn market_split_venues_are_exactly_binance_and_aster() {
    for &v in crate::venues::VENUES {
        let spot = rate_limits_for(v, Market::Spot);
        let perp = rate_limits_for(v, Market::Perp);
        let split = spot != perp;
        assert_eq!(
            split,
            v == "binance" || v == "aster",
            "{v}: market-split is {split}, expected only binance/aster to differ"
        );
    }
    // ...and the split is real on the meter that motivates it, not a cosmetic difference.
    assert_ne!(BINANCE_SPOT.orders, BINANCE_PERP.orders);
    assert_ne!(BINANCE_SPOT.history, BINANCE_PERP.history);
    assert_ne!(ASTER_SPOT.orders, ASTER_PERP.orders);
    // Aster's HISTORY is split too, as of the sapi retune: sapi publishes 6000 where fapi
    // publishes 2400 — the same shape binance has, on the same two host roles. It used to be
    // deliberately UNsplit, with one fallback serving both hosts, which meant a spot backfill
    // braked on the fapi soft limit at 33 % of the budget discovery had already found for it.
    assert_ne!(ASTER_SPOT.history, ASTER_PERP.history);
}

/// The safety property this table exists to make checkable: no gate anywhere admits more than
/// the venue published. Enforced at COMPILE time by the `const _: ()` block above; asserted here
/// over the whole roster so a NEW row cannot land without it.
#[test]
fn no_row_admits_more_than_the_venue_published() {
    for &v in crate::venues::VENUES {
        for market in [Market::Spot, Market::Perp] {
            let row = rate_limits_for(v, market);
            assert!(row.stays_under_published_caps(), "{v} {market:?} out-spends the venue");
            for (name, m) in [
                ("orders", row.orders),
                ("ws_sends", row.ws_sends),
                ("rest_ip_weight", row.rest_ip_weight),
            ] {
                if let Some(published) = m.published_cap() {
                    assert!(m.admitted() <= published, "{v} {market:?} {name}");
                    assert!(m.admitted() > 0, "{v} {market:?} {name} would admit nothing");
                }
            }
            if let Some(ceiling) = row.history.published_ceiling_per_min() {
                assert!(row.history.soft_limit() < ceiling, "{v} {market:?} soft limit");
            }
        }
    }
    // An unknown venue is fail-closed, and equals Default.
    assert_eq!(rate_limits_for("nasdaq", Market::Spot), RateLimits::NOT_DECLARED);
    assert_eq!(RateLimits::default(), RateLimits::NOT_DECLARED);
    assert!(RateLimits::NOT_DECLARED.stays_under_published_caps());
}

/// The meters whose gate sits EXACTLY at the published cap — the reason the invariant is `<=`
/// rather than `<`. Pinned as an exhaustive list so that "every gate has headroom" is never
/// assumed, and so a NEW zero-margin gate has to be added here deliberately.
///
/// **Two today.** Deribit's matching-engine gate at 5/s, and Hyperliquid's per-IP REST weight
/// pool at 1200/min — the latter surfaced by moving the number into this table, where the
/// margin column made it visible. Both are today's literal behaviour; adding headroom to either
/// would change live pacing, which is a retune and belongs in its own change.
#[test]
fn zero_margin_meters_are_pinned() {
    let mut zero_margin = Vec::new();
    for &v in crate::venues::VENUES {
        for market in [Market::Spot, Market::Perp] {
            let row = rate_limits_for(v, market);
            for (name, m) in [
                ("orders", row.orders),
                ("ws_sends", row.ws_sends),
                ("rest_ip_weight", row.rest_ip_weight),
            ] {
                // `published_cap()` is `Some` only for `Meter::Published`, which is also the
                // only variant `admitted()` may be asked of here — an ungated meter panics.
                if m.published_cap().is_some_and(|p| p == m.admitted()) {
                    zero_margin.push(format!("{v}/{name}"));
                }
            }
        }
    }
    zero_margin.sort();
    zero_margin.dedup();
    assert_eq!(
        zero_margin,
        ["deribit/orders", "hyperliquid/rest_ip_weight"],
        "an un-noticed zero-margin gate appeared"
    );
    assert_eq!(DERIBIT.orders.published(), 5);
    assert_eq!(DERIBIT.orders.admitted(), 5);
    assert_eq!(HYPERLIQUID.rest_ip_weight.published(), 1200);
    assert_eq!(HYPERLIQUID.rest_ip_weight.admitted(), 1200);
}

/// The three venues that publish nothing machine-readable get a VARIANT, never a number. This is
/// the honesty rule as a test: a future edit that "fills in" a plausible ceiling for bybit/okx/
/// deribit fails here.
#[test]
fn unpublished_budgets_are_a_variant_not_a_fabricated_number() {
    for venue in ["bybit", "okx", "deribit"] {
        let h = rate_limits_for(venue, Market::Spot).history;
        assert!(matches!(h, History::Unweighted { .. }), "{venue} publishes no weight ceiling");
        assert_eq!(h.published_ceiling_per_min(), None, "{venue}");
    }
    // The WS meters with no documented venue cap: the admitted rate exists, the published one
    // does NOT, and asking for it is a panic rather than a plausible integer.
    for venue in ["bybit", "polymarket"] {
        let m = rate_limits_for(venue, Market::Spot).ws_sends;
        assert!(matches!(m, Meter::Unpublished { .. }), "{venue} documents no WS rate");
        assert_eq!(m.published_cap(), None, "{venue}");
        assert_eq!(m.admitted(), 30, "{venue}: vike's own runaway brake");
    }
    // Provenance says so at row level too.
    assert_eq!(POLYMARKET.provenance, Provenance::Unpublished);
}

/// An absent cap REFUSES to answer rather than returning a number a caller could mistake for a
/// venue fact. In a `const` initializer — how every bridge consumes this — each of these is a
/// COMPILE error, which is what makes an undeclared row unable to produce a gate; here they are
/// reached through the runtime registry, so they are ordinary panics a test can observe.
#[test]
#[should_panic(expected = "the venue publishes no cap for this meter")]
fn an_unpublished_meter_refuses_to_report_a_published_cap() {
    let _ = rate_limits_for("bybit", Market::Spot).ws_sends.published();
}

#[test]
#[should_panic(expected = "no gate is declared for this meter")]
fn an_ungated_meter_refuses_to_produce_a_rate() {
    let _ = rate_limits_for("oanda", Market::Spot).orders.admitted();
}

#[test]
#[should_panic(expected = "vike pages no history from this venue")]
fn a_not_paged_history_refuses_to_produce_a_page_delay() {
    let _ = rate_limits_for("polymarket", Market::Spot).history.page_delay();
}

#[test]
#[should_panic(expected = "this venue runs no request-weight meter")]
fn an_unweighted_history_refuses_to_produce_a_soft_limit() {
    let _ = rate_limits_for("bybit", Market::Spot).history.soft_limit();
}

/// Provenance: every row carrying VENUE numbers is dated, every row whose numbers are only ours
/// says `Unpublished`, and every empty row says `NotDeclared`. A row can never carry a published
/// cap AND claim nothing was published.
#[test]
fn provenance_matches_what_each_row_actually_carries() {
    for &v in crate::venues::VENUES {
        for market in [Market::Spot, Market::Perp] {
            let row = rate_limits_for(v, market);
            let has_published_cap = row.orders.published_cap().is_some()
                || row.ws_sends.published_cap().is_some()
                || row.rest_ip_weight.published_cap().is_some()
                || row.history.published_ceiling_per_min().is_some();
            match row.provenance {
                Provenance::Measured { on } => {
                    assert!(has_published_cap, "{v} {market:?}: dated but publishes nothing");
                    // ISO shape (`YYYY-MM` or `YYYY-MM-DD`), asserted on the SHAPE rather than
                    // on a year literal so this does not have to be edited every January.
                    let b = on.as_bytes();
                    assert!(
                        (b.len() == 7 || b.len() == 10) && b[4] == b'-',
                        "{v} {market:?}: {on:?} is not an ISO YYYY-MM[-DD] date"
                    );
                    assert!(b[..4].iter().all(u8::is_ascii_digit), "{v} {market:?}: {on:?}");
                }
                Provenance::Unpublished => {
                    assert!(!has_published_cap, "{v} {market:?}: claims nothing was published");
                    // ...but SOMETHING is gated, else this would be NotDeclared.
                    assert_ne!(row, RateLimits::NOT_DECLARED, "{v} {market:?}");
                }
                Provenance::NotDeclared => {
                    assert_eq!(row, RateLimits::NOT_DECLARED, "{v} {market:?}");
                }
            }
        }
    }
    // The dated rows are exactly the six crypto venues with venue-published caps (hyperliquid
    // joined when its per-IP weight pool moved in, in roster order).
    let dated: Vec<&str> = crate::venues::VENUES
        .iter()
        .copied()
        .filter(|v| {
            matches!(rate_limits_for(v, Market::Spot).provenance, Provenance::Measured { .. })
        })
        .collect();
    assert_eq!(dated, ["binance", "bybit", "okx", "deribit", "aster", "hyperliquid"]);
}

/// The market token round-trips against the string form `vike_binance::data::kline_market`
/// already uses, and nothing else parses — spot and perp are
/// exactly the pair whose budgets differ, so a near-miss must not resolve.
#[test]
fn market_strings_round_trip_and_reject_everything_else() {
    for m in [Market::Spot, Market::Perp] {
        assert_eq!(Market::from_market_str(m.as_str()), Some(m));
    }
    assert_eq!(Market::Spot.as_str(), "spot");
    assert_eq!(Market::Perp.as_str(), "perp");
    for bad in ["", "Spot", "SPOT", "perpetual", "futures", "swap", "linear"] {
        assert_eq!(Market::from_market_str(bad), None, "{bad:?} must not resolve");
    }
}
