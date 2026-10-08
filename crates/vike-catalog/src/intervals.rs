//! `intervals` — **which BAR INTERVALS each `(venue, instrument type)` pair serves, and where that
//! answer was read from.**
//!
//! Phase 4 of `docs/decisions/0061-an-instrument-names-its-kind.md`, the half that record calls
//! *"the owner's interval table keys on the same pair and lands here"*. It is the playbook's STEP 1
//! in the strict sense — **one row per (venue, class) pair, each citing the source it was read
//! from, a verbatim matrix pin, a completeness test over [`vike_model::VENUES`], and a
//! `vike:new-venue:row` marker** — and **nothing consumes it yet**. That is deliberate:
//! `crates/vike-model/CLAUDE.md`'s "Per-venue capability maps (the playbook)" section requires STEP 1 to merge BYTE-IDENTICAL and
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
        // The venues with NO kline collector in `vike_datahub::backfill::KLINE_SOURCES`. Each
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

#[path = "intervals_tests.rs"]
#[cfg(test)]
mod intervals_tests;
