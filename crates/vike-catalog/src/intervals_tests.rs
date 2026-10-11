use super::*;

/// The whole matrix, pinned VERBATIM — one row per `(venue, class)` pair the addressing table
    /// says exists. The playbook's STEP 1 requirement, and the reason it is keyed on the PAIR is
    /// that binance's two rows genuinely differ.
    ///
    /// ⚠ `#[rustfmt::skip]`: the last line of this literal is a `just new-venue` marker — see
    /// `crates/vike-catalog/src/addressing.rs`'s own `PINNED` for the rustfmt hazard it guards.
    #[rustfmt::skip]
    const PINNED: &[(&str, AssetClass, usize, IntervalEvidence)] = &[
        ("binance",     AssetClass::CryptoSpot,       15, IntervalEvidence::Probed),
        ("binance",     AssetClass::CryptoPerp,       14, IntervalEvidence::Probed),
        ("bybit",       AssetClass::CryptoSpot,       13, IntervalEvidence::BridgeTable),
        ("bybit",       AssetClass::CryptoPerp,       13, IntervalEvidence::BridgeTable),
        ("okx",         AssetClass::CryptoSpot,       13, IntervalEvidence::BridgeTable),
        ("okx",         AssetClass::CryptoPerp,       13, IntervalEvidence::BridgeTable),
        ("okx",         AssetClass::CryptoFuture,     13, IntervalEvidence::BridgeTable),
        ("okx",         AssetClass::Option,           13, IntervalEvidence::BridgeTable),
        ("deribit",     AssetClass::Option,           12, IntervalEvidence::BridgeTable),
        ("deribit",     AssetClass::CryptoFuture,     12, IntervalEvidence::BridgeTable),
        ("deribit",     AssetClass::CryptoPerp,       12, IntervalEvidence::BridgeTable),
        ("oanda",       AssetClass::Fx,                0, IntervalEvidence::Unmeasured),
        ("oanda",       AssetClass::Cfd,               0, IntervalEvidence::Unmeasured),
        ("ig",          AssetClass::Fx,                0, IntervalEvidence::Unmeasured),
        ("ig",          AssetClass::Cfd,               0, IntervalEvidence::Unmeasured),
        ("ig",          AssetClass::Equity,            0, IntervalEvidence::Unmeasured),
        ("ig",          AssetClass::Index,             0, IntervalEvidence::Unmeasured),
        ("dukascopy",   AssetClass::Fx,                0, IntervalEvidence::Unmeasured),
        ("dukascopy",   AssetClass::Cfd,               0, IntervalEvidence::Unmeasured),
        ("polymarket",  AssetClass::PredictionMarket,  0, IntervalEvidence::Unmeasured),
        ("ibkr",        AssetClass::Equity,            0, IntervalEvidence::Unmeasured),
        ("ibkr",        AssetClass::Fx,                0, IntervalEvidence::Unmeasured),
        ("ctrader",     AssetClass::Fx,                0, IntervalEvidence::Unmeasured),
        ("ctrader",     AssetClass::Cfd,               0, IntervalEvidence::Unmeasured),
        ("alpaca",      AssetClass::Equity,            0, IntervalEvidence::Unmeasured),
        ("alpaca",      AssetClass::CryptoSpot,        0, IntervalEvidence::Unmeasured),
        ("aster",       AssetClass::CryptoSpot,        0, IntervalEvidence::Unmeasured),
        ("aster",       AssetClass::CryptoPerp,        0, IntervalEvidence::Unmeasured),
        ("hyperliquid", AssetClass::CryptoSpot,        0, IntervalEvidence::Unmeasured),
        ("hyperliquid", AssetClass::CryptoPerp,        0, IntervalEvidence::Unmeasured),
        // vike:new-venue:row // TODO(new-venue: {venue}): a scaffolded venue addresses NO class and therefore owns NO
        // vike:new-venue:row // row here. Add one row per class the moment `addressing_for`'s row for it names one,
        // vike:new-venue:row // or `every_addressed_pair_has_a_pinned_row` stays red naming the pair.
    ];

#[test]
fn interval_matrix_is_pinned() {
    for (venue, class, count, evidence) in PINNED {
        let row = intervals_for(venue, *class);
        assert_eq!(
            row.intervals.len(),
            *count,
            "{venue}/{class:?}: interval count drifted ({:?})",
            row.intervals
        );
        assert_eq!(row.evidence, *evidence, "{venue}/{class:?}: evidence drifted");
    }
}

/// This module's own source, read at RUNTIME rather than `include_str!`ed — the idiom
/// `crates/vike-catalog/src/addressing.rs` uses, and the reason this file stays out of
/// `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs`'s ratchet.
fn own_source() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/intervals.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Completeness, the playbook shape and WELDED TO THE ADDRESSING TABLE: every `(venue, class)`
/// pair `addressing_for` says exists has a pinned row, and no pinned row names a pair it says
/// does not. Adding a venue — or widening one venue's class set — fails here until the rows
/// exist.
#[test]
fn every_addressed_pair_has_a_pinned_row() {
    let mut expected = 0usize;
    for &venue in vike_model::VENUES {
        for &class in crate::addressing_for(venue).classes {
            expected += 1;
            assert!(
                PINNED.iter().any(|(v, c, ..)| *v == venue && *c == class),
                "{venue} addresses {class:?} and has no pinned interval row — a pair the \
                     addressing table says exists must be CLASSIFIED here, even when the answer is \
                     `Unmeasured`"
            );
        }
    }
    assert_eq!(
        PINNED.len(),
        expected,
        "the pin carries a row for a (venue, class) pair `addressing_for` does not name (or is \
             short one)"
    );
    for (venue, class, ..) in PINNED {
        assert!(
            crate::addressing_for(venue).addresses(*class),
            "{venue}/{class:?} is pinned here but `addressing_for` says this venue cannot \
                 address that class — the two tables disagree about which pairs exist"
        );
    }
}

/// Every roster venue has a NAMED arm, even the ones whose value equals a shared constant — the
/// same convention, and the same SOURCE-scan reason, as `addressing_for`'s
/// `every_roster_venue_has_a_named_row`: a value comparison cannot tell a deliberate
/// `UNMEASURED` row from an absent one.
#[test]
fn every_roster_venue_has_a_named_arm() {
    let src = own_source();
    for &venue in vike_model::VENUES {
        assert!(
            src.contains(&format!("\"{venue}\" =>")),
            "roster venue {venue} has no NAMED arm in `intervals_for` — it would fall through \
                 to the fallback, and a named arm is what proves a venue was classified rather \
                 than forgotten"
        );
    }
}

/// The scan above must be able to FAIL — without this it answers "named" for a venue that is
/// not there and the completeness test is green over an empty table.
#[test]
fn the_named_arm_scan_can_actually_fail() {
    let src = own_source();
    assert!(src.contains("\"deribit\" => "), "the scan cannot see a real arm");
    assert!(!src.contains("\"no-such-venue\" =>"), "the scan matches a venue that has no arm");
}

/// **THE OWNER'S POINT, machine-checked.** *"Every venue serves 5m / 15m / 1h on every
/// instrument type it lists"* — MEASURED 2026-09-16, asserted here over every row that carries
/// evidence at all. An `Unmeasured` row is exempt because it claims nothing.
#[test]
fn every_measured_pair_serves_five_fifteen_and_an_hour() {
    for (venue, class, ..) in PINNED {
        let row = intervals_for(venue, *class);
        if row.evidence == IntervalEvidence::Unmeasured {
            continue;
        }
        for iv in ["5m", "15m", "1h"] {
            assert_eq!(
                row.verdict(iv),
                IntervalVerdict::Serves,
                "{venue}/{class:?} does not serve {iv}, which contradicts the 2026-09-16 \
                     measurement this table is the commit of"
            );
        }
    }
}

/// **The two measured exceptions, as the only two.** `1s` is binance SPOT alone and `3d` is
/// binance alone, across every row the probe axis reaches.
#[test]
fn the_two_exceptions_are_exactly_the_two_that_were_measured() {
    for (venue, class, ..) in PINNED {
        let row = intervals_for(venue, *class);
        let serves = |iv: &str| row.verdict(iv) == IntervalVerdict::Serves;
        assert_eq!(
            serves("1s"),
            *venue == "binance" && *class == AssetClass::CryptoSpot,
            "{venue}/{class:?} disagrees with `1s` being binance SPOT only"
        );
        assert_eq!(
            serves("3d"),
            *venue == "binance",
            "{venue}/{class:?} disagrees with `3d` being binance only"
        );
    }
}

/// The cross-check that makes the two exceptions above more than a restatement: bybit's and
/// okx's OWN code tables — independent of the probe, and written before it — are exactly the
/// probe axis minus those same two intervals. Two tables and one measurement agreeing is what
/// lets binance's rows be stated as the axis, and the axis minus `1s`.
#[test]
fn the_coded_venues_are_the_probe_axis_minus_the_two_exceptions() {
    let axis_minus: Vec<&str> =
        PROBED_AXIS.iter().copied().filter(|iv| *iv != "1s" && *iv != "3d").collect();
    assert_eq!(BYBIT_CODED, axis_minus.as_slice(), "bybit's `interval_code` moved");
    assert_eq!(OKX_CODED, axis_minus.as_slice(), "okx's `bar_code` moved");
}

/// ⚠ **The three-valued answer, exercised in all three directions.** Collapsing it to a `bool`
/// is a bug whichever way it collapses, and this is the test that says so.
#[test]
fn an_unmeasured_pair_is_neither_a_yes_nor_a_no() {
    // OPEN: nothing in this workspace refuses a hyperliquid interval and nobody probed one.
    let hl = intervals_for("hyperliquid", AssetClass::CryptoPerp);
    assert_eq!(hl.verdict("1h"), IntervalVerdict::Unmeasured);
    assert_eq!(hl.verdict("7s"), IntervalVerdict::Unmeasured);
    // CLOSED over the axis: `1s` was asked of binance futures and did not come back...
    let perp = intervals_for("binance", AssetClass::CryptoPerp);
    assert_eq!(perp.verdict("1s"), IntervalVerdict::Refuses);
    // ...while `8h` was never asked of anyone, so this table says nothing about it.
    assert_eq!(perp.verdict("8h"), IntervalVerdict::Unmeasured);
    // CLOSED entirely: the bridge's own table refuses, so the answer is total.
    let deribit = intervals_for("deribit", AssetClass::CryptoPerp);
    assert_eq!(deribit.verdict("4h"), IntervalVerdict::Refuses);
    assert_eq!(deribit.verdict("8h"), IntervalVerdict::Refuses);
    assert_eq!(deribit.verdict("10m"), IntervalVerdict::Serves);
}

/// A class the addressing table does not name gets a REFUSAL rather than an unmeasured shrug,
/// and an unknown venue reaches the same answer through that table's own fallback.
#[test]
fn a_class_the_venue_cannot_address_refuses_every_interval() {
    for (venue, class) in [
        ("bybit", AssetClass::Option),
        ("deribit", AssetClass::CryptoSpot),
        ("fxcm", AssetClass::Fx),
        ("no-such-venue", AssetClass::CryptoSpot),
    ] {
        let row = intervals_for(venue, class);
        assert_eq!(row, VenueIntervals::UNADDRESSABLE, "{venue}/{class:?}");
        assert_eq!(row.verdict("1h"), IntervalVerdict::Refuses, "{venue}/{class:?}");
    }
}

/// **PHASE 4's "declared as needing nothing", as an assertion rather than a sentence.** 0061
/// names okx, deribit and hyperliquid as the venues a class claim cannot help — okx and deribit
/// interpolate the caller's symbol verbatim, hyperliquid resolves through its own registry —
/// and a declaration nothing compares is a blank. Three falsifiable claims per venue:
///
/// 1. **No claim is REQUIRED** — `must_claim()` is `false`, which is a POSITIVE measurement of
///    unambiguity rather than an absence of one (the whole point of `BareSymbol::Unmeasured`
///    existing as a third state).
/// 2. **A claim cannot change the route** — the venue's own id already names the product, which
///    is exactly what `Naming::VenueNative` says. A `Naming::PerpSuffix` venue is the contrast:
///    there the claim genuinely selects a book.
/// 3. **The venue addresses something** — an empty class set would make claims 1 and 2 vacuous.
///
/// ⚠ The claim NOT made here: that a claim is ignored. It is CHECKED, at the one place a
/// `VenueNative` venue can check it without importing the venue's symbology — the seed verb's
/// door (`crates/vike-datahub/src/server/seed_series.rs`'s `seed_series_verb`), against this crate's
/// `addressing_for`. What that door still cannot catch is declared beside it.
#[test]
fn the_venues_that_need_nothing_say_so() {
    for venue in ["okx", "deribit", "hyperliquid"] {
        let row = crate::addressing_for(venue);
        assert!(!row.must_claim(), "{venue}: a venue that needs nothing must need no claim");
        assert_eq!(
            row.naming,
            crate::Naming::VenueNative,
            "{venue}: the venue's own id is what makes a claim unnecessary"
        );
        assert!(!row.classes.is_empty(), "{venue}: an empty class set makes this vacuous");
    }
    // The contrast that keeps the assertion from being true of everything.
    assert_eq!(crate::addressing_for("binance").naming, crate::Naming::PerpSuffix);
    assert!(crate::addressing_for("bybit").must_claim());
}

/// Every interval any row names must have a bar width the store's own vocabulary can parse,
/// EXCEPT the two calendar steps `vike_model::time::interval_ms` has never been able to read.
/// The exception is declared rather than silently tolerated: `1w` and `1M` are real venue
/// intervals this workspace cannot partition, which is why
/// `vike_datahub_client::seed::SEED_INTERVALS` excludes them.
#[test]
fn every_named_interval_is_a_venue_interval_and_the_two_unparsable_ones_are_declared() {
    for (venue, class, ..) in PINNED {
        for iv in intervals_for(venue, *class).intervals {
            let parsable = vike_model::time::interval_ms(iv).is_some_and(|ms| ms > 0);
            assert!(
                parsable || matches!(*iv, "1w" | "1M"),
                "{venue}/{class:?} names {iv:?}, which the store cannot measure and which is \
                     not one of the two declared calendar steps"
            );
        }
    }
}

/// Each row's set is ASCENDING BY BAR WIDTH and free of duplicates — not cosmetic: the sets are
/// read as ladders beside each other in the pin, and a duplicate would mean two arms of a
/// bridge's code table collapsed without anyone noticing. The two calendar steps are given
/// nominal widths here because they have none in the store's vocabulary.
#[test]
fn every_row_is_a_ladder() {
    for set in [BINANCE_SPOT, BINANCE_FUTURES, BYBIT_CODED, OKX_CODED, DERIBIT_CODED] {
        let mut seen = std::collections::BTreeSet::new();
        for iv in set {
            assert!(seen.insert(*iv), "{iv:?} appears twice in {set:?}");
        }
        let widths: Vec<i64> = set
            .iter()
            .map(|iv| match *iv {
                "1w" => 7 * 86_400_000,
                "1M" => 28 * 86_400_000,
                other => vike_model::time::interval_ms(other).unwrap(),
            })
            .collect();
        assert!(widths.windows(2).all(|w| w[0] < w[1]), "not ascending: {set:?}");
    }
}
