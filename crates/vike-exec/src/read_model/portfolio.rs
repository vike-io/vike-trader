//! Portfolio — the named live cross-venue aggregator.
//!
//! Groups the portfolio slice of [`super::CoreSnapshot`] — the equity/realized/fees/
//! funding totals plus the per-venue `venues: Vec<VenueBlock>` rows — under one named type that
//! folds the per-venue `Account`s into one cross-venue view. This is purely a naming reshape of
//! fields that were loose scalars on `CoreSnapshot`; no value changes. [`VenueBlock`] (beside it in
//! the read model) stays the per-venue element below it. See the design spec:
//! `docs/superpowers/specs/2026-07-17-portfolio-read-model.md` (dedup finding F24).

use super::{Interned, VenueBlock};
use crate::BalanceMode;

/// One currency group's cross-venue totals: what [`Portfolio`]'s totals say once the node's engines are
/// not all in one currency (a EUR account beside a dollar one). Every figure is in [`Self::ccy`] and
/// folds only the blocks whose [`VenueBlock::ledger_ccy`] is that group. See [`Portfolio::ledger_totals`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LedgerTotals {
    /// The group's currency key, EMPTY for the dollar group ([`VenueBlock::ledger_ccy`]).
    pub ccy: Interned,
    /// `Σ` block equity (the group's [`Portfolio::equity_total`]).
    pub equity: f64,
    /// [`Portfolio::equity_book_total`] over the group.
    pub equity_book: f64,
    /// [`Portfolio::equity_wallet_total`] over the group.
    pub equity_wallet: f64,
    /// [`Portfolio::realized_pnl_total`] over the group.
    pub realized_pnl: f64,
    /// [`Portfolio::fees_paid_total`] over the group.
    pub fees_paid: f64,
    /// [`Portfolio::funding_paid_total`] over the group.
    pub funding_paid: f64,
    /// `Σ` block `margin_used` over the group (the group's [`Portfolio::margin_used_total`]).
    pub margin_used: f64,
    /// [`Portfolio::pnl_total`] over the group.
    pub pnl: f64,
}

impl LedgerTotals {
    /// A group key as text for a report: the empty dollar key reads `"USD"`, any other is itself.
    #[must_use]
    pub fn label_of(ccy: Interned) -> &'static str {
        if ccy.is_empty() { "USD" } else { ccy.as_str() }
    }

    /// This group's currency as text ([`Self::label_of`]).
    #[must_use]
    pub fn label(&self) -> &'static str {
        Self::label_of(self.ccy)
    }
}

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
    ///
    /// ⚠ **ONE CURRENCY: the PRIMARY block's group** ([`Self::primary_ccy`]). Blocks of an engine in
    /// another currency are left out, because their figures are not in the primary's: see
    /// [`Self::ledger_totals`] for one row per group. A node whose engines share a currency, which is
    /// every node today, folds every block exactly as before.
    pub equity_total: f64,
    pub realized_pnl: f64,
    pub fees_paid: f64,
    pub funding_paid: f64,
    /// cross-venue sum of the `margin_used` of every venue in the PRIMARY block's currency group
    /// (GUI equity strip; 0.0 when the gate is off). See [`Self::equity_total`] for the group rule.
    pub margin_used_total: f64,
    /// **The operator's CONFIGURED capital**: `Σ` each engine's `seed_cash`, py_sum in venue
    /// registration order (primary first) — the frozen base of [`Self::drawdown_curve`].
    ///
    /// Only the engines of the PRIMARY block's currency group are in it ([`Self::equity_total`]):
    /// the same engines [`Self::pnl_total`] folds, so the curve adds two figures of one currency.
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
    /// **The PRIMARY currency group's key, when the frame this portfolio was built from STATES it**
    /// (`WireSnapshot::primary_ccy`, read by the observe bridge); `None` for a portfolio the core built
    /// itself, whose primary is its first block's (`vike_core::snapshot::build` keeps the stopped primary's
    /// block first). See [`Portfolio::primary_ccy`] for why an observer must not derive it.
    pub primary_ccy_stated: Option<Interned>,
    /// **The drawdown curve and high-water mark of every currency group but the PRIMARY's**, as the core's
    /// latch last measured them (`CoreThread::sweep_drawdown_latch`): what lets a push alert watch a group the
    /// primary-group curve ([`Portfolio::drawdown_curve`]) cannot see, using the group's own seed and peak the
    /// latch already keeps (no per-group capital base is published). EMPTY, and never allocated, on a node
    /// whose engines share a currency, and until the latch has swept (it runs only with `max_drawdown`
    /// armed). Never carried on the observe wire: an observer drives no alerting.
    pub group_curves: Vec<GroupCurve>,
}

/// One currency group's drawdown curve and high-water mark ([`Portfolio::group_curves`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroupCurve {
    /// The group's key, EMPTY for the dollar group ([`VenueBlock::ledger_ccy`]).
    pub ccy: Interned,
    /// `capital base + own PnL` of the group at the latch's last sweep, in the group's currency.
    pub curve: f64,
    /// The group's high-water mark of that curve (the latch's own).
    pub peak: f64,
}

/// Query surface over the per-venue `venues` rows (the design spec's optional step 4). These are
/// pure read helpers over the already-built `Vec<VenueBlock>` — GUI/strategy convenience, never on
/// the core fold path — keeping the accessor names (`net_position`/`is_net_long`/…) of the older,
/// engine-side cross-venue aggregator this crate once carried, plus per-venue lookups
/// (`venue(venue)`/`unrealized_pnl(venue)`). That aggregator was retired; this type took its name
/// when the read model moved here from vike-core, and it is not that type back: it folds nothing
/// itself, vike-core's `snapshot::build` fills it from the engines.
/// An unknown venue reads as absent (0.0 / `None`); an unknown symbol nets to 0.0 (flat), never a
/// panic.
impl Portfolio {
    /// The per-venue block for `venue`, or `None` if this portfolio carries no row for it.
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
    /// it is the PRIMARY engine's number and nothing else (`vike_core::snapshot::build` binds it
    /// from `engine.account`), so on a multi-venue core it answers about whichever venue happens to
    /// be registered first. Any REPORT that also carries `equity_total` must fold this instead, or
    /// its numbers describe two different accounts — measured on the CI box, where `vike-tradehub`'s
    /// summary line read `realized_pnl: 0.0` from the untraded binance primary while
    /// `equity_total` tracked the bybit mount's ten maker fills to eight decimal places.
    ///
    /// `py_sum` (not a plain fold) deliberately: this sums the SAME vec in the SAME order as
    /// `equity_total`, and a sibling total reported alongside it should not disagree in the last
    /// ULP over the choice of fold.
    ///
    /// ⚠ Like every total below, it folds the PRIMARY block's currency group only
    /// ([`Self::primary_ccy`]); the other groups are in [`Self::ledger_totals`].
    pub fn realized_pnl_total(&self) -> f64 {
        self.realized_pnl_in(self.primary_ccy())
    }

    /// Cross-venue fees paid — the [`Self::realized_pnl_total`] twin for `fees_paid`, with the same
    /// primary-only footgun on the scalar `fees_paid` field and the same `py_sum` law.
    pub fn fees_paid_total(&self) -> f64 {
        self.fees_paid_in(self.primary_ccy())
    }

    /// Cross-venue funding paid — the [`Self::realized_pnl_total`] twin for `funding_paid`, same
    /// primary-only footgun on the scalar field, same `py_sum` law.
    pub fn funding_paid_total(&self) -> f64 {
        self.funding_paid_in(self.primary_ccy())
    }

    /// **The PRIMARY currency group's key**: [`Self::primary_ccy_stated`] when the portfolio was built from
    /// a frame that states it, else the first block's [`VenueBlock::ledger_ccy`] (the node's default engine
    /// is first), EMPTY for the dollar group and on a portfolio with no block yet. Every cross-venue total
    /// on this type folds exactly the blocks of this group.
    ///
    /// ⚠ The core's list keeps a stopped primary's tombstone first, but the observe frame carries only the
    /// newest block of each account (`vike_tradehub::publish::current_venue_blocks`), so after a primary
    /// restart a frame's first block can be another engine's. An observer therefore never derives the
    /// primary from the frame's blocks: it reads the key the node states.
    #[must_use]
    pub fn primary_ccy(&self) -> Interned {
        self.primary_ccy_stated
            .unwrap_or_else(|| self.venues.first().map_or_else(Interned::default, |v| v.ledger_ccy))
    }

    /// Whether the blocks span more than one currency group. `false` for every node whose engines
    /// share a currency (all of them today), where every total here folds every block.
    #[must_use]
    pub fn is_multi_currency(&self) -> bool {
        let primary = self.primary_ccy();
        self.venues.iter().any(|v| v.ledger_ccy != primary)
    }

    /// The currency groups present, PRIMARY first, then the others in the order their first block
    /// appears. One element for a node whose engines share a currency.
    #[must_use]
    pub fn ledger_ccys(&self) -> Vec<Interned> {
        if self.venues.is_empty() {
            return Vec::new();
        }
        let mut groups: Vec<Interned> = vec![self.primary_ccy()];
        for v in &self.venues {
            if !groups.contains(&v.ledger_ccy) {
                groups.push(v.ledger_ccy);
            }
        }
        groups
    }

    /// The blocks of one currency group, in venue registration order.
    fn group(&self, ccy: Interned) -> impl Iterator<Item = &VenueBlock> {
        self.venues.iter().filter(move |v| v.ledger_ccy == ccy)
    }

    /// [`Self::realized_pnl_total`] for the group `ccy`.
    #[must_use]
    pub fn realized_pnl_in(&self, ccy: Interned) -> f64 {
        vike_model::py_sum(self.group(ccy).map(|v| v.realized_pnl))
    }

    /// [`Self::fees_paid_total`] for the group `ccy`.
    #[must_use]
    pub fn fees_paid_in(&self, ccy: Interned) -> f64 {
        vike_model::py_sum(self.group(ccy).map(|v| v.fees_paid))
    }

    /// [`Self::funding_paid_total`] for the group `ccy`.
    #[must_use]
    pub fn funding_paid_in(&self, ccy: Interned) -> f64 {
        vike_model::py_sum(self.group(ccy).map(|v| v.funding_paid))
    }

    /// [`Self::pnl_total`] for the group `ccy`.
    #[must_use]
    pub fn pnl_in(&self, ccy: Interned) -> f64 {
        vike_model::py_sum(
            self.group(ccy).map(|v| v.realized_pnl - v.fees_paid + v.funding_paid + v.unrealized),
        )
    }

    /// [`Self::equity_book_total`] for the group `ccy`.
    #[must_use]
    pub fn equity_book_in(&self, ccy: Interned) -> f64 {
        vike_model::py_sum(
            self.group(ccy).filter(|v| v.balance_mode == BalanceMode::Delta).map(|v| v.equity),
        )
    }

    /// [`Self::equity_wallet_total`] for the group `ccy`.
    #[must_use]
    pub fn equity_wallet_in(&self, ccy: Interned) -> f64 {
        vike_model::py_sum(
            self.group(ccy)
                .filter(|v| v.balance_mode == BalanceMode::Authoritative)
                .map(|v| v.equity),
        )
    }

    /// **One row of totals per currency group**, the primary's first: how a report shows a node whose
    /// engines are not all in one currency, instead of adding the figures into a number that is no
    /// money. The first row's `equity`, `margin_used`, `realized_pnl`, `fees_paid`, `funding_paid` and
    /// `pnl` are [`Self::equity_total`], [`Self::margin_used_total`] and the `*_total` methods, bit for
    /// bit (the same blocks, the same order, the same fold). A node of one currency answers one row.
    ///
    /// Cold: a report builds this, never the core's publish.
    #[must_use]
    pub fn ledger_totals(&self) -> Vec<LedgerTotals> {
        self.ledger_ccys()
            .into_iter()
            .map(|ccy| LedgerTotals {
                ccy,
                equity: vike_model::py_sum(self.group(ccy).map(|v| v.equity)),
                equity_book: self.equity_book_in(ccy),
                equity_wallet: self.equity_wallet_in(ccy),
                realized_pnl: self.realized_pnl_in(ccy),
                fees_paid: self.fees_paid_in(ccy),
                funding_paid: self.funding_paid_in(ccy),
                // The fold `snapshot::build` and the observe bridge both run for `margin_used_total`
                // (a naive left fold from +0.0), so the primary row equals that field.
                margin_used: self.group(ccy).fold(0.0, |acc, v| acc + v.margin_used),
                pnl: self.pnl_in(ccy),
            })
            .collect()
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
        self.equity_book_in(self.primary_ccy())
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
        self.equity_wallet_in(self.primary_ccy())
    }

    /// The venues contributing to [`Self::equity_wallet_total`], in venue registration order —
    /// so a report that quotes a venue wallet can NAME whose wallet it is quoting. Empty on a
    /// pure-paper core (nothing has ever attested a balance).
    pub fn wallet_venues(&self) -> Vec<&str> {
        self.group(self.primary_ccy())
            .filter(|v| v.balance_mode == BalanceMode::Authoritative)
            .map(|v| v.venue.as_str())
            .collect()
    }

    /// **The daemon's OWN profit and loss across every venue** — the published twin of
    /// [`crate::ExecutionEngine::resolved_own_pnl`], folded from the four `VenueBlock` fields
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
        self.pnl_in(self.primary_ccy())
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
    /// `PriceBoard` resolve produced when `vike_core::snapshot::build` filled these rows, so this
    /// never re-resolves anything or disagrees with the counts beside it. FLAT (zero-size) rows
    /// are EXCLUDED: a closed position leaves a zero-size entry behind forever (see
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
            .map(|p| (p.venue.to_string(), p.symbol.to_string()))
            .collect()
    }
}

#[path = "portfolio_tests.rs"]
#[cfg(test)]
mod portfolio_tests;
