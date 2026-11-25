//! `fee_schedule_for` — the per-venue registry of real published tier-0 fee schedules, and the
//! Polymarket V2 fee curve the opt-in seam hands out.
//!
//! The DATA seam of the fee model: [`vike_model::FeeSchedule`] (the shape, `commission`, the
//! round-trip fee math) is the vocabulary and stays in `vike-model`; this module is the per-venue
//! TABLE over it, the twin of [`crate::venue_caps::capabilities_for`] (decision 0162 moved it here from
//! `vike-model/src/money/fees.rs`, text unchanged).
//!
//! Deliberately a SEPARATE module from [`crate::venue_caps`] rather than a `VenueCapabilities` field:
//! `VenueCapabilities` is a stable compile-time capability matrix with exact-equality unit tests, whereas
//! fee numbers are a drift-prone business fact that changes when a venue re-prices — mixing them
//! would churn the capability tests on every rate move.
//!
//! **Registry decision (default-compatible, byte-identical):** [`fee_schedule_for`] KEEPS returning
//! [`FeeSchedule::Free`] for `"polymarket"` — the paper fill path (`vike_paper` via
//! `vike_tradehub_core::make_engine`) and the snapshot cost display consume that registry directly, so
//! flipping the default would change existing users' numbers. The fee curve is an OPT-IN:
//! [`fee_schedule_for_with_pm_curve`] returns [`POLYMARKET_V2_FEE_CURVE`] — the verified **2026
//! Polymarket V2 regime** (V2 launched 2026-04-28; sports taker `0.05`, maker rebate `15 %` of
//! collected taker fees as a NEGATIVE maker commission) — for `"polymarket"`, and delegates to
//! [`fee_schedule_for`] for every other venue. The separate all-zero [`POLYMARKET_PROB_CURVE`] is
//! retained as the numerically-identical-to-`Free` SHAPE template (its zero rates are relied on by
//! the backtest's zero-rate regression guard). See [`POLYMARKET_V2_FEE_CURVE`]'s doc for the
//! per-market rate variation, per-market rebate pooling, and USDC-at-match caveats that a
//! per-venue, per-fill schedule cannot express — each a `vike-backtest` follow-up.

use vike_model::FeeSchedule;

/// The per-venue static fee registry — real published **tier-0 / no-token-discount** schedules,
/// each cited. Twin of [`crate::venue_caps::capabilities_for`]; an unknown venue is [`FeeSchedule::Free`]
/// (fail-safe: never invent a fee). These are the DEFAULTS the live account-actual layer
/// (`ReconcileClient::fetch_fee_rates`) prefers over when a live rate is fetched.
///
/// ## `venue` is a LANE KEY, not always a roster id
///
/// Most venue ids front ONE order API, so the id IS the lane. Two do not: **binance** and **aster**
/// each mount SPOT and USDⓈ-M PERP behind one id, selected at runtime by a trailing `.P` on the
/// symbol (`exec.rs`'s `let (api_symbol, is_perp) = split_symbol(&symbol); if is_perp { run_perp }
/// else { run_spot }`), and the two lanes are priced completely differently. Their perp lanes
/// therefore get LANE SUB-KEYS — `"binance-perp"` / `"aster-perp"` — with the bare id keeping the
/// lane it always meant, exactly the convention [`crate::venue_time_in_force::venue_time_in_force`] established for
/// `"binance-perp"`. **Callers must not key this table off a bare venue string**: resolve the lane
/// with `crate::fee_lane(venue, symbol)`, which owns the `.P` split, and pass its result
/// here.
///
/// ## A lane is a CONTRACT CLASS, not a symbol — and aster's perp id covers three of them
///
/// A venue may price one order API by contract class. Aster does: its `/fapi` perp lane charges
/// three different taker rates depending on what the contract SETTLES IN and what it tracks, so
/// `"aster-perp"` grew two siblings, `"aster-perp-usd1"` and `"aster-perp-stock"`. Those are still
/// LANE keys and not a per-symbol map — see [`fee_schedule_for`]'s arms for the live measurement
/// behind each.
///
/// The general rule this establishes for the next venue: **encode a class only when membership is
/// decidable from the `(venue, symbol)` pair or from the symbol's KEPT row** (the datahub's list:
/// `crate::kept_fee_lane`), because the paper/backtest mount that reads this table fetches no venue
/// metadata at all. The USD1 class is in the name; the stock class only in the row's
/// `underlyingSubType`, so a symbol with no kept row stays on `"aster-perp"`.
///
/// ## More lanes are decided by the symbol's SHAPE, not by a suffix
///
/// **deribit** fronts an options book, futures and spot pairs under one id and prices them apart: a
/// perpetual or a dated future (`BTCUSD.P`, `BTCUSD_261227`) resolves to `"deribit-futures"` (flat 0/5 bps), a
/// spot pair (`BTCUSDC`) to `"deribit-spot"`, and an option keeps the bare options row.
/// **hyperliquid** fronts perps and spot pairs: a `BASE/QUOTE` (or core `BASE-QUOTE`) pair resolves to
/// `"hyperliquid-spot"`, a bare coin keeps the perp row. **okx**: a spot pair's user name (`BTCUSDT`) resolves to
/// `"okx-spot"`, the SWAP keeps the bare row. **ctrader**: the bare row is the FX and metals commission,
/// any other symbol (a CFD) resolves to `"ctrader-cfd"`. **ibkr**: a forex pair row resolves to `"ibkr-fx"`,
/// a future row to `"ibkr-futures"` (by the kept row's class, `crate::kept_fee_lane`), a stock keeps the bare row. All are resolved by
/// `crate::fee_lane`, never here, and pinned by `the_shape_lanes_are_pinned` below and by
/// `crate::fee_lane`'s `a_shape_lane_prices_apart_from_the_bare_row`. A caller that passes the BARE venue
/// (the docs generator's `venues.json`) still gets the bare row, as it always did.
///
/// A lane sub-key is NOT a roster venue: `every_roster_venue_has_a_fee_schedule` classifies
/// [`vike_model::VENUES`] ids only, same as `venue_time_in_force`'s roster gate. The lane rows are pinned
/// by `lane_rows_are_pinned` below, and vike-catalog's
/// `fee_lane_is_declared_for_every_perp_suffix_venue` /
/// `a_dual_lane_venue_prices_its_two_lanes_apart` are what force a NEW `.P` venue to answer the
/// lane question instead of silently inheriting one lane's fees.
pub fn fee_schedule_for(venue: &str) -> FeeSchedule {
    match venue {
        // Binance SPOT lane VIP0 (Regular User): 0.10% / 0.10%, no BNB discount. The BARE id is the
        // SPOT lane — `exec::run` routes a non-`.P` symbol to `run_spot` (`/api/v3`), and that lane's
        // `ReconcileClient` reads live `commissionRates` off `/api/v3/account`.
        // https://www.binance.com/en/fee/trading — spot regular user.
        "binance" => FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 },
        // Binance USDⓈ-M FUTURES lane VIP0 (Regular User): 0.0200% maker / 0.0500% taker — the lane
        // a `BTCUSDT.P` symbol routes to (`exec::run` → `run_perp`, `/fapi/v1`).
        // https://www.binance.com/en/fee/futureFee — USDⓈ-M futures, VIP 0.
        //
        // ⚠ The 2/5 is an EXTERNAL business fact (a published venue schedule); NOTHING in this repo
        // verifies it. Its only in-tree corroboration is the `"aster"` row below, which has always
        // called 0.02%/0.05% "Binance-perp-shaped". Where a `ReconcileClient` exists
        // (`reconcile_if_enabled`: a LIVE mount, reconcile ON by default — decision 0111 removed the
        // `VIKE_RECONCILE` variable this comment used to name), `vike_tradehub_core::resolve_fee_schedule`
        // prefers `ReconcileClient::fetch_fee_rates` over this row; a paper/backtest mount builds no recon
        // client, so this static row is authoritative there. (A LIVE fill books the venue's own fee
        // either way: neither feeds the live fold.)
        //
        // Before this row existed a `BTCUSDT.P` paper/backtest mount was charged the SPOT 10/10
        // above — 5x the maker fee and 2x the taker fee — because every consumer keyed the table off
        // the bare venue string with no lane discrimination.
        //
        // ⚠ The demo account MEASURED 4 bps taker on 206 fills (commission audit 2026-11-08), against
        // this row's 5. The row stays at 5: the published Regular User rate is 0.05 % taker, re-read
        // 2026-11-08 (https://www.binance.com/en/support/faq/360033544231, "a Regular User's taker fee
        // is 0.05%", worked example "10,104 * 0.05% = 5.052 USDT"). A live account that reports its own
        // rate is priced at it (`vike_tradehub_core::resolve_fee_schedule`); a paper book has no account
        // and stays on this row.
        "binance-perp" => FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 },
        // Bybit linear-perp VIP0: 0.02% / 0.055%.
        // https://www.bybit.com/en/help-center/article/Trading-Fee-Structure
        "bybit" => FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 },
        // Bybit SPOT lane (the unmarked pair `PAIR`, `crate::fee_lane`'s `spot_pair_row`) VIP0: 0.10% / 0.10%.
        // ✅ The TAKER side is MEASURED: two demo market buys of DOGEUSDT (2026-10-30) carried `feeRate "0.001"`, and
        // the fee was 0.1% of the quantity, charged in the BASE coin on a buy and in the quote coin on a sell. ⚠ The
        // maker figure is the published VIP0 one (https://www.bybit.com/en/help-center/article/Trading-Fee-Structure),
        // no spot maker fill was taken.
        "bybit-spot" => FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 },
        // OKX SWAP perp Lv1 (regular): 0.02% / 0.05%. https://www.okx.com/fees
        "okx" => FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 },
        // OKX SPOT pairs (the user name `BTCUSDT` of the venue's `BTC-USDT`; `crate::fee_lane`'s `spot_pair_row`), Lv1
        // regular user: **0.08 % maker / 0.10 % taker**. https://www.okx.com/fees (the page renders its
        // table client-side; the figure is the regular tier OKX's fee schedule has carried, re-checked
        // 2026-11-08 against several published copies of it). Before this row a spot pair was priced at
        // the SWAP 2/5 above (commission audit 2026-11-08). UNMEASURED on a fill here.
        "okx-spot" => FeeSchedule::PercentMakerTaker { maker_bps: 8.0, taker_bps: 10.0 },
        // Deribit options: 0.03% of the UNDERLYING, maker == taker, capped at 12.5% of the premium
        // (the buyer-protecting cap that binds for cheap deep-OTM options). The paper fill site has
        // no underlying price, so `commission` degrades to the premium-cap-bounded approximation
        // (numerically the old 3-bps-of-premium figure); `commission_with_underlying` is the
        // accurate path for a caller that has the index. https://www.deribit.com/kb/fees
        "deribit" => FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_percent: 0.125 },
        // Deribit FUTURES: perpetuals (`BTC-PERPETUAL`, `BTC_USDC-PERPETUAL`), dated futures, inverse
        // and USDC (`BTC-27DEC26`, `BTC_USDC-27DEC26`), and future spreads (`BTC-FS-27DEC26_PERP`): a flat
        // **0 bps maker / 5 bps taker** of the contract's notional, NOT the options row above (3 bps of
        // the underlying, capped at 12.5 % of the premium), which these mounts used to be charged
        // (3 bps both ways). The lane is the symbol's shape (`crate::fee_lane`'s
        // `DERIBIT_FUTURES_LANE`), decidable from the string alone.
        //
        // ✅ MEASURED (FEES-AUDIT 2026-10-26 §3, testnet): two ETH-PERPETUAL taker fills charged 0.0000021 ETH
        // each, 5.0 bps of the 10 USD contract; and the commission audit of 2026-11-08 measured the dated
        // futures (inverse and USDC) at 5 bps on real demo fills, where the options row had priced them
        // at 0.55-0.60 of it. ⚠ The MAKER leg is unmeasured (no maker fill on the node), and a rebate
        // (negative) would be a gift to a market-making backtest, so 0 is the conservative end.
        //
        // ⚠ `GOLD_USDC-PERPETUAL` was charged 3.5 bps on the same account (audit 2026-11-08; this row
        // says 5). Deribit's schedule effective 2026-08-01 publishes a Standard tier of 1.5 bps maker /
        // 3.5 bps taker for perpetuals and futures, with no separate gold or commodity row
        // (https://insights.deribit.com/exchange-updates/new-fee-schedule-on-deribit/, whose table is an
        // image; https://support.deribit.com/hc/en-us/articles/25944746248989-Fees answers 403 to a
        // fetch). The same account's BTC and ETH perpetuals and dated futures still charged 5, so which
        // instruments the 3.5 applies to is not readable from the name: left at 5 for all, an
        // over-charge of 1.5 bps on gold.
        //
        // ⚠ The official page, read 2026-11-17 through its help-center API
        // (https://support.deribit.com/api/v2/help_center/en-us/articles/25944746248989.json, the article
        // https://support.deribit.com/hc/en-us/articles/25944746248989-Fees as updated 2026-11-11), lists the
        // Standard tier's "Perpetuals and futures" at 1.5 bps maker / 3.5 bps taker for every perpetual and
        // future, which this row does not follow: the measured demo fills above (5 bps on BTC and ETH) win
        // until a fill says otherwise.
        "deribit-futures" => FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 5.0 },
        // Deribit SPOT pairs (`BTC_USDC`; `crate::fee_lane`'s `DERIBIT_SPOT_LANE`): **0 / 5 bps**,
        // MEASURED 5 bps taker on `BTC_USDC` demo fills (commission audit 2026-11-08), which the options
        // row priced at 3. Published apart from the futures schedule (2026-08-01: at most 2 bps maker /
        // 5 bps taker, waived until Deribit spot joins Coinbase's books, per the insights page above), so
        // it is its own row although the numbers agree today. ⚠ The official page read 2026-11-17 (the
        // help-center article above, updated 2026-11-11) lists the Standard tier's "Spot (routed pairs
        // only)" at 2 bps maker / 5 bps taker; the row is left as it is (ruled 2026-11-17: the measured fills
        // set it), so a spot MAKER fill is charged 2 bps less here than the page says.
        "deribit-spot" => FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 5.0 },
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
        // That endpoint is no longer a hand-run probe: `ASTER_RECONCILE`
        // (`crates/bridges/aster/src/reconcile.rs`) sets the shared `ReconcilePaths`'
        // `spot_commission_rate` slot to it, so a reconciling mount's `fetch_fee_rates` returns the
        // account's REAL spot rates and `vike_tradehub_core::resolve_fee_schedule` prefers them over this
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
        //   - The live lane only runs where a `ReconcileClient` exists (a LIVE mount; decision 0111 removed
        //     the `VIKE_RECONCILE` variable this comment used to name). On every paper/backtest mount
        //     `fetch_fee_rates` is never called and THIS ROW is the answer, so keeping it correct
        //     still matters.
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
        // The 0.9 bps class is `"aster-perp-stock"` below and the USD1 class `"aster-perp-usd1"`.
        // A perp is on THIS row when its kept row states neither, or when no kept row is held:
        // the stock class is not decidable from `(venue, symbol)` (`AAPLUSDT` and `ASTERUSDT` are
        // the same string shape), so a name alone stays here, over-paying an equity perp 4.4x
        // (conservative for an absolute result, wrong for a comparative one).
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
        // ⚠ And nothing refreshes it automatically: `ASTER_RECONCILE.perp_commission_rate` is still
        // `None` (`crates/bridges/aster/src/reconcile.rs`), so `fetch_fee_rates` answers
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
        // The settlement asset is IN the name — `crate::fee_lane` routes a `.P` aster symbol
        // whose exchange form ends in `USD1` here — so this lane needs no venue metadata, no dated
        // symbol list and no network read, and it cannot go stale the way an equity-membership table
        // would: a NEW USD1 contract is classified correctly the day it lists. Contrast `USD1USDT.P`,
        // which is USDT-settled with USD1 as the BASE and correctly stays on `"aster-perp"`.
        //
        // ⚠ Direction of the residual risk, opposite to the equity gap's: this row makes fees
        // CHEAPER than the old flat 4 bps, so an over-broad match would FLATTER a backtest. That is
        // why the match is anchored to the settlement suffix and pinned by
        // `usd1_lane_matches_settlement_not_substring` (`crate::fee_lane`'s tests) rather than a loose contains.
        "aster-perp-usd1" => FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 0.5 },
        // Aster STOCK-PERPETUAL lane (owner, 2026-11-16, flag ASTER-2): **0% maker / 0.009% taker**.
        // Published (https://docs.asterdex.com/trading/perpetuals/fees-and-specs/fees, read 2026-08-05,
        // "Stock-Perp 0/0.009%") and MEASURED: `GET /fapi/v3/commissionRate` on the real mainnet account,
        // 2026-08-05, answered 0 / 0.9 bps for AAPLUSDT TSLAUSDT NVDAUSDT SPYUSDT QQQUSDT XAUUSDT XAGUSDT
        // (`crates/bridges/aster/tests/aster_reconcile_smoke.rs`'s `PERP_PROBE_SYMBOLS`). The class is
        // the venue's `underlyingSubType` (`STOCK`, `ETF`, `Commodities` or a blend: the fee page's
        // "Stock Perpetual" under-describes it, `XAUUSDT` is tagged `Commodities` and charged 0.9 too),
        // which only the symbol's KEPT row carries: `crate::kept_fee_lane` routes a row here, a name
        // never. ⚠ A match too broad would FLATTER a backtest (4.4x cheaper than `"aster-perp"`).
        "aster-perp-stock" => FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 0.9 },
        // Hyperliquid perp VIP0: 0.015% maker / 0.045% taker.
        // https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees
        "hyperliquid" => FeeSchedule::PercentMakerTaker { maker_bps: 1.5, taker_bps: 4.5 },
        // Hyperliquid SPOT pairs (the unmarked `HYPEUSDC`; `crate::fee_lane`'s
        // `spot_pair_row`): **0.040 % maker / 0.070 % taker** at the base tier, against the
        // perp row above. https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees (published
        // base tier, from memory, not re-read here). ⚠ UNMEASURED: nothing is traded in spot on any
        // account today, so no spot fill has ever been read; the first one (a demo/testnet spot fill)
        // settles it. A spot BUY also charges its fee in the BASE token, which the bridge books at face
        // value (FEES-AUDIT §4.5) — this row is the paper/backtest schedule only and cannot fix that.
        // HIP-3 builder-dex perps (`xyz:TSLA`) are priced above the standard perp and are NOT encoded:
        // the surcharge depends on the deployer's growth mode, which only the venue's metadata states.
        "hyperliquid-spot" => FeeSchedule::PercentMakerTaker { maker_bps: 4.0, taker_bps: 7.0 },
        // Interactive Brokers US-equities fixed (IBKR Pro Fixed): $0.005/share, $1.00 min per order, and a
        // CAP of 1 % of the trade value (the vendor's published figure, from memory, not re-read here).
        // The cap was 0.5 % here until 2026-10-27 with no source (FEES-AUDIT §4.11). It binds only on a
        // very low-priced stock (below $0.50 a share), so no node fill (6 AAPL orders, the $1.00 minimum
        // plus 0.003-0.007 of regulatory fees each) could measure it either way.
        // https://www.interactivebrokers.com/en/pricing/commissions-stocks.php
        //
        // The $1.00 minimum is per ORDER: an order filled in pieces pays it once, the fill site carrying
        // what the order was charged so far (`vike_fills::broker_sim::commission`).
        "ibkr" | "ibkr_cpapi" => {
            FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_percent: 0.01 }
        }
        // IBKR SPOT CURRENCIES (a forex pair row, `EURUSD`; `crate::fee_lane`'s
        // `IBKR_FX_LANE`): **0.20 bps of the trade value, minimum USD 2.00 per order**, the first volume
        // tier (monthly trade value up to USD 1,000,000,000) of IBKR's one spot-currency schedule.
        // https://www.interactivebrokers.com/en/pricing/commissions-spot-currencies.php (answers 403 to a
        // fetch; the figure is the one the venue's regional copies of that page carry, read 2026-11-08).
        // Before this row a pair was charged the stock row above: $0.005 a currency UNIT, $125 on a
        // 25,000 EUR order (commission audit 2026-11-08). ⚠ The minimum is dollars, and the fee is booked
        // in the pair's QUOTE money, so on a pair not quoted in dollars (USD.JPY) the 2.00 is read as
        // 2.00 of the quote money: right on every `*.USD` pair, an under-charge on the rest (a backtest
        // reads no venue table since the owner's ruling of 2026-11-18; the paper book does).
        "ibkr-fx" => FeeSchedule::BpsWithFloor { bps: 0.2, min: 2.0 },
        // IBKR FUTURES (a future row, `ES_261218`; `IBKR_FX_LANE`'s sibling `IBKR_FUTURES_LANE`):
        // **USD 0.85 per contract**, IBKR Pro Fixed for US futures (ES).
        // https://www.interactivebrokers.com/en/pricing/commissions-futures.php (answers 403 to a fetch,
        // read 2026-11-08 through published copies, which disagree on whether exchange and regulatory
        // fees ride on top; they are not encoded). A micro (MES, MNQ: listed at 0.25) is over-charged
        // at this rate. Per contract and never scaled by the multiplier
        // (`vike_model::FeeSchedule::commission_on`). Before this row a future was charged the stock row.
        "ibkr-futures" => {
            FeeSchedule::PerShareWithFloor { per_share: 0.85, min: 0.0, max_percent: 0.0 }
        }
        // Polymarket's DEFAULT stays `Free` so every existing consumer (paper fill path, snapshot
        // cost display, `resolve_fee_schedule`'s static default) is byte-identical. The verified
        // 2026 V2 fee regime (sports taker 0.05, 15% maker rebate) is the OPT-IN
        // `fee_schedule_for_with_pm_curve` → `POLYMARKET_V2_FEE_CURVE` (see the module doc). This is
        // a documented `KNOWN_FREE` default, not a silent `_` fallback.
        "polymarket" => FeeSchedule::Free,
        // FX/CFD venues charge via the spread, not commission.
        "oanda" | "ig" | "fxcm" => FeeSchedule::Free,
        // Dukascopy charges a commission on the USD value of every fill, per side: **$30 per million**
        // = 0.3 bps. MEASURED (commission audit 2026-11-08, demo): USDJPY 1,000 units (1,000 USD of
        // value) charged 0.03 USD. A rate on the notional is unit-free, so 0.3 bps of the QUOTE notional
        // converted to dollars is 0.3 bps of the dollar value, and no new shape is needed. It was `Free`
        // here, read as a spread-only venue.
        "dukascopy" => FeeSchedule::PercentMakerTaker { maker_bps: 0.3, taker_bps: 0.3 },
        // Alpaca US-equities cash account — commission-free.
        "alpaca" => FeeSchedule::Free,
        // cTrader Open API is a multi-broker gateway: its fee model is BROKER/account-dependent —
        // "standard" accounts charge via the spread, raw/ECN accounts charge a commission. The protocol
        // DOES carry it: each `ProtoOASymbol` states the account's commission for that symbol
        // (`commissionType`, `preciseTradingCommissionRate`, `preciseMinCommission`) and each
        // `ProtoOADeal` the commission it charged, so a live fill books the venue's own figure and a
        // per-symbol read of the rate is bridge work. This row is the STATIC fallback the
        // paper/backtest/display path uses when nothing was read.
        //
        // ✅ MEASURED (commission audit 2026-11-08, the hedged demo account, USD deposit): the broker
        // charges **$30 per million USD of value per side = 0.3 bps** on currency pairs and metals
        // (EURUSD 0.01 lot = 1,000 units charged +0.03 USD on each of 94 fills in FEES-AUDIT 2026-10-26
        // §3), and nothing on its CFDs (indices, energy, crypto), which `crate::fee_lane` sends to
        // `"ctrader-cfd"` below. The per-unit row it replaces (0.00003 USD a base unit) fitted 3 of the
        // 7 measured symbols: a unit of a metal or an index is not a hundred-thousandth of a lot. A rate
        // on the notional is unit-free, so the backtest no longer refuses it on a non-dollar ledger.
        // ⚠ It is the measured account's schedule, not cTrader's: a `standard` (spread-only) account
        // pays nothing.
        "ctrader" => FeeSchedule::PercentMakerTaker { maker_bps: 0.3, taker_bps: 0.3 },
        // cTrader CFDs (an index, energy, share or crypto CFD, `US500`, `XTIUSD`, `BTCUSD`): charged
        // through the spread alone on the measured account (commission audit 2026-11-08).
        "ctrader-cfd" => FeeSchedule::Free,
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
///    the true per-market number would ride on [`vike_model::SymbolProperties`] exactly as
///    `taker_hold_ms` does (#659), which is out of this constant's scope. A backtest that needs a
///    different market's rate sets it explicitly (the strategy's `commission` param in the
///    `probability_scaled` unit).
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
/// display, `vike_tradehub_core::resolve_fee_schedule`'s static default, all of which read
/// [`fee_schedule_for`], not this) stay byte-identical unless a caller explicitly reaches for this
/// function. This is the ONE seam whose polymarket value changed under V2 (was the all-zero
/// [`POLYMARKET_PROB_CURVE`]).
pub fn fee_schedule_for_with_pm_curve(venue: &str) -> FeeSchedule {
    match venue {
        "polymarket" => POLYMARKET_V2_FEE_CURVE,
        v => fee_schedule_for(v),
    }
}

/// The ISO weekday index (`vike_model::time::utc_weekday`, Monday = 0) of a Friday.
const FRIDAY: u32 = 4;

/// **The DELIVERY fee `venue` charges an option position at its expiry settlement**, for an option
/// expiring at `expiry_ms` (epoch ms UTC): a [`FeeSchedule::PercentOfUnderlying`] the settlement is
/// charged through [`FeeSchedule::commission_with_underlying`] (the settled quantity, the option's
/// value at expiry as the premium, the underlying's price), or `None` where the venue charges none,
/// or this table states no rule for it (fail-safe: never invent a fee).
///
/// Its own table, not a [`fee_schedule_for`] row: that answer is the TRADING schedule and a wire
/// value (the gatewayhub lease carries a `FeeSchedule` as it is), and a delivery fee is neither. Its
/// one consumer is a backtest's option-expiry settlement
/// (`vike_simulator::StrategyEngineConfig::delivery_fees`, filled by `vike-backtest`'s harness).
///
/// - **deribit**: 0.015 % of the UNDERLYING, never more than 12.5 % of the option's value, on its
///   coin-settled (BTC/ETH) and USDC linear options alike; a DAILY option, one that does not expire
///   on a Friday, pays none. Read 2026-11-17 from the venue's fee page,
///   <https://support.deribit.com/hc/en-us/articles/25944746248989-Fees> (updated there 2026-11-11),
///   "Delivery fees (settlement at expiry)": "BTC/ETH Options: 0.015% - this fee can never be higher
///   than 12.5% of the option's value", "USDC Linear Options: 0.015% - this fee can never be higher
///   than 12.5% of the option's value", "BTC/ETH Daily Options: 0%", "The daily options and weekly
///   futures are exempt from delivery fees"; "Daily expiry is any option that doesn't expire on a
///   Friday"; and its fees "are calculated as a percentage of the underlying asset of the contract".
///   Tier 0, as the trading registry: the top VIP tier's waiver of option settlement fees is not here.
///   An out-of-the-money option is worth nothing at expiry, so its cap charges it nothing.
pub fn option_delivery_fee_for(venue: &str, expiry_ms: i64) -> Option<FeeSchedule> {
    match venue {
        "deribit" if vike_model::time::utc_weekday(expiry_ms) == FRIDAY => {
            Some(FeeSchedule::PercentOfUnderlying { bps: 1.5, premium_cap_percent: 0.125 })
        }
        _ => None,
    }
}

#[path = "fees_tests.rs"]
#[cfg(test)]
mod fees_tests;

#[path = "xemm_fee_tests.rs"]
#[cfg(test)]
mod xemm_fee_tests;
