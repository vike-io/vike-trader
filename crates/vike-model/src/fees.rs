//! `FeeSchedule` — the per-venue transaction-fee model (integration steal-list #5).
//!
//! Two seams, kept apart on purpose (see `docs/superpowers/specs/2026-07-17-fee-model-design.md`):
//! - **data**: [`FeeSchedule`] describes a venue's fee *shape and rate* (crypto maker/taker bps,
//!   equities per-share+floor, or free), and [`fee_schedule_for`] is the per-venue registry of
//!   real published tier-0 defaults — the twin of [`crate::venue_caps::caps_for`], living here in
//!   `vike-model` (the bottom layer every crate reaches down to) for the same layering reason.
//! - **logic**: [`FeeSchedule::commission`] is the pure fee-application function (LEAN's
//!   `GetOrderFee` analog): `(is_maker, qty, px) -> commission` in quote currency, deterministic
//!   (no clock/env reads), so it is backtest-safe.
//!
//! Deliberately a SEPARATE module from [`crate::venue_caps`] rather than a `VenueCaps` field:
//! `VenueCaps` is a stable compile-time capability matrix with exact-equality unit tests, whereas
//! fee numbers are a drift-prone business fact that changes when a venue re-prices — mixing them
//! would churn the capability tests on every rate move.
//!
//! The `slippage` cost stays SEPARATE (it lives on the paper/backtest engine, not here); this
//! module models commission only. Perp funding is a different path
//! (`FundingEvent`/`Account.funding_paid`) and is untouched.
//!
//! Prediction-market probability curve ([`FeeSchedule::ProbabilityScaled`], pm-economics lane):
//! `fee = qty × rate × p × (1−p)` with `p` the fill price clamped into the `[0,1]` probability
//! domain — the fee vanishes at the certainty bounds (p→0 or p→1) and peaks at p=0.5, mirroring
//! how prediction-market venues scale fees by outcome uncertainty (the mechanism behind
//! NautilusTrader's `PolymarketFeeModel`, reimplemented independently). Maker fills may
//! additionally earn a rebate: `maker_rebate_share` of the equivalent TAKER fee is subtracted, so
//! with `maker_rate == 0.0` and a positive share the maker commission is NEGATIVE (a rebate — the
//! sign convention below). **Registry decision (default-compatible, byte-identical):**
//! [`fee_schedule_for`] KEEPS returning [`FeeSchedule::Free`] for `"polymarket"` — the paper fill
//! path (`vike_backtest::paper` via `vike_mount::make_engine`, #414/#416) and the snapshot cost
//! display consume that registry directly, so flipping the default would change existing users'
//! numbers. The fee curve is an OPT-IN: [`fee_schedule_for_with_pm_curve`] returns
//! [`POLYMARKET_V2_FEE_CURVE`] — the verified **2026 Polymarket V2 regime** (V2 launched
//! 2026-04-28; sports taker `0.05`, maker rebate `15 %` of collected taker fees as a NEGATIVE maker
//! commission) — for `"polymarket"`, and delegates to [`fee_schedule_for`] for every other venue.
//! The separate all-zero [`POLYMARKET_PROB_CURVE`] is retained as the numerically-identical-to-
//! `Free` SHAPE template (its zero rates are relied on by the backtest's zero-rate regression
//! guard). See [`POLYMARKET_V2_FEE_CURVE`]'s doc for the per-market rate variation, per-market
//! rebate pooling, and USDC-at-match caveats that a per-venue, per-fill schedule cannot express —
//! each a `vike-backtest` follow-up, not fixable in `vike-model` alone.
//!
//! **Maker/taker classification caveat (matters most for a rebate-bearing schedule).** Every
//! commission call site decides `is_maker` from the ORDER KIND, not from crossing aggressiveness:
//! `vike_backtest::paper::PaperExecutionClient` and the backtest engine's `dispatch_fill` both
//! classify `OrderKind::Limit` as maker. A MARKETABLE limit (priced through the book, filled
//! immediately at the next bar's open) is therefore booked as a maker fill. Under the fee shapes
//! that predate this module that only understated the cost by `taker − maker`; under a
//! [`FeeSchedule::ProbabilityScaled`] with a positive `maker_rebate_share` it flips the SIGN — an
//! aggressive limit EARNS a rebate instead of paying the taker fee, so a strategy that uses limits
//! as marketable orders books systematically negative costs. Reclassifying by marketability would
//! change the fee of every existing limit fill (a parity-gated path), so it stays as-is and is
//! documented here: model aggressive orders as `OrderKind::Market` when the fee sign matters.
//!
//! Residual Deribit-options paper gap (fee model follow-up 2): Deribit's real options fee is
//! `min(0.03% × underlying, 12.5% × premium)`. The paper fill context carries only the traded
//! premium (no underlying/index price), so [`FeeSchedule::commission`] on the
//! [`FeeSchedule::PercentOfUnderlying`] shape can only apply the premium-cap-bounded approximation
//! — where the 0.03% is charged against the premium, not the (larger) underlying. That
//! UNDERSTATES the true fee for options priced well below their underlying, but it is a
//! strictly-typed, cap-modeling improvement over the previous flat `PercentMakerTaker`, and the
//! accurate [`FeeSchedule::commission_with_underlying`] is available to any caller that supplies an
//! underlying price. Live Deribit fills report the venue's real commission, so the gap is confined
//! to PAPER/backtest of Deribit options.

/// A venue's fee shape + rate. `Copy` (all fields scalar) so it is cheap to pass by value.
///
/// Sign convention (matching NautilusTrader and the venues' own execution reports): a positive
/// commission is a cost; a negative commission is a rebate. Two ways to reach a rebate: a live
/// maker-rebate rate fetched from a venue, or a STATICALLY constructed
/// [`FeeSchedule::ProbabilityScaled`] with `maker_rebate_share > 0` (its maker side then charges
/// `maker_fee − share × taker_fee`, negative whenever the rebate outweighs the maker rate). Every
/// entry in the [`fee_schedule_for`] registry is non-negative on both sides.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FeeSchedule {
    /// Crypto maker/taker as **basis points of quote-notional** (`qty * px`). The required variant.
    PercentMakerTaker { maker_bps: f64, taker_bps: f64 },
    /// Equities-shaped: a per-share fee, a minimum floor, and a maximum-percent-of-notional cap
    /// (the Interactive Brokers fixed schedule shape). `is_maker` does not apply.
    PerShareWithFloor { per_share: f64, min: f64, max_pct: f64 },
    /// Deribit-options-shaped: `bps` of the **underlying** notional, capped at `premium_cap_pct` of
    /// the **premium** notional (Deribit's real rule: `min(0.03% × underlying, 12.5% × premium)`,
    /// maker == taker). The accurate figure needs BOTH the underlying price AND the premium — use
    /// [`FeeSchedule::commission_with_underlying`]. The plain [`FeeSchedule::commission`] (the paper
    /// fill site, which carries ONLY the traded premium — no underlying/index price) degrades to the
    /// **premium-cap-bounded** approximation `min(bps × premium, premium_cap_pct × premium)`; since
    /// `bps ≪ premium_cap_pct` the cap never binds there, so this reproduces the previous
    /// `PercentMakerTaker{3,3}` paper number bit-for-bit while the type now carries the correct
    /// underlying+cap semantics. See the module note on the residual paper gap.
    PercentOfUnderlying { bps: f64, premium_cap_pct: f64 },
    /// Prediction-market-shaped (Polymarket-style): `fee = qty × rate × p × (1−p)` where `p` is the
    /// fill price clamped into the `[0,1]` probability domain. `taker_rate`/`maker_rate` are plain
    /// FRACTIONS of the p(1−p)-scaled share count (NOT bps — a probability price has no bps-of-
    /// notional convention). Maker fills subtract a rebate of `maker_rebate_share` × the equivalent
    /// taker fee, so `maker_rate == 0.0` with a positive share yields a NEGATIVE maker commission
    /// (a rebate, per the sign convention above). See the module doc for the registry decision.
    ProbabilityScaled { taker_rate: f64, maker_rate: f64, maker_rebate_share: f64 },
    /// No per-order commission — FX/CFD venues charge via the spread; polymarket CLOB V2 dropped
    /// `feeRateBps`; US-equities cash accounts are commission-free.
    Free,
}

impl FeeSchedule {
    /// The `GetOrderFee` analog: the commission (quote currency) for a fill of `qty` @ `px`, given
    /// whether it was a maker (resting-limit) or taker (crossing) fill. Pure — no clock/env reads.
    #[inline]
    pub fn commission(&self, is_maker: bool, qty: f64, px: f64) -> f64 {
        match self {
            FeeSchedule::PercentMakerTaker { maker_bps, taker_bps } => {
                let bps = if is_maker { *maker_bps } else { *taker_bps };
                qty * px * (bps / 10_000.0)
            }
            FeeSchedule::PerShareWithFloor { per_share, min, max_pct } => {
                let raw = (per_share * qty).max(*min);
                let cap = max_pct * qty * px;
                // The cap is a ceiling only when it is a real (positive) notional bound.
                if cap > 0.0 { raw.min(cap) } else { raw }
            }
            // No underlying available at the plain-commission site (px is the traded PREMIUM), so
            // this is the premium-cap-bounded approximation. `commission_with_underlying` is the
            // accurate path when an underlying/index price is available (see that method + the
            // variant doc). `is_maker` does not apply (Deribit options maker == taker).
            FeeSchedule::PercentOfUnderlying { bps, premium_cap_pct } => {
                let premium_notional = qty * px;
                (premium_notional * (bps / 10_000.0)).min(premium_notional * premium_cap_pct)
            }
            // The p(1−p) probability curve: zero at the certainty bounds, peak at p = 0.5. `px` is
            // clamped into [0,1] — a paper/backtest price a hair outside the domain (slippage
            // adjustment on a 0/1-priced fill) must not manufacture a negative curve. The maker
            // rebate is `maker_rebate_share` of the EQUIVALENT TAKER fee (not of the maker fee),
            // so with `maker_rate == 0` the maker commission is the negative rebate outright.
            FeeSchedule::ProbabilityScaled { taker_rate, maker_rate, maker_rebate_share } => {
                let p = px.clamp(0.0, 1.0);
                let curve = p * (1.0 - p);
                let taker_fee = qty * taker_rate * curve;
                if is_maker {
                    qty * maker_rate * curve - maker_rebate_share * taker_fee
                } else {
                    taker_fee
                }
            }
            FeeSchedule::Free => 0.0,
        }
    }

    /// The ACCURATE Deribit-options fee when both the traded `premium` and the `underlying`/index
    /// price are known: `min(bps × underlying × qty, premium_cap_pct × premium × qty)` — the real
    /// `min(0.03%-of-underlying, 12.5%-of-premium)` rule. The 12.5%-of-premium cap genuinely binds
    /// for cheap deep-OTM options (where `0.125 × premium < 0.0003 × underlying`), protecting the
    /// buyer. For every NON-`PercentOfUnderlying` shape the `underlying` is irrelevant and this
    /// delegates to [`Self::commission`] on the `premium` (side-agnostic taker). Pure — no I/O.
    ///
    /// NOTE the paper fill site (`vike_backtest::paper::PaperExecutionClient`) carries only the
    /// traded premium, so it cannot call this — it uses the bounded [`Self::commission`]
    /// approximation. This method exists for a caller that DOES have the underlying (a future paper
    /// mount wired to an index feed, or a cost-estimate surface); live Deribit fills report the real
    /// commission regardless, so the residual gap is confined to paper/backtest of Deribit options.
    #[inline]
    pub fn commission_with_underlying(&self, qty: f64, premium: f64, underlying: f64) -> f64 {
        match self {
            FeeSchedule::PercentOfUnderlying { bps, premium_cap_pct } => {
                let underlying_term = qty * underlying * (bps / 10_000.0);
                let premium_cap = qty * premium * premium_cap_pct;
                underlying_term.min(premium_cap)
            }
            _ => self.commission(false, qty, premium),
        }
    }

    /// Flat maker/taker fractions (bps ÷ 10_000) for the percent shape — the raw rate inputs the
    /// paper/backtest primitives take. `(0.0, 0.0)` for the non-percent shapes — including
    /// [`FeeSchedule::ProbabilityScaled`], whose effective rate is price-dependent (p(1−p)-scaled)
    /// and has no flat equivalent.
    #[inline]
    pub fn maker_taker_rates(&self) -> (f64, f64) {
        match self {
            FeeSchedule::PercentMakerTaker { maker_bps, taker_bps } => {
                (maker_bps / 10_000.0, taker_bps / 10_000.0)
            }
            // Deribit options maker == taker; report the bps-of-notional fraction for both (the
            // premium cap is not expressible as a flat rate).
            FeeSchedule::PercentOfUnderlying { bps, .. } => (bps / 10_000.0, bps / 10_000.0),
            _ => (0.0, 0.0),
        }
    }

    /// Build a [`FeeSchedule::PercentMakerTaker`] from raw **fractions** (e.g. `0.001` = 10 bps),
    /// the form venue fee endpoints report. Multiplies to bps ONCE (no rate→bps→rate round-trip).
    #[inline]
    pub fn from_fractions(maker: f64, taker: f64) -> Self {
        FeeSchedule::PercentMakerTaker { maker_bps: maker * 10_000.0, taker_bps: taker * 10_000.0 }
    }

    /// Alias of [`Self::from_fractions`] for the Binance `/api/v3/account` `commissionRates`
    /// (already fractions) — named at the call site for clarity.
    #[inline]
    pub fn from_binance_rates(maker: f64, taker: f64) -> Self {
        Self::from_fractions(maker, taker)
    }
}

/// The per-venue static fee registry — real published **tier-0 / no-token-discount** schedules,
/// each cited. Twin of [`crate::venue_caps::caps_for`]; an unknown venue is [`FeeSchedule::Free`]
/// (fail-safe: never invent a fee). These are the DEFAULTS the live account-actual layer
/// (`ReconClient::fetch_fee_rates`) prefers over when a live rate is fetched.
///
/// ## `venue` is a LANE KEY, not always a roster id
///
/// Most venue ids front ONE order API, so the id IS the lane. Two do not: **binance** and **aster**
/// each mount SPOT and USDⓈ-M PERP behind one id, selected at runtime by a trailing `.P` on the
/// symbol (`exec.rs`'s `let (api_symbol, is_perp) = split_symbol(&symbol); if is_perp { run_perp }
/// else { run_spot }`), and the two lanes are priced completely differently. Their perp lanes
/// therefore get LANE SUB-KEYS — `"binance-perp"` / `"aster-perp"` — with the bare id keeping the
/// lane it always meant, exactly the convention `vike_bridge_core::tif::venue_tif` established for
/// `"binance-perp"`. **Callers must not key this table off a bare venue string**: resolve the lane
/// with `vike_catalog::fee_lane(venue, symbol)`, which owns the `.P` split, and pass its result
/// here.
///
/// ## A lane is a CONTRACT CLASS, not a symbol — and aster's perp id covers three of them
///
/// A venue may price one order API by contract class. Aster does: its `/fapi` perp lane charges
/// three different taker rates depending on what the contract SETTLES IN and what it tracks, so
/// `"aster-perp"` grew a sibling, `"aster-perp-usd1"`. That is deliberately still a LANE key and
/// not a per-symbol map — see [`fee_schedule_for`]'s `"aster-perp-usd1"` arm for the live
/// measurement behind it, and the `"aster-perp"` arm for the ONE class that is measured but
/// **not** encoded here (equity/ETF/commodity perps) and why encoding it would be a guess.
///
/// The general rule this establishes for the next venue: **encode a class only when membership is
/// decidable from the `(venue, symbol)` pair alone**, because `vike_catalog::fee_lane` is a pure
/// string function and the paper/backtest mount that reads this table fetches no venue metadata at
/// all. A class that needs the venue's own instrument feed to identify belongs in the live
/// `ReconClient::fetch_fee_rates` lane, not here.
///
/// A lane sub-key is NOT a roster venue: `every_roster_venue_has_a_fee_schedule` classifies
/// [`crate::venues::VENUES`] ids only, same as `venue_tif`'s roster gate. The lane rows are pinned
/// by `lane_rows_are_pinned` below, and vike-catalog's
/// `fee_lane_is_declared_for_every_perp_suffix_venue` /
/// `a_dual_lane_venue_prices_its_two_lanes_apart` are what force a NEW `.P` venue to answer the
/// lane question instead of silently inheriting one lane's fees.
pub fn fee_schedule_for(venue: &str) -> FeeSchedule {
    match venue {
        // Binance SPOT lane VIP0 (Regular User): 0.10% / 0.10%, no BNB discount. The BARE id is the
        // SPOT lane — `exec::run` routes a non-`.P` symbol to `run_spot` (`/api/v3`), and that lane's
        // `ReconClient` reads live `commissionRates` off `/api/v3/account`.
        // https://www.binance.com/en/fee/trading — spot regular user.
        "binance" => FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 },
        // Binance USDⓈ-M FUTURES lane VIP0 (Regular User): 0.0200% maker / 0.0500% taker — the lane
        // a `BTCUSDT.P` symbol routes to (`exec::run` → `run_perp`, `/fapi/v1`).
        // https://www.binance.com/en/fee/futureFee — USDⓈ-M futures, VIP 0.
        //
        // ⚠ The 2/5 is an EXTERNAL business fact (a published venue schedule); NOTHING in this repo
        // verifies it. Its only in-tree corroboration is the `"aster"` row below, which has always
        // called 0.02%/0.05% "Binance-perp-shaped". The live cross-check does not save you either:
        // `vike_mount::resolve_fee_schedule` consults `ReconClient::fetch_fee_rates` only when a
        // recon client was built, and that is gated on `VIKE_RECONCILE=1` (`recon_if_enabled`),
        // which is OFF by default — so this static row is authoritative on essentially every
        // paper/backtest mount.
        //
        // Before this row existed a `BTCUSDT.P` paper/backtest mount was charged the SPOT 10/10
        // above — 5x the maker fee and 2x the taker fee — because every consumer keyed the table off
        // the bare venue string with no lane discrimination.
        "binance-perp" => FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 },
        // Bybit linear-perp VIP0: 0.02% / 0.055%.
        // https://www.bybit.com/en/help-center/article/Trading-Fee-Structure
        "bybit" => FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 },
        // OKX SWAP perp Lv1 (regular): 0.02% / 0.05% (spot Lv1 is 8/10 bps — the wired adapter is
        // SWAP). https://www.okx.com/fees
        "okx" => FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 },
        // Deribit options: 0.03% of the UNDERLYING, maker == taker, capped at 12.5% of the premium
        // (the buyer-protecting cap that binds for cheap deep-OTM options). The paper fill site has
        // no underlying price, so `commission` degrades to the premium-cap-bounded approximation
        // (numerically the old 3-bps-of-premium figure); `commission_with_underlying` is the
        // accurate path for a caller that has the index. https://www.deribit.com/kb/fees
        "deribit" => FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 },
        // Aster SPOT lane (the BARE id — `aster::exec::run` routes a non-`.P` symbol to `run_spot`,
        // `/api/v3`): **0.005% maker / 0.04% taker**, the published flat spot schedule.
        // https://docs.asterdex.com/trading/spot/spot-fee-structure — read 2026-08-05, verbatim
        // "Maker fee: 0.005%" / "Taker fee: 0.04%", each with a worked example that confirms the
        // rate arithmetically (0.1 BTC at 100,000 → 0.5 USDT maker, 4 USDT taker).
        //
        // ✅ **CONFIRMED LIVE, and now WIRED.** Aster exposes a per-symbol
        // `GET /api/v3/commissionRate` on the spot host, and asking it for `BTCUSDT` on the real
        // mainnet account (2026-08-05, read-only signed GET, HTTP 200) returns exactly
        // **maker 0.5 bps / taker 4 bps** — the published numbers above, to the digit. So this row
        // is a MEASUREMENT that happens to agree with the published schedule, not merely an
        // external claim.
        //
        // That endpoint is no longer a hand-run probe: `ASTER_RECON`
        // (`crates/bridges/aster/src/recon_client.rs`) sets the shared `ReconPaths`'
        // `spot_commission_rate` slot to it, so a reconciling mount's `fetch_fee_rates` returns the
        // account's REAL spot rates and `vike_mount::resolve_fee_schedule` prefers them over this
        // row. `crates/bridges/aster/tests/aster_reconcile_smoke.rs`'s `aster_spot_fee_rates_smoke`
        // is the live check, re-runnable on demand.
        //
        // ✅ **AND THE FLAT SHAPE ITSELF IS NOW MEASURED, not assumed.** The endpoint takes a
        // `symbol`, so one flat row was a claim about 61 other pairs that nothing had checked, and
        // the venue's own API doc appeared to contradict it by pricing `APXUSDT` at 2/7 bps in its
        // `commissionRate` RESPONSE EXAMPLE. Both questions were settled by sweeping the live
        // endpoint across 12 pairs spanning every axis the venue lists — the majors, its own token,
        // stablecoin pairs, a long-tail listing, and all THREE quote assets it trades (`USDT`,
        // `USD1`, `FORM`) — on 2026-08-05, read-only signed GETs:
        //
        //     BTCUSDT ETHUSDT SOLUSDT BNBUSDT ASTERUSDT USDCUSDT USD1USDT
        //     BUSD1 ANUSD1 CDLFORM GIGGLEUSDT 4USDT      -> all HTTP 200, all 0.5 / 4 bps
        //
        // **Twelve of twelve identical.** The spot lane genuinely is flat, and this row is right for
        // every pair on it, not just for `BTCUSDT`.
        //
        // The `APXUSDT` contradiction dissolved with it: that symbol answers
        // `-1121 Invalid symbol` — it is not listed on either aster lane, so the doc's 2/7 bps is a
        // stale example (this doc family is copied from Binance; the futures doc's example likewise
        // claims `BTCUSDT` 2/4 bps where the live account reads 0/4). **Do not re-open this from
        // the doc example alone** — it is a phantom, and the measurement above outranks it.
        // `crates/bridges/aster/tests/aster_reconcile_smoke.rs`'s
        // `aster_per_symbol_commission_rate_sweep` is that sweep, re-runnable on demand.
        //
        // ⚠ Two caveats keep it honest anyway:
        //   - The live lane only runs where a `ReconClient` exists — i.e. behind `VIKE_RECONCILE=1`.
        //     On every paper/backtest mount `fetch_fee_rates` is never called and THIS ROW is the
        //     answer, so keeping it correct still matters.
        //   - The rate is TIER-dependent, and this row is the base tier. The account reads
        //     `feeTier: 0` (measured the same day off `/api/v3/account`) — the tier this table
        //     documents itself as carrying — so the sweep above measured tier 0 and says nothing
        //     about a VIP account. Aster's VIP grid is published only as IMAGES, so no tier table
        //     could be transcribed here.
        // The $ASTER fee-payment discount (−5%) is deliberately NOT applied: this table is the
        // no-token-discount tier-0 schedule by construction (see this function's doc).
        //
        // This REPLACES a self-declared "Binance-perp-shaped 0.02%/0.05%" assumption that had no
        // source and matched no Aster schedule at any point in the venue's history.
        "aster" => FeeSchedule::PercentMakerTaker { maker_bps: 0.5, taker_bps: 4.0 },
        // Aster USDⓈ-M PERP lane, CRYPTO contracts — the lane a `BTCUSDT.P` symbol routes to:
        // **0% maker / 0.04% taker** for USDT-Perpetual contracts.
        // https://docs.asterdex.com/trading/perpetuals/fees-and-specs/fees — read 2026-08-05,
        // § "Fee Rates for USDT-Perpetual Contracts". The page's own worked examples confirm both
        // legs: a 0.1 BTC taker buy at 80,000 → "3.20 USDT" (= 8,000 × 0.04%), and the limit-sell
        // example → "0 USDT", so the zero maker rate is real rather than a rounding artifact. The
        // page carries no promotional/expiry qualifier.
        //
        // ⚠⚠ **THIS ROW IS NOT THE WHOLE PERP LANE. Aster prices its perps by CONTRACT CLASS, and
        // this key covers only the crypto one.** The same fee page prices USD1-Perp at 0/0.005% and
        // Stock-Perp at 0/0.009%, and a live sweep of `GET /fapi/v3/commissionRate` across 17
        // symbols on 2026-08-05 (read-only signed GETs, all HTTP 200) found exactly three rates,
        // partitioned by class and never by individual symbol:
        //
        //     0 / 4.0 bps  BTCUSDT ETHUSDT ASTERUSDT 1000PEPEUSDT TAOUSDT BTCU BTCDOMUSDT
        //     0 / 0.9 bps  AAPLUSDT TSLAUSDT NVDAUSDT SPYUSDT QQQUSDT XAUUSDT XAGUSDT
        //     0 / 0.5 bps  BTCUSD1 ETHUSD1 SOLUSD1
        //
        // Only ONE of the two non-crypto classes is encoded (see `"aster-perp-usd1"` below). The
        // **0.9 bps class is measured but deliberately NOT encoded**, and that is the standing gap
        // in this table:
        //
        //   - It covers ~105 of the venue's 523 live perps — everything whose `exchangeInfo`
        //     `underlyingSubType` is `STOCK` (90), `ETF` (6), `Commodities` (9) or a blend. Note the
        //     venue's fee page calls this class "Stock Perpetual" and UNDER-describes it: `XAUUSDT`
        //     and `XAGUSDT` are tagged `Commodities`, not `STOCK`, and are charged 0.9 bps anyway.
        //     So even the published rule would misclassify them — only the venue's own instrument
        //     feed is authoritative.
        //   - **It is not decidable from `(venue, symbol)`.** `AAPLUSDT` and `ASTERUSDT` are the
        //     same string shape; only `exchangeInfo`'s `underlyingSubType` separates them.
        //     `vike_catalog::fee_lane` is a pure string function, and the paper/backtest mount that
        //     reads this table fetches no venue metadata at all (`make_engine`'s instrument
        //     pre-fetch is CREDENTIALED and runs only in a live arm), so there is nowhere honest to
        //     get the class at the moment this row is chosen. Encoding a ~105-symbol membership
        //     list instead would be a dated snapshot of venue state that goes stale on every new
        //     stock listing — the drift-prone shape the capability-map playbook says to DECLARE,
        //     not to act on.
        //   - **The error is CONSERVATIVE**, which is why leaving it is defensible: an equity perp
        //     is charged 4 bps here against a true 0.9, so a backtest over-pays 4.4x and any
        //     strategy that clears this bar clears the real one. It is still wrong for anything
        //     COMPARATIVE — venue selection and taker-vs-maker choices on equity perps are made
        //     against a cost 4.4x too high, which can reject a viable strategy outright.
        //   - The honest fix is the LIVE lane, not a bigger table: wire
        //     `ASTER_RECON.perp_commission_rate` (the endpoint answers, see below) so a reconciling
        //     mount reads the real per-symbol rate. That is a behavior change on a live-money
        //     venue's reconcile path and is deliberately out of scope here.
        //
        // ✅ **CROSS-CHECKED LIVE 2026-08-05 — the 0-bps maker is REAL, not a documentation
        // artifact.** Aster documents `GET /fapi/v3/commissionRate` (weight 20, `symbol` required)
        // in `V3(Recommended)/EN/aster-finance-futures-api-v3.md` § "User Commission Rate
        // (USER_DATA)" (https://github.com/asterdex/api-docs). Asking it for `BTCUSDT` on the real
        // mainnet account (read-only signed GET to `fapi.asterdex.com`, HTTP 200) returns
        // `{symbol, makerCommissionRate: "0", takerCommissionRate: "0.000400"}` — 0 bps / 4 bps,
        // the published pair exactly. `crates/bridges/aster/tests/aster_reconcile_smoke.rs`'s
        // `aster_perp_commission_rate_probe` is that check, re-runnable on demand.
        //
        // ⚠ **What that measurement does NOT establish, and the risk it leaves.** It proves the rate
        // is what this account is charged TODAY. It CANNOT distinguish a permanent schedule from a
        // running promotion — a withdrawable 0-bps maker campaign reads identically to a permanent
        // one on both the fee page (which carries no promotional qualifier, no effective date and no
        // tier grid — re-read 2026-08-05) and this endpoint. Note the venue's OWN API doc example
        // for this endpoint shows `0.0002`/`0.0004` (2/4 bps), contradicting the 0-bps maker; that
        // example is almost certainly copied from Binance, which is why the live account is the
        // tiebreaker — but it is a standing reminder that the perp maker rate is the number in this
        // table most likely to move.
        //
        // The error runs in the DANGEROUS direction: if the maker rate rises off 0, every maker
        // strategy's backtested edge was OVERSTATED — a flattered result, not a conservative one.
        // Rough scale: at 2 bps maker (the Binance-shaped rate the doc example implies), a strategy
        // quoting both sides pays 4 bps of notional per round trip that this row charges 0 for, so
        // ~200 round trips per unit of capital turns a modelled +8% into break-even.
        //
        // ⚠ And nothing refreshes it automatically: `ASTER_RECON.perp_commission_rate` is still
        // `None` (`crates/bridges/aster/src/recon_client.rs`), so `fetch_fee_rates` answers
        // `Ok(None)` on the perp arm even with reconciliation ON — the SPOT lane is wired, this one
        // is not. That asymmetry is the single most important thing to know about these two rows.
        // Re-run the probe before trusting a maker-heavy perp result.
        //
        // The previous 2.0/5.0 here was the same unsourced Binance-perp assumption as the spot row;
        // for the record it matched NO published Aster schedule — not even the legacy AstherusEX
        // one (https://docs.asterdex.com/astherusex-orderbook-perp-guide/fees), which was 0.02%
        // maker but 0.07% taker.
        "aster-perp" => FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 4.0 },
        // Aster USD1-PERPETUAL lane — the perp contracts SETTLED IN USD1 rather than USDT:
        // **0% maker / 0.005% taker**, an 8x cheaper taker leg than the crypto USDT-Perp row above.
        //
        // Sourced TWICE, which is the bar this class had to clear to be encoded at all while the
        // 0.9-bps equity class (see the `"aster-perp"` arm) was not:
        //   1. PUBLISHED — https://docs.asterdex.com/trading/perpetuals/fees-and-specs/fees, read
        //      2026-08-05, § USD1-Perpetual: "Maker fee: 0%" / "Taker fee: 0.005%".
        //   2. MEASURED — a read-only signed `GET /fapi/v3/commissionRate` against the real mainnet
        //      account on 2026-08-05 returned 0 / 0.5 bps for **all three** USD1 contracts the venue
        //      lists (`BTCUSD1`, `ETHUSD1`, `SOLUSD1`, each HTTP 200). Not a sample: that is the
        //      whole class. `crates/bridges/aster/tests/aster_reconcile_smoke.rs`'s
        //      `aster_per_symbol_commission_rate_sweep` is the re-runnable check.
        //
        // ⚠ **Membership is decidable from the SYMBOL, which is the entire reason this row exists.**
        // The settlement asset is IN the name — `vike_catalog::fee_lane` routes a `.P` aster symbol
        // whose exchange form ends in `USD1` here — so this lane needs no venue metadata, no dated
        // symbol list and no network read, and it cannot go stale the way an equity-membership table
        // would: a NEW USD1 contract is classified correctly the day it lists. Contrast `USD1USDT.P`,
        // which is USDT-settled with USD1 as the BASE and correctly stays on `"aster-perp"`.
        //
        // ⚠ Direction of the residual risk, opposite to the equity gap's: this row makes fees
        // CHEAPER than the old flat 4 bps, so an over-broad match would FLATTER a backtest. That is
        // why the match is anchored to the settlement suffix and pinned by
        // `usd1_lane_matches_settlement_not_substring` in vike-catalog rather than a loose contains.
        "aster-perp-usd1" => FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 0.5 },
        // Hyperliquid perp VIP0: 0.015% maker / 0.045% taker.
        // https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees
        "hyperliquid" => FeeSchedule::PercentMakerTaker { maker_bps: 1.5, taker_bps: 4.5 },
        // Interactive Brokers US-equities fixed: $0.005/share, $1.00 min, 0.5% of trade value cap.
        // https://www.interactivebrokers.com/en/pricing/commissions-stocks.php
        "ibkr" | "ibkr_cpapi" => {
            FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_pct: 0.005 }
        }
        // Polymarket's DEFAULT stays `Free` so every existing consumer (paper fill path, snapshot
        // cost display, `resolve_fee_schedule`'s static default) is byte-identical. The verified
        // 2026 V2 fee regime (sports taker 0.05, 15% maker rebate) is the OPT-IN
        // `fee_schedule_for_with_pm_curve` → `POLYMARKET_V2_FEE_CURVE` (see the module doc). This is
        // a documented `KNOWN_FREE` default, not a silent `_` fallback.
        "polymarket" => FeeSchedule::Free,
        // FX/CFD venues charge via the spread, not commission.
        "oanda" | "ig" | "fxcm" | "dukascopy" => FeeSchedule::Free,
        // Alpaca US-equities cash account — commission-free.
        "alpaca" => FeeSchedule::Free,
        // cTrader Open API is a multi-broker gateway: its fee model is BROKER/account-dependent —
        // "standard" accounts charge via the spread (Free-shaped), while raw/ECN accounts charge a
        // per-lot commission the adapter has no way to read from the protocol. Like its VenueCaps
        // row (a KNOWN-STALE conservative stub), this stays the conservative spread-based `Free`
        // rather than inventing a per-lot number. The `KNOWN_FREE` test set below documents this as
        // a DELIBERATE Free (a NAMED arm, not a silent `_`-fallback), honoring the `VENUES` contract
        // that no roster venue rides an unnamed fallback.
        // TODO(ctrader-fees): once the adapter surfaces the account's real commission schedule (or a
        // per-broker default is chosen), replace this with the real per-lot/`PercentMakerTaker` arm.
        "ctrader" => FeeSchedule::Free,
        // vike:new-venue:row // TODO(new-venue: {venue}): read the venue's PUBLISHED schedule and cite the URL, the way every
        // vike:new-venue:row // arm above does. `Free` is the scaffolded value; it is only defensible for a venue that
        // vike:new-venue:row // genuinely charges through the spread, and it must then be listed in `KNOWN_FREE` below.
        // vike:new-venue:row "{venue}" => FeeSchedule::Free,
        // Unknown venue — fail-safe Free (never invent a fee). A ROSTER venue must NOT reach here:
        // `every_roster_venue_has_a_fee_schedule` asserts each roster venue is either an explicit
        // non-Free arm or a documented `KNOWN_FREE` venue, so a new venue cannot silently get Free.
        _ => FeeSchedule::Free,
    }
}

/// The Polymarket p(1−p) curve SHAPE with every rate `0.0` — numerically identical to
/// [`FeeSchedule::Free`] at every price, the default-compatible / no-fee modelling entry. It is the
/// zero-rate baseline a caller uses to model "the curve shape, but no charge", and the template a
/// fee-bearing curve's fields are flipped on from.
///
/// **No longer what the opt-in seam hands out.** Since the 2026 V2 regime (see the module doc)
/// [`fee_schedule_for_with_pm_curve`] returns [`POLYMARKET_V2_FEE_CURVE`]; this constant stays all
/// zeros so every existing consumer of it is byte-identical — notably `vike-backtest`'s zero-rate
/// regression guard (`cheap_np_profile::the_zero_rate_polymarket_curve_still_charges_nothing`) and
/// the `sim_broker`/`cheap_np_run` doc claims that its rates still charge zero.
pub const POLYMARKET_PROB_CURVE: FeeSchedule =
    FeeSchedule::ProbabilityScaled { taker_rate: 0.0, maker_rate: 0.0, maker_rebate_share: 0.0 };

/// The verified **2026 Polymarket V2 fee regime** (V2 launched 2026-04-28) as a
/// [`FeeSchedule::ProbabilityScaled`] — what the opt-in [`fee_schedule_for_with_pm_curve`] hands out
/// for `"polymarket"`. Numbers verified live 2026-07-22:
/// - `taker_rate: 0.05` — the SPORTS-game taker fee (moved `0.03 → 0.05` under V2). A taker fill
///   pays `qty × 0.05 × p(1−p)`.
/// - `maker_rate: 0.0` + `maker_rebate_share: 0.15` — the maker rebate is **15 %** of collected
///   taker fees (down from 25 %), so a maker fill's commission is the NEGATIVE
///   `−0.15 × equivalent-taker-fee` (a rebate, per the sign convention on [`FeeSchedule`]).
///
/// ⚠ **Two facts this per-venue, per-fill constant deliberately CANNOT model** (both are
/// `vike-backtest` follow-ups — a per-venue *pure* schedule cannot express either, so they are
/// reported, not bodged in here):
/// 1. **Per-market rate variation.** Exactly like [`crate::venue_hold`]'s taker hold, the taker fee
///    is a PER-MARKET fact: sports games `0.05`, crypto up/down `0.072` (the `cheap_np` bot's rate),
///    politics `0`. A single per-venue schedule names ONE representative regime (the sports taker);
///    the true per-market number would ride on [`crate::SymbolProperties`] exactly as
///    `taker_hold_ms` does (#659), which is out of this constant's scope. A backtest that needs a
///    different market's rate sets it explicitly (e.g. the harness `[engine.fee]` `taker_rate`).
/// 2. **Per-market rebate POOLING.** V2's 15 % rebate is collected per market and split among that
///    market's makers pro-rata (makers compete only with same-market makers); a stateless per-fill
///    function cannot see the market's total taker fees or a maker's volume share, so
///    `maker_rebate_share` models each maker fill earning 15 % of ITS OWN equivalent taker fee — the
///    per-fill approximation of the per-market pool.
///
/// **Currency / timing (USDC-at-match).** V2 charges the fee in USDC at MATCH time (not in shares at
/// placement). [`FeeSchedule::commission`] returns a bare `f64` with no currency tag; the backtest
/// engine applies it at the FILL event (= match, never at submit) and debits it from cash — so the
/// timing is already match-aligned there, but whether the scalar is the exact USDC magnitude is an
/// engine-side modelling choice (`vike_backtest`), outside what this pure schedule can express.
pub const POLYMARKET_V2_FEE_CURVE: FeeSchedule =
    FeeSchedule::ProbabilityScaled { taker_rate: 0.05, maker_rate: 0.0, maker_rebate_share: 0.15 };

/// The OPT-IN twin of [`fee_schedule_for`] (pm-economics lane): identical for every venue EXCEPT
/// `"polymarket"`, which gets the verified V2 fee regime [`POLYMARKET_V2_FEE_CURVE`] (the p(1−p)
/// curve with real rates) instead of [`FeeSchedule::Free`]. The default registry
/// ([`fee_schedule_for`]) is untouched — existing consumers (the paper fill path, the snapshot cost
/// display, `vike_mount::resolve_fee_schedule`'s static default, all of which read
/// [`fee_schedule_for`], not this) stay byte-identical unless a caller explicitly reaches for this
/// function. This is the ONE seam whose polymarket value changed under V2 (was the all-zero
/// [`POLYMARKET_PROB_CURVE`]).
pub fn fee_schedule_for_with_pm_curve(venue: &str) -> FeeSchedule {
    match venue {
        "polymarket" => POLYMARKET_V2_FEE_CURVE,
        v => fee_schedule_for(v),
    }
}

/// The CROSS-EXCHANGE round-trip fee an xEMM pays for one full cycle: rest passively on the maker
/// venue, get filled, cross the taker venue to hedge. Returns the total as a FRACTION OF PRICE —
/// exactly the `total_fee` argument `vike_mm::xemm::pricing::xemm_maker_quotes` backs its quotes
/// off by, on top of the required edge.
///
/// `maker_a + taker_b` by default: `maker` is the schedule of the venue the quote RESTS on,
/// `taker` of the venue the hedge CROSSES.
///
/// ## `None` is a REFUSAL, and callers must not default it to `0.0`
///
/// [`FeeSchedule::maker_taker_rates`] flattens three shapes to `(0.0, 0.0)` because they have no
/// flat-fraction equivalent at all: [`FeeSchedule::Free`] (the seven spread-charging /
/// commission-free venues), [`FeeSchedule::PerShareWithFloor`] (ibkr — per SHARE, with a floor and
/// a notional cap, so the fraction depends on price and size) and
/// [`FeeSchedule::ProbabilityScaled`] (the polymarket p(1−p) curve, price-dependent by
/// construction). A silent `0.0` there would make the maker quote at the reference touch plus
/// `min_profitability` alone — pricing a fee-bearing round trip as free, and systematically
/// under-charging every quote it rests. So the shape is matched, NOT the value: a genuine 0-bps
/// [`FeeSchedule::PercentMakerTaker`] is accepted and yields `Some(0.0)`.
///
/// [`FeeSchedule::PercentOfUnderlying`] (deribit) is also REFUSED: it reports bps-of-NOTIONAL for
/// both sides while the real charge is `min(bps × underlying, 12.5% × premium)`, so a flat fraction
/// UNDERSTATES it — and understating the fee is the dangerous direction (it narrows the maker
/// spread below break-even). A deribit-legged xEMM needs a fee model this scalar cannot express.
///
/// ## `assume_taker_on_maker_leg` — why it defaults to FALSE
///
/// A resting quote that gets CROSSED INTO is a genuine maker fill; the taker rate applies only to a
/// quote that was MARKETABLE AT SUBMIT. `vike_mm::xemm::pricing::passive_clamp` structurally
/// prevents that (it holds each side strictly inside the maker venue's own touch), which is the
/// substitute for the post-only no roster venue supports. Defaulting to `taker + taker` would
/// roughly double the fee term on a crypto pair and back the maker so far off the reference touch
/// that it never fills — disabling the strategy in the name of caution. A caller that cannot rely
/// on the clamp (no maker-venue book, an aggressive style) passes `true`.
///
/// ⚠ Maker/taker is classified by ORDER KIND, not by crossing aggressiveness, at both fill sites
/// (`vike_paper`'s `is_maker = kind == Limit`, and `vike_backtest`'s engine) — so a marketable
/// limit books as a MAKER fill in paper/backtest and pays TAKER live. Read a rehearsal's PnL with
/// that systematic optimism in mind.
///
/// PURE, naive fold (no `mul_add`), no I/O.
pub fn xemm_round_trip_fee(
    maker: FeeSchedule,
    taker: FeeSchedule,
    assume_taker_on_maker_leg: bool,
) -> Option<f64> {
    let maker_rate = flat_rate(maker, assume_taker_on_maker_leg)?;
    let taker_rate = flat_rate(taker, true)?;
    Some(maker_rate + taker_rate)
}

/// The SINGLE-VENUE round-trip fee a market maker pays for one full cycle: rest a bid on this
/// venue, get filled, rest an ask on the same venue, get filled. **Both legs are MAKER fills**, so
/// the total is `maker + maker`, returned as a FRACTION OF PRICE — the one-venue sibling of
/// [`xemm_round_trip_fee`] (whose second leg CROSSES a different venue and therefore pays taker).
///
/// ## What a consumer does with it: the per-side break-even half-spread is HALF of this
///
/// A two-sided maker resting at `mid ± δ` captures `2δ` on a completed round trip and pays
/// `m·P_buy + m·P_sell ≈ this` fee against `P ≈ mid`. So break-even is `2δ ≥ fee·P`, i.e.
/// **`δ ≥ ½ · fee · P` per side** — one leg's rate times the price, even though the bar being
/// cleared is the whole round trip. That is why this returns the ROUND TRIP and the `½` lives at
/// the call site (`vike_mm::avellaneda::break_even_half_spread`): a half-spread is half a spread,
/// and stating it any other way invites the factor-of-two error this whole seam exists to catch.
///
/// ## What it does NOT cover
///
/// A cycle whose EXIT is a taker (a flatten, a stop, an inventory unwind) pays `maker + taker`,
/// which is strictly larger on every [`fee_schedule_for`] row where the two differ. This number is
/// therefore a LOWER bound on the cost of getting flat, and a maker floored at it is protected
/// against the passive-round-trip loss only. Feed [`xemm_round_trip_fee`]`(s, s, false)` for the
/// maker-in/taker-out bar.
///
/// ## `None` is a REFUSAL, and callers must not default it to `0.0`
///
/// Identical shape rule to [`xemm_round_trip_fee`], and for the identical reason: only
/// [`FeeSchedule::PercentMakerTaker`] has a flat fraction-of-price at all. [`FeeSchedule::Free`]
/// (the FX/CFD venues that charge through the SPREAD, plus the deliberately-conservative ctrader
/// stub whose real schedule is broker-dependent and unreadable from the protocol),
/// [`FeeSchedule::PerShareWithFloor`] and [`FeeSchedule::ProbabilityScaled`] are refused by SHAPE —
/// a `Free` FX venue's maker is not free, it is charged in a currency this scalar cannot name, and
/// answering `0.0` there is exactly the "unknown fee silently became zero" failure. A genuine
/// 0-bps [`FeeSchedule::PercentMakerTaker`] is accepted and yields `Some(0.0)`, because that IS a
/// measured zero. [`FeeSchedule::PercentOfUnderlying`] (deribit) is refused because a flat fraction
/// UNDERSTATES it, and understating is the dangerous direction — it would floor the maker BELOW
/// break-even, which is the bug rather than the fix.
///
/// PURE, naive fold (no `mul_add`), no I/O.
pub fn maker_round_trip_fee(schedule: FeeSchedule) -> Option<f64> {
    let maker = flat_rate(schedule, false)?;
    Some(maker + maker)
}

/// One leg's flat fraction-of-price, or `None` when the SHAPE cannot express one — see
/// [`xemm_round_trip_fee`] for why the refusal is by shape and not by value.
fn flat_rate(schedule: FeeSchedule, taker_side: bool) -> Option<f64> {
    match schedule {
        FeeSchedule::PercentMakerTaker { maker_bps, taker_bps } => {
            Some(if taker_side { taker_bps } else { maker_bps } / 10_000.0)
        }
        FeeSchedule::PercentOfUnderlying { .. }
        | FeeSchedule::PerShareWithFloor { .. }
        | FeeSchedule::ProbabilityScaled { .. }
        | FeeSchedule::Free => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_maker_taker_distinguishes_sides() {
        let s = FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 };
        // maker: 2 bps of 1000 notional = 0.2; taker: 5.5 bps = 0.55
        assert_eq!(s.commission(true, 10.0, 100.0), 10.0 * 100.0 * (2.0 / 10_000.0));
        assert_eq!(s.commission(false, 10.0, 100.0), 10.0 * 100.0 * (5.5 / 10_000.0));
    }

    #[test]
    fn commission_is_the_broker_sim_primitive_bit_for_bit() {
        // Guards the paper byte-path: commission == qty*px*(bps/1e4) in the SAME associativity as
        // `broker_sim::fee(size, px, rate, 1.0)` where rate = bps/1e4.
        let s = FeeSchedule::PercentMakerTaker { maker_bps: 7.0, taker_bps: 7.0 };
        let (q, p) = (0.375, 29550.1234);
        let rate = 7.0 / 10_000.0;
        assert_eq!(s.commission(false, q, p), q * p * rate);
    }

    #[test]
    fn per_share_floor_and_cap() {
        let s = FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_pct: 0.005 };
        // tiny order: 10 shares * $0.005 = $0.05 -> floored to $1.00 min
        assert_eq!(s.commission(false, 10.0, 100.0), 1.0);
        // mid order: 1000 shares * $0.005 = $5.00, cap = 0.5% * 1000 * $50 = $250 -> $5.00
        assert_eq!(s.commission(false, 1000.0, 50.0), 5.0);
        // penny-stock huge order: 100000 * $0.005 = $500 raw, cap = 0.5% * 100000 * $0.20 = $100
        assert_eq!(s.commission(false, 100_000.0, 0.20), 100.0);
        // is_maker is ignored for the per-share shape
        assert_eq!(s.commission(true, 10.0, 100.0), s.commission(false, 10.0, 100.0));
    }

    #[test]
    fn percent_of_underlying_commission_degrades_to_bounded_premium_approx() {
        // The paper site (premium-only): min(bps × premium, cap × premium). bps ≪ cap, so the bps
        // term wins and the number matches the old PercentMakerTaker{3,3} paper figure bit-for-bit.
        let d = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 };
        let (qty, premium) = (2.0, 3000.0);
        assert_eq!(d.commission(false, qty, premium), qty * premium * (3.0 / 10_000.0));
        // identical to the schedule this replaced, at the paper site
        let old = FeeSchedule::PercentMakerTaker { maker_bps: 3.0, taker_bps: 3.0 };
        assert_eq!(d.commission(false, qty, premium), old.commission(false, qty, premium));
        // maker == taker for the options shape
        assert_eq!(d.commission(true, qty, premium), d.commission(false, qty, premium));
    }

    #[test]
    fn percent_of_underlying_accurate_uses_underlying_with_premium_cap() {
        let d = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 };
        // ATM: 0.03% of underlying (18) < 12.5% of premium (375) → underlying term wins.
        assert_eq!(d.commission_with_underlying(1.0, 3000.0, 60_000.0), 1.0 * 60_000.0 * 0.0003);
        // Cheap deep-OTM: 0.03% of underlying (18) > 12.5% of premium (12.5) → the premium CAP binds
        // (the buyer-protecting rule the flat approximation could never express).
        assert_eq!(d.commission_with_underlying(1.0, 100.0, 60_000.0), 1.0 * 100.0 * 0.125);
        // The accurate path exceeds the premium-only approximation whenever underlying > premium
        // (the honest correction the paper site cannot make without an index price).
        assert!(
            d.commission_with_underlying(1.0, 3000.0, 60_000.0) > d.commission(false, 1.0, 3000.0)
        );
    }

    #[test]
    fn commission_with_underlying_delegates_for_non_option_shapes() {
        // Non-PercentOfUnderlying shapes ignore the underlying — delegate to the premium commission.
        let p = FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 };
        assert_eq!(
            p.commission_with_underlying(2.0, 100.0, 9_999.0),
            p.commission(false, 2.0, 100.0)
        );
        assert_eq!(FeeSchedule::Free.commission_with_underlying(2.0, 100.0, 9_999.0), 0.0);
    }

    #[test]
    fn percent_of_underlying_reports_maker_taker_fraction() {
        let d = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 };
        assert_eq!(d.maker_taker_rates(), (3.0 / 10_000.0, 3.0 / 10_000.0));
    }

    #[test]
    fn free_is_zero() {
        assert_eq!(FeeSchedule::Free.commission(true, 10.0, 100.0), 0.0);
        assert_eq!(FeeSchedule::Free.commission(false, 10.0, 100.0), 0.0);
    }

    #[test]
    fn maker_taker_rates_only_for_percent() {
        let s = FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 };
        assert_eq!(s.maker_taker_rates(), (2.0 / 10_000.0, 5.0 / 10_000.0));
        assert_eq!(FeeSchedule::Free.maker_taker_rates(), (0.0, 0.0));
        assert_eq!(
            FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_pct: 0.005 }
                .maker_taker_rates(),
            (0.0, 0.0)
        );
    }

    #[test]
    fn from_fractions_round_trips_to_commission() {
        let s = FeeSchedule::from_fractions(0.001, 0.001); // 10 bps / 10 bps
        assert_eq!(s, FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 });
        assert_eq!(s.commission(false, 2.0, 100.0), 2.0 * 100.0 * (10.0 / 10_000.0));
        // from_binance_rates is the same construction
        assert_eq!(FeeSchedule::from_binance_rates(0.001, 0.001), s);
    }

    #[test]
    fn probability_scaled_taker_curve_peaks_at_half() {
        let s = FeeSchedule::ProbabilityScaled {
            taker_rate: 0.02,
            maker_rate: 0.0,
            maker_rebate_share: 0.0,
        };
        // p = 0.5 is the curve peak: fee = qty * rate * 0.25.
        assert_eq!(s.commission(false, 100.0, 0.5), 100.0 * 0.02 * (0.5 * (1.0 - 0.5)));
        assert_eq!(s.commission(false, 100.0, 0.5), 0.5);
        // symmetric: p and 1−p charge the same fee (analytically; f64 rounding differs by ulps —
        // 0.2·0.8 evaluates as 0.2·(1−0.2) vs 0.8·(1−0.8), which are not the same bit pattern)
        let (lo, hi) = (s.commission(false, 100.0, 0.2), s.commission(false, 100.0, 0.8));
        assert!((lo - hi).abs() < 1e-12, "curve symmetry: {lo} vs {hi}");
        // every off-peak price charges strictly less than the peak
        for p in [0.01, 0.1, 0.3, 0.49, 0.51, 0.9, 0.99] {
            assert!(s.commission(false, 100.0, p) < s.commission(false, 100.0, 0.5), "p={p}");
        }
    }

    #[test]
    fn probability_scaled_fee_vanishes_at_certainty_bounds() {
        let s = FeeSchedule::ProbabilityScaled {
            taker_rate: 0.02,
            maker_rate: 0.01,
            maker_rebate_share: 0.5,
        };
        for is_maker in [false, true] {
            assert_eq!(s.commission(is_maker, 100.0, 0.0), 0.0);
            assert_eq!(s.commission(is_maker, 100.0, 1.0), 0.0);
            // out-of-domain prices clamp into [0,1] — never a negative curve
            assert_eq!(s.commission(is_maker, 100.0, -0.25), 0.0);
            assert_eq!(s.commission(is_maker, 100.0, 1.75), 0.0);
        }
        // p→0 / p→1: the fee tends to zero
        assert!(s.commission(false, 100.0, 1e-9) < 1e-8);
        assert!(s.commission(false, 100.0, 1.0 - 1e-9) < 1e-8);
    }

    #[test]
    fn probability_scaled_maker_rebate_is_negative_commission() {
        // maker_rate 0 + a rebate share: the maker EARNS `share × taker fee` (negative commission,
        // the module's sign convention), while the taker side is unaffected.
        let s = FeeSchedule::ProbabilityScaled {
            taker_rate: 0.02,
            maker_rate: 0.0,
            maker_rebate_share: 0.25,
        };
        let taker = s.commission(false, 100.0, 0.5);
        assert_eq!(taker, 100.0 * 0.02 * (0.5 * (1.0 - 0.5)));
        let maker = s.commission(true, 100.0, 0.5);
        assert!(maker < 0.0, "maker commission must be a rebate, got {maker}");
        assert_eq!(maker, -(0.25 * taker));
        // with a nonzero maker_rate and no rebate the maker pays the (positive) maker curve
        let paying = FeeSchedule::ProbabilityScaled {
            taker_rate: 0.02,
            maker_rate: 0.01,
            maker_rebate_share: 0.0,
        };
        assert_eq!(paying.commission(true, 100.0, 0.5), 100.0 * 0.01 * (0.5 * (1.0 - 0.5)));
        // rebate nets against a nonzero maker curve: maker_fee − share × taker_fee
        let netted = FeeSchedule::ProbabilityScaled {
            taker_rate: 0.02,
            maker_rate: 0.01,
            maker_rebate_share: 0.5,
        };
        let curve = 0.5 * (1.0 - 0.5);
        assert_eq!(
            netted.commission(true, 100.0, 0.5),
            100.0 * 0.01 * curve - 0.5 * (100.0 * 0.02 * curve)
        );
    }

    #[test]
    fn probability_scaled_has_no_flat_rate_and_delegates_underlying() {
        let s = FeeSchedule::ProbabilityScaled {
            taker_rate: 0.02,
            maker_rate: 0.01,
            maker_rebate_share: 0.5,
        };
        // price-dependent — not expressible as a flat fraction
        assert_eq!(s.maker_taker_rates(), (0.0, 0.0));
        // no underlying concept: delegates to the plain (taker) commission
        assert_eq!(
            s.commission_with_underlying(100.0, 0.5, 9_999.0),
            s.commission(false, 100.0, 0.5)
        );
    }

    #[test]
    fn pm_curve_registry_is_opt_in_and_default_compatible() {
        // The DEFAULT registry is untouched: polymarket stays Free (byte-identical consumers —
        // paper fill path, snapshot cost display, `vike_mount::resolve_fee_schedule`'s static
        // default all read this, NOT the opt-in seam).
        assert_eq!(fee_schedule_for("polymarket"), FeeSchedule::Free);
        // The opt-in twin hands out the verified V2 fee curve for polymarket (the VALUE change of
        // this workstream) — NOT the all-zero shape template.
        assert_eq!(fee_schedule_for_with_pm_curve("polymarket"), POLYMARKET_V2_FEE_CURVE);
        assert_ne!(fee_schedule_for_with_pm_curve("polymarket"), FeeSchedule::Free);
        // …and delegates every other venue to the default registry unchanged.
        for v in ["binance", "bybit", "okx", "deribit", "hyperliquid", "ibkr", "oanda", "nope"] {
            assert_eq!(fee_schedule_for_with_pm_curve(v), fee_schedule_for(v), "{v}");
        }
    }

    /// OFF/default-path byte-identity: the all-zero [`POLYMARKET_PROB_CURVE`] shape template MUST
    /// stay numerically identical to [`FeeSchedule::Free`] at every price/side. Three `vike-backtest`
    /// consumers depend on this (the zero-rate regression guard
    /// `cheap_np_profile::the_zero_rate_polymarket_curve_still_charges_nothing`, plus the
    /// `sim_broker`/`cheap_np_run` doc claims that its rates still charge zero); it must NEVER pick
    /// up the V2 rates.
    #[test]
    fn zero_shape_template_stays_byte_identical_to_free() {
        assert_eq!(
            POLYMARKET_PROB_CURVE,
            FeeSchedule::ProbabilityScaled {
                taker_rate: 0.0,
                maker_rate: 0.0,
                maker_rebate_share: 0.0,
            }
        );
        for p in [0.0, 0.1, 0.25, 0.5, 0.9, 1.0] {
            assert_eq!(POLYMARKET_PROB_CURVE.commission(false, 100.0, p), 0.0, "taker p={p}");
            assert_eq!(POLYMARKET_PROB_CURVE.commission(true, 100.0, p), 0.0, "maker p={p}");
            assert_eq!(
                POLYMARKET_PROB_CURVE.commission(false, 100.0, p),
                FeeSchedule::Free.commission(false, 100.0, p),
                "identical to Free at p={p}"
            );
        }
    }

    /// The verified 2026 Polymarket V2 regime resolves through the opt-in curve: sports taker `0.05`,
    /// a `15 %` maker rebate as a NEGATIVE commission, no separate maker fee. Pins the numbers AND
    /// the rebate sign/shape.
    #[test]
    fn polymarket_v2_curve_has_the_verified_regime_numbers() {
        assert_eq!(
            POLYMARKET_V2_FEE_CURVE,
            FeeSchedule::ProbabilityScaled {
                taker_rate: 0.05,
                maker_rate: 0.0,
                maker_rebate_share: 0.15,
            }
        );
        // Taker pays qty × 0.05 × p(1−p) — strictly positive in the curve's interior.
        let taker = POLYMARKET_V2_FEE_CURVE.commission(false, 100.0, 0.25);
        assert_eq!(taker, 100.0 * 0.05 * (0.25 * (1.0 - 0.25)));
        assert!(taker > 0.0, "taker fee must be positive inside the curve support");
        // The maker side is a REBATE: a negative commission of exactly −15 % of the equivalent
        // taker fee at the same fill (the per-market rebate, modelled per-fill).
        let maker = POLYMARKET_V2_FEE_CURVE.commission(true, 100.0, 0.25);
        assert!(maker < 0.0, "maker rebate must be a negative commission, got {maker}");
        assert_eq!(maker, -(0.15 * taker));
        // Fee vanishes at the certainty bounds on both sides — a rebate must not manufacture cost
        // (or income) at p=0 / p=1.
        for is_maker in [false, true] {
            assert_eq!(POLYMARKET_V2_FEE_CURVE.commission(is_maker, 100.0, 0.0), 0.0);
            assert_eq!(POLYMARKET_V2_FEE_CURVE.commission(is_maker, 100.0, 1.0), 0.0);
        }
    }

    /// The LANE sub-key rows, pinned verbatim. These are NOT roster ids (so
    /// `every_roster_venue_has_a_fee_schedule` below ignores them, exactly as `venue_tif`'s roster
    /// gate ignores `"binance-perp"`); they exist because binance's and aster's `exec.rs` route
    /// SPOT vs PERP on the `.P` suffix and the lanes are priced differently.
    ///
    /// BOTH pairs are genuinely two-priced now. Binance: the SPOT row must stay 10/10 and the PERP
    /// row 2/5 — collapsing them, which is what the table did before these rows existed, charges a
    /// `BTCUSDT.P` paper/backtest mount 5x maker and 2x taker. Aster joined them on 2026-08-05,
    /// when both of its rows were finally sourced from the venue's published fee pages and turned
    /// out to differ (0.5 bps maker on spot vs 0 on perp). vike-catalog's
    /// `a_dual_lane_venue_prices_its_two_lanes_apart` is the gate that catches a collapse through
    /// the REAL resolver; this test pins the VALUES the resolver hands back.
    ///
    /// ⚠ **A venue can have more than two lanes.** Aster has THREE: the same 2026-08-05 sweep that
    /// confirmed its two rows also found its ONE perp order API charging three taker rates by
    /// contract class, so `"aster-perp-usd1"` is pinned here alongside them. The pin is not
    /// decorative — that row is 8x cheaper than `"aster-perp"`, so a collapse would flatter every
    /// USD1-settled backtest rather than merely over-charge it.
    #[test]
    fn lane_rows_are_pinned() {
        // binance: the two lanes are genuinely different schedules.
        assert_eq!(
            fee_schedule_for("binance"),
            FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 },
            "binance BARE id is the SPOT lane"
        );
        assert_eq!(
            fee_schedule_for("binance-perp"),
            FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 },
            "binance-perp is the USDⓈ-M futures lane"
        );
        assert_ne!(
            fee_schedule_for("binance"),
            fee_schedule_for("binance-perp"),
            "the whole point of the lane split — a perp mount must not pay spot fees"
        );
        // aster: GENUINELY DUAL-PRICED as of 2026-08-05. Both rows now come from Aster's own
        // published fee pages (cited on the arms), and they DIFFER — the spot lane charges a 0.5
        // bps maker fee while the USDⓈ-M perp lane charges none. Until then both rows carried one
        // unsourced Binance-perp-shaped assumption and this block asserted them EQUAL; the
        // assertion is inverted rather than deleted, and vike-catalog's aster classification moved
        // LANE_NAMED_SAME_PRICE -> LANE_PRICED to match.
        assert_eq!(
            fee_schedule_for("aster"),
            FeeSchedule::PercentMakerTaker { maker_bps: 0.5, taker_bps: 4.0 },
            "aster BARE id is the SPOT lane — docs.asterdex.com/trading/spot/spot-fee-structure"
        );
        assert_eq!(
            fee_schedule_for("aster-perp"),
            FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 4.0 },
            "aster-perp is the USDT-Perpetual lane — \
             docs.asterdex.com/trading/perpetuals/fees-and-specs/fees"
        );
        assert_ne!(
            fee_schedule_for("aster"),
            fee_schedule_for("aster-perp"),
            "aster's two lanes are priced apart (0.5 bps maker on spot, 0 on perp) — collapsing \
             them back onto one row re-introduces the unsourced assumption this replaced"
        );
        // aster's THIRD lane: its perp order API is priced by CONTRACT CLASS, and the USD1-settled
        // contracts take an 8x cheaper taker leg. Published (USD1-Perpetual 0%/0.005%) AND measured
        // on all three live USD1 contracts, 2026-08-05 — see the arm for both citations.
        assert_eq!(
            fee_schedule_for("aster-perp-usd1"),
            FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 0.5 },
            "aster-perp-usd1 is the USD1-Perpetual lane — \
             docs.asterdex.com/trading/perpetuals/fees-and-specs/fees"
        );
        assert_ne!(
            fee_schedule_for("aster-perp-usd1"),
            fee_schedule_for("aster-perp"),
            "the USD1 perp lane collapsed back onto the crypto perp row — a USD1-settled mount is \
             being charged 8x its real taker fee"
        );
        // A lane key belongs to its OWN venue only — no other venue grew one by accident.
        for stray in [
            "bybit-perp",
            "okx-perp",
            "hyperliquid-perp",
            "deribit-perp",
            // The class sub-key convention must not be assumed to generalise: only the lanes
            // actually declared above exist. `aster-perp-stock` in particular is the measured but
            // deliberately-unencoded equity class — a future PR that adds it must add the ARM, not
            // just the key, and this row is what makes forgetting that loud.
            "aster-perp-stock",
            "binance-perp-usd1",
        ] {
            assert_eq!(
                fee_schedule_for(stray),
                FeeSchedule::Free,
                "{stray} is not a declared lane key — it must ride the fail-safe fallback"
            );
        }
    }

    #[test]
    fn registry_has_real_published_defaults() {
        assert_eq!(
            fee_schedule_for("binance"),
            FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 }
        );
        assert_eq!(
            fee_schedule_for("bybit"),
            FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 }
        );
        assert_eq!(
            fee_schedule_for("okx"),
            FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 }
        );
        assert_eq!(
            fee_schedule_for("deribit"),
            FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 }
        );
        assert_eq!(
            fee_schedule_for("hyperliquid"),
            FeeSchedule::PercentMakerTaker { maker_bps: 1.5, taker_bps: 4.5 }
        );
        assert_eq!(
            fee_schedule_for("aster"),
            FeeSchedule::PercentMakerTaker { maker_bps: 0.5, taker_bps: 4.0 }
        );
        assert_eq!(
            fee_schedule_for("ibkr"),
            FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_pct: 0.005 }
        );
        assert_eq!(fee_schedule_for("ibkr_cpapi"), fee_schedule_for("ibkr"));
        for free in ["polymarket", "oanda", "ig", "fxcm", "dukascopy", "alpaca", "nasdaq"] {
            assert_eq!(fee_schedule_for(free), FeeSchedule::Free, "{free} is free/unknown");
        }
    }

    /// Completeness vs the canonical roster (`crate::venues::VENUES`): every roster venue resolves
    /// to a DELIBERATE fee outcome — either an explicit, non-`Free` published schedule arm
    /// (`WITH_SCHEDULE`, pinned verbatim in `registry_has_real_published_defaults` above) or a
    /// DOCUMENTED-`Free` venue (`KNOWN_FREE` — its NAMED arm returns `Free` on purpose). A roster
    /// venue in NEITHER set fails here, so a NEW venue cannot silently inherit `Free` from the `_`
    /// fallback: it must be classified one way or the other. Same shape as tif's
    /// `every_roster_venue_is_classified` (#498).
    #[test]
    fn every_roster_venue_has_a_fee_schedule() {
        // roster venues with an explicit, non-Free published schedule arm
        const WITH_SCHEDULE: &[&str] =
            &["binance", "bybit", "okx", "deribit", "aster", "hyperliquid", "ibkr"];
        // roster venues that DELIBERATELY resolve to Free, each with a NAMED arm documenting why:
        // polymarket (CLOB V2 dropped feeRateBps), oanda/ig/fxcm/dukascopy (FX/CFD spread-based),
        // alpaca (US-equities cash), ctrader (broker-dependent — conservative spread-based, TODO).
        // A venue here is a CLASSIFIED Free, never a silent `_`-fallback Free.
        #[rustfmt::skip]
        const KNOWN_FREE: &[&str] = &[
            "polymarket", "oanda", "ig", "fxcm", "dukascopy", "alpaca", "ctrader",
            // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): move to WITH_SCHEDULE the moment a real arm lands
        ];
        assert_eq!(
            WITH_SCHEDULE.len() + KNOWN_FREE.len(),
            crate::venues::VENUES.len(),
            "every roster venue classified exactly once (explicit schedule or documented Free)"
        );
        for &v in crate::venues::VENUES {
            let scheduled = WITH_SCHEDULE.contains(&v);
            let known_free = KNOWN_FREE.contains(&v);
            assert!(
                scheduled ^ known_free,
                "roster venue {v} must be classified exactly once (explicit schedule arm or \
                 documented KNOWN_FREE)"
            );
            let fee = fee_schedule_for(v);
            if scheduled {
                assert_ne!(
                    fee,
                    FeeSchedule::Free,
                    "{v}: declared a real schedule, must not be Free"
                );
            } else {
                assert_eq!(fee, FeeSchedule::Free, "{v}: KNOWN_FREE venue must resolve to Free");
            }
        }
    }
}

#[cfg(test)]
mod xemm_fee_tests {
    use super::*;

    /// The v1 pair — hyperliquid maker (1.5 bps) resting, okx taker (5.0 bps) hedging. The exact
    /// arithmetic is pinned bit-for-bit because this number IS the maker's break-even offset: a
    /// silent change to either registry row moves every quote the maker rests.
    #[test]
    fn xemm_round_trip_fee_is_maker_plus_taker_by_default() {
        let fee =
            xemm_round_trip_fee(fee_schedule_for("hyperliquid"), fee_schedule_for("okx"), false)
                .expect("both legs are PercentMakerTaker");
        assert_eq!(
            fee.to_bits(),
            (1.5_f64 / 10_000.0 + 5.0 / 10_000.0).to_bits(),
            "hyperliquid maker + okx taker = 6.5 bps, naive fold"
        );
    }

    /// The conservative opt-in charges the MAKER venue's TAKER rate instead — strictly larger, so
    /// it can only widen the maker spread, never narrow it below break-even.
    #[test]
    fn assume_taker_on_maker_leg_charges_the_taker_rate_on_both_legs() {
        let maker = fee_schedule_for("hyperliquid");
        let taker = fee_schedule_for("okx");
        let default = xemm_round_trip_fee(maker, taker, false).expect("some");
        let conservative = xemm_round_trip_fee(maker, taker, true).expect("some");
        assert_eq!(
            conservative.to_bits(),
            (4.5_f64 / 10_000.0 + 5.0 / 10_000.0).to_bits(),
            "hyperliquid TAKER + okx taker = 9.5 bps"
        );
        assert!(conservative > default, "the conservative reading can only widen");
    }

    /// The four shapes with no flat-fraction equivalent are REFUSED, on EITHER leg. A `0.0` here
    /// would price a fee-bearing round trip as free — see the fn doc.
    #[test]
    fn shapes_without_a_flat_rate_are_refused_not_defaulted_to_zero() {
        let ok = FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 };
        let refused = [
            FeeSchedule::Free,           // oanda/ig/alpaca/polymarket/…
            fee_schedule_for("ibkr"),    // PerShareWithFloor
            POLYMARKET_V2_FEE_CURVE,     // ProbabilityScaled
            fee_schedule_for("deribit"), // PercentOfUnderlying
        ];
        for s in refused {
            assert_eq!(
                xemm_round_trip_fee(s, ok, false),
                None,
                "{s:?} on the MAKER leg has no flat fraction — must refuse, not default to 0.0"
            );
            assert_eq!(
                xemm_round_trip_fee(ok, s, false),
                None,
                "{s:?} on the TAKER leg has no flat fraction — must refuse"
            );
        }
    }

    /// The refusal is by SHAPE, not by VALUE: a genuine zero-rate percent schedule is a legitimate
    /// answer of `0.0`, not a missing one. (Matching on the value would make a zero-fee venue
    /// indistinguishable from an unknown one.)
    #[test]
    fn a_genuine_zero_bps_percent_schedule_is_accepted() {
        let zero = FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 0.0 };
        assert_eq!(
            xemm_round_trip_fee(zero, zero, false).map(f64::to_bits),
            Some(0.0_f64.to_bits()),
            "a real 0-bps schedule resolves to 0.0, unlike the refused shapes"
        );
    }

    /// The maker-rate default is only defensible because NO roster venue supports post-only, which
    /// is why the passive clamp (not an order flag) is what keeps the maker leg passive. If a venue
    /// ever gains post-only this test BREAKS, forcing the default to be revisited rather than
    /// silently inherited.
    #[test]
    fn the_maker_rate_default_is_justified_by_the_absence_of_post_only() {
        for &v in crate::venues::VENUES {
            assert!(
                !crate::venue_caps::caps_for(v).supports_post_only,
                "{v} now supports post-only: revisit `assume_taker_on_maker_leg`'s default and \
                 whether xEMM should send a post-only maker leg instead of relying on the clamp"
            );
        }
    }

    // --- maker_round_trip_fee (the SINGLE-VENUE twin) --------------------------------------------

    /// Both legs are MAKER fills on the SAME venue, so the total is `maker + maker` — never
    /// `maker + taker`, which is the xEMM shape and is strictly larger on every row where the two
    /// differ. Pinned on the row the the CI box finding was measured against (bybit 2/5.5 bps).
    #[test]
    fn a_single_venue_round_trip_pays_the_maker_rate_twice() {
        let bybit = fee_schedule_for("bybit");
        assert_eq!(bybit, FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 });
        assert_eq!(
            maker_round_trip_fee(bybit).map(f64::to_bits),
            Some((2.0_f64 / 10_000.0 + 2.0_f64 / 10_000.0).to_bits()),
            "maker + maker = 4 bps round trip on bybit VIP0"
        );
        // ...and it is STRICTLY CHEAPER than the maker-in/taker-out bar, which is what makes it a
        // LOWER bound rather than the whole cost of getting flat (see the fn doc).
        let flatten = xemm_round_trip_fee(bybit, bybit, false).expect("percent shape");
        assert!(
            maker_round_trip_fee(bybit).unwrap() < flatten,
            "a taker exit costs more than a passive round trip; the floor covers only the latter"
        );
    }

    /// THE MEASUREMENT THIS SEAM EXISTS FOR: on bybit BTCUSDT at the mid the finding was taken at,
    /// the per-side break-even half-spread is ~126 ticks — more than DOUBLE
    /// `MakerMountConfig::crypto`'s 60-tick `max_half_spread_ticks` ceiling. The arithmetic lives
    /// here so the claim is checkable, not merely asserted in prose.
    #[test]
    fn bybit_btcusdt_break_even_half_spread_exceeds_the_crypto_mount_cap() {
        let fee = maker_round_trip_fee(fee_schedule_for("bybit")).expect("percent shape");
        let (mid, tick, cap_ticks) = (63_050.0_f64, 0.1_f64, 60.0_f64);
        let break_even_half = 0.5 * fee * mid; // the ½ that lives at the consumer (see the fn doc)
        assert!(
            (break_even_half - 12.61).abs() < 0.01,
            "break-even half-spread ≈ $12.61, got {break_even_half}"
        );
        assert!(
            break_even_half / tick > 2.0 * cap_ticks,
            "126 ticks needed vs a 60-tick ceiling: {} ticks",
            break_even_half / tick
        );
    }

    /// The refusal is by SHAPE, for the identical reason as [`xemm_round_trip_fee`]'s: a `Free` FX
    /// venue's maker is not free (it is charged through the spread), a per-share or p(1−p) fee has
    /// no flat fraction, and deribit's flat fraction UNDERSTATES the real charge. Answering `0.0`
    /// for any of them is the "unknown fee silently became zero" failure this whole seam is against.
    #[test]
    fn shapes_without_a_flat_rate_are_refused_rather_than_floored_at_zero() {
        for s in [
            FeeSchedule::Free,
            FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_pct: 0.005 },
            POLYMARKET_V2_FEE_CURVE,
            FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 },
        ] {
            assert_eq!(maker_round_trip_fee(s), None, "{s:?} must REFUSE, not answer 0.0");
        }
        // ...while a genuine, measured 0-bps percent schedule IS an answer (aster's perp maker row).
        assert_eq!(
            maker_round_trip_fee(fee_schedule_for("aster-perp")).map(f64::to_bits),
            Some(0.0_f64.to_bits()),
            "a measured 0-bps maker is a real zero, not a refusal"
        );
    }
}
