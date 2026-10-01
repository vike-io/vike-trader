//! Portfolio — the named live cross-venue aggregator (Nautilus-style naming).
//!
//! Groups the portfolio slice of [`crate::snapshot::CoreSnapshot`] — the equity/realized/fees/
//! funding totals plus the per-venue `venues: Vec<VenueBlock>` rows — under one named type, the way
//! Nautilus Trader exposes a single live `Portfolio` folding per-venue `Account`s (and LEAN a
//! single `SecurityPortfolioManager`). This is purely a naming reshape of fields that were loose
//! scalars on `CoreSnapshot`; no value changes. `VenueBlock` (in [`crate::snapshot`]) stays the
//! per-venue element below it. See the design spec:
//! `docs/superpowers/specs/2026-07-17-portfolio-read-model.md` (dedup finding F24).

use crate::snapshot::VenueBlock;
use vike_exec::BalanceMode;

/// The live cross-venue portfolio view the core publishes inside each `CoreSnapshot`. Every field
/// is moved verbatim from `CoreSnapshot` (same types, same doc contracts, same computed values) —
/// this is a behavior-preserving grouping, not new state.
#[derive(Debug, Clone, Default)]
pub struct Portfolio {
    /// mode-aware total equity at the configured seed (resolver-priced via `resolve_equity`)
    pub equity: f64,
    /// cross-venue: per-venue equity summed in venue registration order (py_sum law,
    /// CrossVenueDriver::aggregate_equity semantics). Equals `equity` single-venue.
    ///
    /// ⚠ **HETEROGENEOUS: this sum spans two incommensurate quantities and a REPORT must not
    /// print it as one figure.** A `VenueBlock`'s `equity` answers a different question depending
    /// on its `balance_mode` (see [`Portfolio::equity_book_total`] /
    /// [`Portfolio::equity_wallet_total`], which partition exactly this sum): a `Delta` block is
    /// the daemon's OWN book-keeping from a configured seed, an `Authoritative` block is the
    /// VENUE's attested wallet for the whole account. Measured on the CI box 2026-08-17: nine paper
    /// mounts at 1000 seed each plus one bybit mount that had adopted the shared UNIFIED
    /// account's 53647 USDT `walletBalance` summed to a headline `equity` of 62647.10600813 —
    /// a number that is neither the account's cash nor the daemon's book. The value is UNCHANGED
    /// (the `aggregate_equity` law and every consumer of this field are exactly as before); what
    /// changed is that the honest split is now available beside it.
    pub equity_total: f64,
    pub realized_pnl: f64,
    pub fees_paid: f64,
    pub funding_paid: f64,
    /// cross-venue sum of every venue's `margin_used` (GUI equity strip; 0.0 when the gate is off)
    pub margin_used_total: f64,
    /// **The operator's CONFIGURED capital**: `Σ` each engine's `seed_cash`, py_sum in venue
    /// registration order (primary first) — the frozen base of [`Self::drawdown_curve`].
    ///
    /// This is a CONFIG constant, not an observation: no venue frame moves it, so it is identical
    /// across a restart, and a third party moving money in a shared account cannot touch it. It is
    /// carried on the portfolio rather than per-`VenueBlock` because the wire snapshot
    /// (`vike_tradehub_client::wire::WireVenueBlock`) has no seed field to map from; a
    /// portfolio built off the wire (`vike_app_core::backend::observe_bridge::build_portfolio`) therefore
    /// reports `0.0` here, which is honest — the observe path has no seeds and drives no alerting.
    ///
    /// ⚠ `0.0` means UNKNOWN or unconfigured, never "no capital": [`Self::drawdown_curve`] is
    /// unusable as a fraction denominator when it is not positive, and the core says so out loud
    /// (see `CoreThread::sweep_drawdown_latch`'s DISARMED note).
    pub capital_base: f64,
    /// sum of every venue's `missing_prices` (GUI badge — resolver-unpriceable open positions)
    pub missing_prices_total: u32,
    /// ADDITIVE per-asset ledger view (accounting-upgrade Phase A): the raw (asset, qty)
    /// pairs venue AccountState frames carried, upsert order. `balance` stays the oracle's
    /// collapsed scalar; this block is what the collapse loses.
    pub balances_by_asset: Vec<(String, f64)>,
    /// one block per engine (primary first) — empty only before the first build
    pub venues: Vec<VenueBlock>,
}

/// Nautilus-style query surface over the per-venue `venues` rows (the design spec's optional
/// step 4). These are pure read helpers over the already-built `Vec<VenueBlock>` — GUI/strategy
/// convenience, never on the core fold path — mirroring the accessor names the retired
/// `vike_exec::Portfolio` exposed (`net_position`/`is_net_long`/…) plus Nautilus's per-venue
/// `account(venue)`/`unrealized_pnls(venue)` shape. An unknown venue reads as absent (0.0 / `None`);
/// an unknown symbol nets to 0.0 (flat), never a panic.
impl Portfolio {
    /// The per-venue block for `venue` (Nautilus `account(venue)`), or `None` if this portfolio
    /// carries no row for it.
    pub fn venue(&self, venue: &str) -> Option<&VenueBlock> {
        self.venues.iter().find(|v| v.venue == venue)
    }

    /// Mode-aware equity at `venue` (resolver-priced via `resolve_equity`), 0.0 if the venue is absent.
    pub fn equity(&self, venue: &str) -> f64 {
        self.venue(venue).map_or(0.0, |v| v.equity)
    }

    /// Resolver-priced unrealized PnL total at `venue`, 0.0 if absent.
    pub fn unrealized_pnl(&self, venue: &str) -> f64 {
        self.venue(venue).map_or(0.0, |v| v.unrealized)
    }

    /// Realized PnL at `venue`, 0.0 if absent.
    pub fn realized_pnl(&self, venue: &str) -> f64 {
        self.venue(venue).map_or(0.0, |v| v.realized_pnl)
    }

    /// Margin currently locked by open positions at `venue` (0.0 when the gate is off or absent).
    pub fn margin_used(&self, venue: &str) -> f64 {
        self.venue(venue).map_or(0.0, |v| v.margin_used)
    }

    /// Cross-venue realized PnL: every venue block's `realized_pnl`, summed in venue registration
    /// order — the `equity_total` twin for realized. The scalar [`Self::realized_pnl`] field beside
    /// it is the PRIMARY engine's number and nothing else (`CoreSnapshot::build` binds it from
    /// `engine.account`), so on a multi-venue core it answers about whichever venue happens to be
    /// registered first. Any REPORT that also carries `equity_total` must fold this instead, or its
    /// numbers describe two different accounts — measured on the CI box, where `vike-tradehub`'s summary
    /// line read `realized_pnl: 0.0` from the untraded binance primary while `equity_total` tracked
    /// the bybit mount's ten maker fills to eight decimal places.
    ///
    /// `py_sum` (not a plain fold) deliberately: this sums the SAME vec in the SAME order as
    /// `equity_total`, and a sibling total reported alongside it should not disagree in the last
    /// ULP over the choice of fold.
    pub fn realized_pnl_total(&self) -> f64 {
        vike_model::py_sum(self.venues.iter().map(|v| v.realized_pnl))
    }

    /// Cross-venue fees paid — the [`Self::realized_pnl_total`] twin for `fees_paid`, with the same
    /// primary-only footgun on the scalar `fees_paid` field and the same `py_sum` law.
    pub fn fees_paid_total(&self) -> f64 {
        vike_model::py_sum(self.venues.iter().map(|v| v.fees_paid))
    }

    /// Cross-venue funding paid — the [`Self::realized_pnl_total`] twin for `funding_paid`, same
    /// primary-only footgun on the scalar field, same `py_sum` law.
    pub fn funding_paid_total(&self) -> f64 {
        vike_model::py_sum(self.venues.iter().map(|v| v.funding_paid))
    }

    /// How many position ROWS this portfolio carries across every venue — the cross-venue count of
    /// what `CoreSnapshot::positions` counts for the primary venue alone. ⚠ Rows, not OPEN
    /// positions: a closed position leaves a zero-size entry behind forever (see
    /// `CoreThread::any_position_open`), exactly as the primary-only count always did.
    pub fn position_count(&self) -> usize {
        self.venues.iter().map(|v| v.positions.len()).sum()
    }

    /// **The book-kept half of [`Self::equity_total`]**: every venue block still on
    /// `BalanceMode::Delta`, summed in venue registration order.
    ///
    /// A `Delta` block's equity is `seed + own cash flow + realized + unrealized`
    /// (`ExecutionEngine::mode_equity`) — money this daemon put there and money this daemon's own
    /// fills moved. It is an assertion about THIS MOUNT'S trading and nothing else, which is why
    /// it can be summed across venues: each mount's seed is a separate, daemon-chosen number.
    ///
    /// The complement is [`Self::equity_wallet_total`], and the two partition `equity_total`
    /// exactly (`balance_mode` is a two-variant enum, every block lands in one side). ⚠ They do
    /// NOT sum back to `equity_total` bit-for-bit in general — each is its own `py_sum` over a
    /// SUBSEQUENCE, and Neumaier compensation is not associative across a partition — so compare
    /// them to `equity_total` only within tolerance, never with `to_bits()`.
    pub fn equity_book_total(&self) -> f64 {
        vike_model::py_sum(
            self.venues.iter().filter(|v| v.balance_mode == BalanceMode::Delta).map(|v| v.equity),
        )
    }

    /// **The venue-attested half of [`Self::equity_total`]**: every venue block that has flipped to
    /// `BalanceMode::Authoritative`, summed in venue registration order.
    ///
    /// An `Authoritative` block's equity is `venue wallet + unrealized` — the venue's own number
    /// for the WHOLE ACCOUNT the credentials open, adopted verbatim by
    /// `CoreThread::reconcile_reports` (or by a live `AccountState` push through
    /// `Account::apply_account_state`). That makes it an OBSERVATION ABOUT AN ACCOUNT, not an
    /// accounting of a mount.
    ///
    /// ⚠ **The daemon cannot tell whether that account is dedicated to it.** Each venue's
    /// `ReconClient` fetches positions and fills scoped to the mount's own symbol
    /// (`crates/bridges/bybit/src/recon_client.rs`'s `fetch_position_status_reports` /
    /// `fetch_fill_reports` both pass `symbol`) and cash scoped to the whole account
    /// (`fetch_balance` reads the UNIFIED account's USDT `walletBalance`, with no symbol at all —
    /// there is no venue field that says how much of it is this mount's). On a DEDICATED account
    /// adopting it is exactly right and is why the code does it; on a SHARED one — the CI box's bybit
    /// demo account, which carried settlements on AUCTIONUSDT/ONDOUSDT/ETHUSDT/WLDUSDT the daemon
    /// never traded — the wallet includes money no mount here owns, and nothing distinguishes the
    /// two cases. So this is reported BESIDE the book rather than folded into it, and the adoption
    /// is deliberately left alone.
    ///
    /// ⚠ Summing wallets across venues assumes each block's account is a DIFFERENT account. Two
    /// blocks backed by the same underlying account (two mounts under one venue's sub-account
    /// arrangement) would double-count it. [`Self::wallet_venues`] names the contributors so a
    /// report can say whose wallets it added.
    pub fn equity_wallet_total(&self) -> f64 {
        vike_model::py_sum(
            self.venues
                .iter()
                .filter(|v| v.balance_mode == BalanceMode::Authoritative)
                .map(|v| v.equity),
        )
    }

    /// The venues contributing to [`Self::equity_wallet_total`], in venue registration order —
    /// so a report that quotes a venue wallet can NAME whose wallet it is quoting. Empty on a
    /// pure-paper core (nothing has ever attested a balance).
    pub fn wallet_venues(&self) -> Vec<&str> {
        self.venues
            .iter()
            .filter(|v| v.balance_mode == BalanceMode::Authoritative)
            .map(|v| v.venue.as_str())
            .collect()
    }

    /// **The daemon's OWN profit and loss across every venue** — the published twin of
    /// [`vike_exec::ExecutionEngine::resolved_own_pnl`], folded from the four `VenueBlock` fields
    /// that engine-side scalar is built from: `realized_pnl − fees_paid + funding_paid +
    /// unrealized`, py_sum in venue registration order.
    ///
    /// ⚠ Term order and fold law are pinned to match the engine-side scalar exactly, so the number
    /// a `RuleTrigger::Drawdown` alert reads out of a published snapshot is bit-identical to the
    /// one `CoreThread::sweep_drawdown_latch` acted on — not merely close.
    /// `crates/vike-core/tests/drawdown_measures_own_pnl.rs`'s
    /// `the_latch_curve_and_the_published_curve_are_bit_identical` is the gate.
    ///
    /// Unlike [`Self::equity_total`] this carries NO balance level, so it is homogeneous across
    /// `Delta` and `Authoritative` blocks and a venue-wallet movement with no trading behind it
    /// does not move it. See `resolved_own_pnl` for the argument and its one declared residual.
    ///
    /// ⚠ **Not [`Self::equity_book_total`], and the difference decides whether a safety mechanism
    /// works.** That method is the right answer to "how much of the headline total is book-kept
    /// rather than attested", which is a REPORTING question, and it answers it by FILTERING on
    /// `balance_mode`. This one answers "what has this daemon made or lost", and it is mode-BLIND.
    /// On a DEDICATED live account every block is `Authoritative`, so `equity_book_total` is 0.0 and
    /// any risk fraction built on it silently reads no drawdown ever — protection quietly off, which
    /// is a worse failure than the wallet-contaminated total it would be replacing. Same trap for
    /// "just sum the `Delta` blocks". Risk fractions use [`Self::drawdown_curve`].
    pub fn pnl_total(&self) -> f64 {
        vike_model::py_sum(
            self.venues
                .iter()
                .map(|v| v.realized_pnl - v.fees_paid + v.funding_paid + v.unrealized),
        )
    }

    /// **The risk-measurement curve**: [`Self::capital_base`] + [`Self::pnl_total`] — configured
    /// capital plus what this daemon actually did with it. The quantity
    /// `CoreThread::sweep_drawdown_latch` folds its high-water-mark on and
    /// `vike_alerting`'s `RuleTrigger::Drawdown` compares against, so the automatic latch and the
    /// operator-facing alert cannot disagree about what "drawdown" means.
    ///
    /// ⚠ Use this, NOT [`Self::equity_total`], for any drawdown/risk fraction. `equity_total`
    /// includes each `Authoritative` block's venue WALLET for the whole account the credentials
    /// open, so it moves when somebody else moves money and it dwarfs the daemon's own book —
    /// measured on the CI box 2026-08-17, where a 25% loss on ~9000 of book would have read as 3.6% of
    /// a 62647 total.
    ///
    /// ⚠ A non-positive value cannot serve as a fraction denominator (an unseeded or wire-built
    /// portfolio reads `capital_base == 0.0`); callers must guard rather than divide.
    pub fn drawdown_curve(&self) -> f64 {
        self.capital_base + self.pnl_total()
    }

    /// Signed net position size for `symbol` summed across every venue (long +, short −). A plain
    /// fold — this is a read-model helper, not the oracle-gated `equity_total` py_sum path.
    pub fn net_position(&self, symbol: &str) -> f64 {
        self.venues
            .iter()
            .flat_map(|v| v.positions.iter())
            .filter(|p| p.symbol == symbol)
            .map(|p| p.size)
            .sum()
    }

    /// Whether the cross-venue net position in `symbol` is long (> 0).
    pub fn is_net_long(&self, symbol: &str) -> bool {
        self.net_position(symbol) > 0.0
    }

    /// Whether the cross-venue net position in `symbol` is short (< 0).
    pub fn is_net_short(&self, symbol: &str) -> bool {
        self.net_position(symbol) < 0.0
    }

    /// Whether the cross-venue net position in `symbol` is flat (exactly 0 — basis legs cancel).
    pub fn is_flat(&self, symbol: &str) -> bool {
        self.net_position(symbol) == 0.0
    }

    /// Cross-venue GROSS position size for `symbol`: Σ|size| across every venue's legs, never
    /// netting a long against a short (a perfectly hedged pair sums to its two legs, not zero).
    /// The gross twin of [`Self::net_position`] — a plain fold, read-model helper only.
    pub fn gross_position(&self, symbol: &str) -> f64 {
        self.venues
            .iter()
            .flat_map(|v| v.positions.iter())
            .filter(|p| p.symbol == symbol)
            .map(|p| p.size.abs())
            .sum()
    }

    /// Signed net position size for `symbol` on ONE venue — the per-venue slice of
    /// [`Self::net_position`] (which sums across every venue). 0.0 for an unknown venue/symbol.
    pub fn venue_net_position(&self, venue: &str, symbol: &str) -> f64 {
        self.venues
            .iter()
            .filter(|v| v.venue == venue)
            .flat_map(|v| v.positions.iter())
            .filter(|p| p.symbol == symbol)
            .map(|p| p.size)
            .sum()
    }

    /// Every OPEN position the resolver could not price, as `(venue, symbol)` pairs in venue-block
    /// then position order (core-ergonomics). `missing_prices_total` already says HOW MANY are
    /// unpriceable; this says WHICH — the answer an operator staring at the GUI's missing-price
    /// badge, or a strategy deciding whether its own instrument is trustworthy, actually needs.
    ///
    /// "Unpriceable" is exactly `PositionView::mark_source == None` — the same definition the
    /// `PriceBoard` resolve produced when `CoreSnapshot::build` filled these rows, so this never
    /// re-resolves anything or disagrees with the counts beside it. FLAT (zero-size) rows are
    /// EXCLUDED: a closed position leaves a zero-size entry behind forever (see
    /// `CoreThread::any_position_open`), and an unpriceable position you no longer hold is not a
    /// problem worth surfacing. Pure read over already-built `Vec`s, never on the fold path.
    ///
    /// A pair can appear twice only if a venue holds hedge-mode LONG/SHORT legs of the same
    /// symbol, both unpriceable — the rows are reported as they are, not deduped, so the caller
    /// keeps the per-leg truth.
    pub fn missing_price_instruments(&self) -> Vec<(String, String)> {
        self.venues
            .iter()
            .flat_map(|v| v.positions.iter())
            .filter(|p| p.mark_source.is_none() && p.size != 0.0)
            .map(|p| (p.venue.clone(), p.symbol.clone()))
            .collect()
    }
}

#[path = "portfolio_tests.rs"]
#[cfg(test)]
mod portfolio_tests;
