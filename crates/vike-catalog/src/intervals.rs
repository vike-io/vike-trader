//! `intervals` — **which BAR INTERVALS each `(venue, instrument type)` pair serves, and where that
//! answer was read from.**
//!
//! Phase 4 of `docs/decisions/0061-an-instrument-names-its-kind.md`, the half that record calls
//! *"the owner's interval table keys on the same pair and lands here"*. It is the playbook's STEP 1
//! in the strict sense — **one row per (venue, class) pair, each citing the source it was read
//! from, a verbatim matrix pin, a completeness test over [`vike_model::VENUES`], and a
//! `vike:new-venue:row` marker** — and **nothing consumes it yet**. That is deliberate:
//! `CLAUDE.md`'s *Per-venue capability maps* section requires STEP 1 to merge BYTE-IDENTICAL and
//! STEP 2 to flip behaviour one venue at a time, and the divergences this table exposes (deribit's
//! missing `4h`, binance's spot-only `1s`) are PINNED here rather than fixed.
//!
//! # Why it keys on the PAIR and not on the venue
//!
//! Because exactly one venue's answer moves with the instrument type, and pretending otherwise
//! would lose the fact. **MEASURED 2026-09-16** by
//! `crates/vike-datahub/tests/venue_interval_matrix.rs`, which drives the daemon's own
//! `real_backfill_table` against the live public endpoints over [`PROBED_AXIS`]:
//!
//! * **every probed venue serves `5m`, `15m` and `1h` on every instrument type it lists** — the
//!   headline, and the reason that harness exists: the plumbing was missing, not the data;
//! * **`1s` is binance SPOT only** — so binance's two rows below genuinely differ;
//! * **`3d` is binance only.**
//!
//! ⚠ **The harness probed THREE venues — binance, bybit, okx — and the collector table dispatches
//! SIX.** aster, deribit and hyperliquid have never been probed, and this table says so in the one
//! way that cannot be mistaken for an answer: their rows carry [`IntervalEvidence::Unmeasured`] or
//! are read off the bridge's OWN code table instead. Nothing here is inferred from a venue's
//! documentation, and nothing is inferred from a sibling venue's behaviour.
//!
//! ⚠ **The harness's results live on STDOUT and are committed nowhere else.** This table IS the
//! commit of the two exceptions above; a full cell-by-cell capture is follow-up work, not claimed
//! here.
//!
//! # The fallback REFUSES to answer, which is not the same as refusing
//!
//! [`crate::addressing_for`]'s fallback answers "addresses nothing, must be claimed" — a REFUSAL,
//! because a wrong route writes a wrong book. An interval question has a third honest answer:
//! *nobody looked*. Collapsing that into "no" would make this table claim binance does not serve
//! `8h` — an interval [`PROBED_AXIS`] never asked about — so [`IntervalVerdict`] has three
//! variants and [`VenueIntervals::verdict`] answers [`IntervalVerdict::Unmeasured`] wherever the
//! evidence does not reach. **A caller that treats `Unmeasured` as `Serves` has widened this
//! table, and a caller that treats it as `Refuses` has narrowed it; both are wrong, and the enum is
//! what makes that a deliberate choice rather than a default.**
//!
//! # What this table is NOT
//!
//! It is not `vike_datahub_client::seed::SEED_INTERVALS`. That constant is a WIRE allowlist both
//! ends of one protocol predict with, deliberately narrower than any one venue, and its own doc
//! already declares the divergence this table measures — *"`4h` is in this set and outside the new
//! intersection"* for deribit. This table is the per-venue answer that constant's doc says "is what
//! makes this answerable per venue"; wiring the two together is STEP 2 and is not done here.
//!
//! It is also not a routing table, for the same reason [`crate::addressing_for`] is not: no venue's
//! own word for an interval (`"60"`, `"1H"`, `"1D"`) appears in this crate. The bridges keep their
//! own code tables and stay the authority for the wire spelling.

use vike_model::AssetClass;

/// **The intervals the measurement ASKED about** — the axis
/// `crates/vike-datahub/tests/venue_interval_matrix.rs` sweeps, restated here because an absence
/// from a [`IntervalEvidence::Probed`] row only means "refused" for an interval that was actually
/// asked.
///
/// An interval outside this axis has no probed verdict at all, which is what
/// [`IntervalVerdict::Unmeasured`] exists to say. Binance's `8h` is the standing example: the venue
/// publishes one, nobody probed it, and this table does not pretend either way.
pub const PROBED_AXIS: [&str; 15] =
    ["1s", "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1d", "3d", "1w", "1M"];

/// Where a row's interval set came from — and therefore how far an ABSENCE from it reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalEvidence {
    /// **CLOSED.** Read off the bridge's OWN interval code table, which refuses everything outside
    /// the set BEFORE a request leaves the box. An absence here is a refusal this workspace
    /// performs itself, so it is a total answer for every interval.
    BridgeTable,
    /// **CLOSED OVER [`PROBED_AXIS`].** Measured against the live venue. An interval inside the
    /// axis and absent from the set was asked and did not come back; one outside the axis was never
    /// asked.
    Probed,
    /// **OPEN.** The bridge carries no code table and nobody has probed this pair: the caller's
    /// string is interpolated and the venue answers. The set is EMPTY and every interval is
    /// [`IntervalVerdict::Unmeasured`] — the row is a declaration that this pair is unmeasured, not
    /// a claim that it serves nothing.
    Unmeasured,
    /// **CLOSED AND EMPTY.** [`crate::addressing_for`] says this venue's data path cannot address
    /// this class at all, so there is no pair here to serve an interval for. Every interval is
    /// [`IntervalVerdict::Refuses`], and the refusal is the addressing table's, not this one's.
    Unaddressable,
}

/// One `(venue, class)` pair's answer about one interval. **Three-valued on purpose** — see this
/// module's doc for why collapsing it to a `bool` is a bug in whichever direction it is collapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalVerdict {
    /// The pair serves it, on the evidence the row cites.
    Serves,
    /// The pair does NOT serve it, and the row's evidence reaches far enough to say so.
    Refuses,
    /// **Nobody looked.** Not a refusal and not a permission.
    Unmeasured,
}

/// One `(venue, class)` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueIntervals {
    /// The intervals this pair is known to SERVE, in ascending bar width. Empty under
    /// [`IntervalEvidence::Unmeasured`] and [`IntervalEvidence::Unaddressable`].
    pub intervals: &'static [&'static str],
    /// How far an absence from [`Self::intervals`] reaches.
    pub evidence: IntervalEvidence,
}

impl VenueIntervals {
    /// The answer for a class the venue's addressing row does not name.
    pub const UNADDRESSABLE: Self =
        Self { intervals: &[], evidence: IntervalEvidence::Unaddressable };

    /// The answer for a pair with no code table and no measurement — the scaffolded row, and the
    /// row every venue with no kline collector in this workspace carries.
    pub const UNMEASURED: Self = Self { intervals: &[], evidence: IntervalEvidence::Unmeasured };

    /// **Does this pair serve `interval`?** Three-valued; see [`IntervalVerdict`].
    #[must_use]
    pub fn verdict(&self, interval: &str) -> IntervalVerdict {
        if self.intervals.contains(&interval) {
            return IntervalVerdict::Serves;
        }
        match self.evidence {
            IntervalEvidence::BridgeTable | IntervalEvidence::Unaddressable => {
                IntervalVerdict::Refuses
            }
            IntervalEvidence::Probed if PROBED_AXIS.contains(&interval) => IntervalVerdict::Refuses,
            IntervalEvidence::Probed | IntervalEvidence::Unmeasured => IntervalVerdict::Unmeasured,
        }
    }
}

// The interval sets, named so a row reads as a declaration rather than a literal. Each is cited at
// the row that uses it, and each is ASCENDING BY BAR WIDTH so the pin reads as a ladder.

/// Binance SPOT — [`PROBED_AXIS`] entire. The `1s` is what makes this row differ from its sibling.
const BINANCE_SPOT: &[&str] =
    &["1s", "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1d", "3d", "1w", "1M"];

/// Binance FUTURES (USDⓈ-M and COIN-M alike) — [`PROBED_AXIS`] minus `1s`.
const BINANCE_FUTURES: &[&str] =
    &["1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1d", "3d", "1w", "1M"];

/// `crates/bridges/bybit/src/data.rs`'s `interval_code`, arm for arm.
const BYBIT_CODED: &[&str] =
    &["1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1d", "1w", "1M"];

/// `crates/bridges/okx/src/data.rs`'s `bar_code`, arm for arm.
const OKX_CODED: &[&str] =
    &["1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1d", "1w", "1M"];

/// `crates/bridges/deribit/src/data.rs`'s `resolution_code`, arm for arm. **The odd one out in both
/// directions**: no `4h`, no `1w`, no `1M`, and it uniquely carries `10m` and `3h`.
const DERIBIT_CODED: &[&str] =
    &["1m", "3m", "5m", "10m", "15m", "30m", "1h", "2h", "3h", "6h", "12h", "1d"];

/// **Which intervals `venue` serves for `class`.** A class the venue's addressing row does not name
/// is [`VenueIntervals::UNADDRESSABLE`] before any row is consulted, so the two tables cannot
/// disagree about which pairs exist.
///
/// An unknown venue string is [`VenueIntervals::UNADDRESSABLE`] too — it reaches that answer
/// through [`crate::addressing_for`]'s own refusing fallback rather than through a rule here.
#[must_use]
pub fn intervals_for(venue: &str, class: AssetClass) -> VenueIntervals {
    // THE PAIR IS THE KEY, and the first half of it is the addressing table's answer rather than a
    // second opinion: a class this venue cannot address has no interval question.
    if !crate::addressing_for(venue).addresses(class) {
        return VenueIntervals::UNADDRESSABLE;
    }
    match venue {
        // MEASURED 2026-09-16 by `crates/vike-datahub/tests/venue_interval_matrix.rs` over
        // [`PROBED_AXIS`]. **The one venue whose answer moves with the instrument type**, and the
        // reason this table keys on the pair: `1s` came back on spot and on nothing else. The
        // binance family bridge carries NO interval code table at all
        // (`crates/bridges/binance/src/family/klines.rs`'s `klines_url` interpolates the caller's
        // string), so `Probed` is the strongest evidence available here — an absence means the
        // probe asked and got nothing back, never that this workspace refused.
        //
        // ⚠ Both futures books share ONE row, deliberately: 0061 refuses to split `CryptoPerp` into
        // USDⓈ-M and COIN-M, and `crates/bridges/binance/src/instruments.rs` — the venue's own
        // listing — is what separates them. A second row here would be this table doing routing.
        "binance" => VenueIntervals {
            intervals: if matches!(class, AssetClass::CryptoSpot) {
                BINANCE_SPOT
            } else {
                BINANCE_FUTURES
            },
            evidence: IntervalEvidence::Probed,
        },
        // `crates/bridges/bybit/src/data.rs`'s `interval_code`. Read off the CODE TABLE rather than
        // off the probe although both exist, because the code table is CLOSED: it refuses `1s` and
        // `3d` before the request leaves the box, which is a stronger statement than "the probe got
        // nothing back" and is the same statement the probe measured.
        //
        // Spot and perp share one row: `interval_code` is consulted before `Category` is chosen, so
        // the venue's own category cannot move this answer.
        "bybit" => {
            VenueIntervals { intervals: BYBIT_CODED, evidence: IntervalEvidence::BridgeTable }
        }
        // `crates/bridges/okx/src/data.rs`'s `bar_code`, and identical in MEMBERSHIP to bybit's
        // above while being a different table with a different wire vocabulary (`1H`, `1D`). The
        // duplication is the playbook's, not an accident: two venues agreeing is a measurement.
        //
        // One row for all four classes. `REST_CANDLES` interpolates the caller's `instId` verbatim
        // and `bar_code` never sees the instrument type, so a swap, a dated future and an option
        // are asked the same question. **This is okx's half of "declared as needing nothing"** —
        // see `the_venues_that_need_nothing_say_so` in this module's tests.
        "okx" => VenueIntervals { intervals: OKX_CODED, evidence: IntervalEvidence::BridgeTable },
        // `crates/bridges/deribit/src/data.rs`'s `resolution_code`, whose own test
        // `resolution_code_errors_on_intervals_deribit_does_not_serve` pins the gaps.
        //
        // ⚠ **THE ROW THAT MAKES THIS TABLE WORTH HAVING.** `4h` is inside
        // `vike_datahub_client::seed::SEED_INTERVALS` and outside this set, so a `4h` deribit seed
        // is refused by the venue's own table after passing the wire allowlist. That constant's doc
        // already declares the divergence and says a per-venue table is what makes it answerable —
        // this is that table, and PINNING the divergence rather than resolving it is STEP 1's whole
        // discipline. Never probed: the harness's roster predates this venue's collector.
        "deribit" => {
            VenueIntervals { intervals: DERIBIT_CODED, evidence: IntervalEvidence::BridgeTable }
        }
        // ⚠ **UNMEASURED, and that is the honest row.** Aster pages through the binance family's
        // `klines_url`, so it carries no code table either — but the probe roster has never named
        // it, so there is no measurement to copy and the binance row above is a statement about
        // binance. Inferring this row from the family shape is exactly the guess this column exists
        // to refuse.
        "aster" => VenueIntervals::UNMEASURED,
        // ⚠ UNMEASURED for the same reason by a different route:
        // `crates/bridges/hyperliquid/src/history.rs`'s `candle_snapshot_body` sends the caller's
        // interval string verbatim in the JSON body, so nothing in this workspace refuses one, and
        // the probe roster has never named this venue.
        "hyperliquid" => VenueIntervals::UNMEASURED,
        // The venues with NO kline collector in `vike_backfill::kline_source::KLINE_SOURCES`. Each
        // has a bar path of some kind — oanda's candles, ig's prices, ctrader's trendbars,
        // dukascopy's `.bi5` archive, alpaca's bars — and none has an interval table in this
        // workspace or a row in the probe roster, so each is UNMEASURED rather than assumed. A
        // NAMED row, not an omission: the playbook's rule, and what proves they were classified.
        "oanda" => VenueIntervals::UNMEASURED,
        "ig" => VenueIntervals::UNMEASURED,
        "dukascopy" => VenueIntervals::UNMEASURED,
        "ctrader" => VenueIntervals::UNMEASURED,
        "alpaca" => VenueIntervals::UNMEASURED,
        // ⚠ `crates/bridges/polymarket/src/data.rs` addresses an ERC-1155 outcome token's price
        // history, which is not a kline grid at all. UNMEASURED rather than a set, because naming
        // one would assert this venue has a bar-interval axis.
        "polymarket" => VenueIntervals::UNMEASURED,
        // ⚠ `crates/bridges/vike-ibkr` has a historical collector behind its own feature and no
        // interval table in this crate's reach. UNMEASURED.
        "ibkr" => VenueIntervals::UNMEASURED,
        // ⚠ `crates/bridges/fxcm` has no `data.rs` at all, so its addressing row addresses NO class
        // and the guard above answers before this arm is reached. The arm exists because a NAMED
        // row is what proves the venue was classified rather than forgotten — the identical
        // convention `addressing_for` follows for this venue.
        "fxcm" => VenueIntervals::UNADDRESSABLE,
        // vike:new-venue:row // TODO(new-venue: {venue}): a scaffolded venue addresses NO class, so the guard at the
        // vike:new-venue:row // top of this function already answers UNADDRESSABLE for it and this arm is never
        // vike:new-venue:row // reached. Keep it anyway — a NAMED arm is what `every_roster_venue_has_a_named_arm`
        // vike:new-venue:row // reads. The moment this venue's `addressing_for` row names a class, replace UNMEASURED
        // vike:new-venue:row // with what the bridge's own interval table says (or leave it and pin the measurement's
        // vike:new-venue:row // absence), and add one PINNED row per class it addresses.
        // vike:new-venue:row "{venue}" => VenueIntervals::UNMEASURED,
        _ => VenueIntervals::UNADDRESSABLE,
    }
}

#[cfg(test)]
mod tests {
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
    /// `crates/vike-ops/tests/compile_time_path_gate.rs`'s ratchet.
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
    /// door (`crates/vike-datahub/src/server.rs`'s `seed_series_verb`), against this crate's
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
}
