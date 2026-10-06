//! `FeeSchedule` — the per-venue transaction-fee model (integration steal-list #5).
//!
//! Two seams, kept apart on purpose (see `docs/superpowers/specs/2026-07-17-fee-model-design.md`):
//! - **data**: [`FeeSchedule`] describes a venue's fee *shape and rate* (crypto maker/taker bps,
//!   equities per-share+floor, or free), and [`fee_schedule_for`] is the per-venue registry of
//!   real published tier-0 defaults — the twin of [`crate::venues::venue_caps::caps_for`], living here in
//!   `vike-model` (the bottom layer every crate reaches down to) for the same layering reason.
//! - **logic**: [`FeeSchedule::commission`] is the pure fee-application function (LEAN's
//!   `GetOrderFee` analog): `(is_maker, qty, px) -> commission` in quote currency, deterministic
//!   (no clock/env reads), so it is backtest-safe.
//!
//! Deliberately a SEPARATE module from [`crate::venues::venue_caps`] rather than a `VenueCaps` field:
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
//! path (`vike_paper` via `vike_mount::make_engine`, #414/#416; it was `vike_backtest::paper`
//! until the paper exchange left that crate, and this named that path until 2026-09-28) and the
//! snapshot cost display consume that registry directly, so flipping the default would change
//! existing users' numbers. The fee curve is an OPT-IN: [`fee_schedule_for_with_pm_curve`] returns
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
//! `vike_paper::PaperExecutionClient` and the backtest engine's `dispatch_fill` both
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
    /// NOTE the paper fill site (`vike_paper::PaperExecutionClient`) carries only the
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
/// each cited. Twin of [`crate::venues::venue_caps::caps_for`]; an unknown venue is [`FeeSchedule::Free`]
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
/// lane it always meant, exactly the convention [`crate::venues::venue_tif::venue_tif`] established for
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
/// 1. **Per-market rate variation.** Exactly like [`crate::venues::venue_hold`]'s taker hold, the taker fee
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

#[path = "fees_tests.rs"]
#[cfg(test)]
mod fees_tests;

#[path = "xemm_fee_tests.rs"]
#[cfg(test)]
mod xemm_fee_tests;
