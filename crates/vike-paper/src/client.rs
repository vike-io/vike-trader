//! The paper book itself: `ExecutionClientPaper`, the constructors and the HALT seam. Each fill it
//! books is captured as a `vike_model::Fill` (the r7 gate compares these against the engine's fills).

mod combo;
mod contingency;
mod exec;
mod expiry;
mod fees;
mod fill;
mod slippage;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;

use vike_execution::ContingencyBook;
use vike_marketdata::Bar;
use vike_model::OrderResting;
use vike_model::events::Event;
use vike_model::{ContractValue, FeeSchedule, Fill};

use combo::RestingCombo;
use expiry::OrderExpiry;
use fees::UnderlyingSource;
use slippage::PaperImpact;

/// **Why a paper book refuses an order on `symbol` of `venue`**, or `None`: the symbol is quoted in a fiat
/// currency other than the book's one dollar ledger (`vike_model::DEFAULT_CURRENCY`), and a paper book has no
/// rate source (decision 0183 items 7 and 13), so accepting it would book yen or pounds as dollars at par. The quote
/// is its KEPT row's where the row states a fiat code (`vike_catalog::kept_fiat_quote` over
/// `vike_catalog::kept_row_of`, the datahub's list as this process reads it), else its NAME's. A broker's symbol
/// (an FX or CFD broker, ibkr) whose currency neither states (`vike_model::FiatQuote::Unknown`: an IG index epic with
/// no row), and a row quoted in a minor unit (`vike_model::FiatQuote::Minor`: dukascopy's `GBX`), are refused in the
/// same words, never booked at par; on any other venue a symbol that states no fiat quote is never refused (crypto, a
/// US share, a bare coin). The words are the order's rejection reason, and `vike-tradehub-core` says them again at the
/// paper start.
#[must_use]
pub fn foreign_quote_refusal(venue: &str, symbol: &str) -> Option<String> {
    use vike_model::{DEFAULT_CURRENCY, FiatQuote};
    let row = vike_catalog::kept_row_of(venue, symbol);
    let quote = match vike_catalog::kept_fiat_quote(venue, symbol, row.as_ref())? {
        FiatQuote::Code(quote) if quote == DEFAULT_CURRENCY => return None,
        FiatQuote::Code(quote) => quote,
        FiatQuote::Unknown | FiatQuote::Minor(_) => {
            return Some(format!(
                "paper-quote-currency: the currency {symbol} is quoted in is not known (no kept instrument row of \
                 it on this box states one, and its name names none), and a paper book is one {DEFAULT_CURRENCY} \
                 ledger, so it would book an unknown currency as {DEFAULT_CURRENCY}. Refresh {venue}'s instrument \
                 list, or use a demo account, where the venue states the currency"
            ));
        }
    };
    Some(format!(
        "paper-quote-currency: {symbol} is quoted in {quote}, and a paper book is one {DEFAULT_CURRENCY} \
         ledger with no {quote}/{DEFAULT_CURRENCY} rate, so it would book {quote} amounts as \
         {DEFAULT_CURRENCY}. Paper a {DEFAULT_CURRENCY}-quoted symbol, or use a demo account, where the venue \
         states the rate"
    ))
}

/// **Why a paper book costed at `schedule` refuses an order on `symbol` while nothing prices its UNDERLYING**,
/// or `None`: deribit charges an option a share of its underlying ([`FeeSchedule::PercentOfUnderlying`],
/// `min(0.03 % × underlying, 12.5 % × premium)`), and without the underlying's price a book could only charge
/// the premium, 41 times too little on a measured fill (0.0057 where the venue charged 0.2318). The backtest
/// refuses the same run; the book refuses the order in these words unless its underlying source prices the
/// symbol, and `vike-tradehub-core` says them again at the paper start.
#[must_use]
pub fn unpriced_option_refusal(
    venue: &str,
    symbol: &str,
    schedule: Option<FeeSchedule>,
) -> Option<String> {
    if !matches!(schedule, Some(FeeSchedule::PercentOfUnderlying { .. })) {
        return None;
    }
    let [perp, spot] = vike_catalog::option_underlyings(venue, symbol)?;
    Some(format!(
        "paper-option-fee: {symbol} is an option, and {venue} charges its fee on the UNDERLYING ({perp} \
         or {spot}); this paper book has no price for it, so it would charge the premium instead, about \
         the underlying-to-premium ratio too little. Trade it on a demo account, where the venue charges \
         its own fee"
    ))
}

/// **Whether `venue`'s paper book takes the unit no name fixes from the NODE, read over the venue's DEMO login**
/// (ig, the owner's ruling of 2026-11-16: an epic's `contractSize`): [`paper_contract_value`]'s refusal of such a
/// symbol stands only until the engine states the unit (`ExecutionClient::on_contract_value`), so it is no refusal a
/// paper START can judge (it runs before any statement can exist). Whether a demo login runs to state it is the
/// node's to say (`TradeLeaseRegistry::state_paper_units`, in `vike-tradehub`).
#[must_use]
pub fn unit_stated_over_demo_login(venue: &str) -> bool {
    venue == "ig"
}

/// **What one unit of `symbol` on `venue` is worth to a paper book, or why the book refuses it**: the one
/// answer the book's fees read and `vike-tradehub-core` builds a paper engine's grid from, so the two
/// cannot disagree. The answer is the catalog's (`vike_catalog::kept_contract_value`, the value each live
/// bridge's rows are held to), over the symbol's KEPT row as this process reads it (`vike_catalog::kept_row_of`,
/// the datahub's `<project>/instruments/<venue>.json`: a binance COIN-M or okx inverse face is the row's
/// `contract_size` alone):
///
/// - the catalog's value where it gives one: linear 1.0, `Inverse(1.0)` on a deribit or bybit
///   coin-margined contract, the value its live mount books by (decision 0170), and a binance COIN-M or
///   okx inverse contract at the face its kept row states (`Inverse(100.0)` on `BTCUSD_PERP.P`);
/// - a venue outside the roster (a test's or a replay's book, which no account reaches): the default,
///   linear 1.0, what the book always booked;
/// - every other symbol is refused in words, never booked at a guessed unit: an inverse contract whose
///   face only its row states and no kept row of it is read (binance COIN-M `contractSize`, okx
///   `ctVal`), a deribit coin-settled option
///   (its premium is coin, and the live mount refuses it in the same words), and an ibkr, ig or fxcm
///   symbol whose unit only its row states (a future's multiplier, an epic's contract size, a CFD's
///   `ContractMultiplier`).
///
/// An ig epic's words are the owner's (2026-11-16): its size is a fact of ig's DEMO login, which the
/// node reads and the engine states to the book (`ExecutionClient::on_contract_value`), and the book
/// then values the epic at it; this answer is the name's alone, so it stays the refusal
/// ([`unit_stated_over_demo_login`]).
pub fn paper_contract_value(venue: &str, symbol: &str) -> Result<ContractValue, String> {
    let row = vike_catalog::kept_row_of(venue, symbol);
    if let Some(value) = vike_catalog::kept_contract_value(venue, symbol, row.as_ref()) {
        return Ok(value);
    }
    if vike_model::venues::roster_id(venue).is_none() {
        return Ok(ContractValue::default());
    }
    Err(if vike_catalog::kept_is_inverse_contract(venue, symbol, row.as_ref()) {
        format!(
            "paper-inverse-unit: {symbol} is a coin-margined contract on {venue}, and only the \
             venue's instrument row states what one contract is worth (100 dollars on BTC, 10 on \
             most other coins). No kept instrument list on this box carries its row (the \
             datahub's <project>/instruments/{venue}.json), so a paper book would book a guessed \
             unit. Refresh {venue}'s instrument list, or paper a linear symbol of this venue"
        )
    } else if unit_stated_over_demo_login(venue) {
        format!(
            "no price: no demo login for ig: one unit of the epic {symbol} is its `contractSize`, \
             which only ig states, over a running ig DEMO account's login; none has stated it to \
             this paper book, so it refuses the order rather than book 1 where IG books that \
             figure. Run an ig demo account on this node (`account.active = 1` on its `demo` row): \
             its login states the size, and the book then fills on ig's bid and ask"
        )
    } else if vike_catalog::is_coin_settled_option(venue, symbol) {
        let coin = symbol.split('-').next().unwrap_or(symbol).to_ascii_uppercase();
        format!(
            "paper-coin-option: {symbol} is a coin-settled option: its premium is quoted in {coin}, \
             so its notional and PnL are coin amounts that an account's one-currency ledger would \
             book as dollars. Trade the USDC-settled options of the same underlying"
        )
    } else {
        format!(
            "paper-contract-unit: {symbol} on {venue} is a contract whose unit only the venue's \
             instrument row states (a future's or an option's multiplier, an epic's contract size, \
             a CFD's multiplier), and a paper book holds no row, so it would book 1 where the venue \
             books the row's figure. Paper a symbol whose unit is one: on ibkr a stock, an ETF, a \
             pair or a coin by its user name (AAPL, USDJPY) over the operator's kept ibkr row of it \
             (`vike-cli data instruments add --venue ibkr`), on fxcm a currency pair (EURUSD); ig \
             has none"
        )
    })
}

/// The paper exchange behind the ExecutionClient seam. `submit` publishes
/// Submitted+Accepted and rests the order; `on_bar` (driven by the core on each closed
/// bar of the mounted series) fills the book with the engine's exact semantics —
/// next-open market fills, limit/stop triggers, adverse-first intrabar resolution,
/// slippage + maker/taker fees. Where the venue's quotes reach it (`on_quote`) they are the clock
/// instead, and `fill_at_close` fills a `fill = "close"` strategy's bar orders at that bar's close
/// (decision 0285's amendment of 2026-11-16).
pub struct ExecutionClientPaper {
    pub venue: String,
    pub symbol: String,
    pub slippage: f64,
    pub maker_fee: f64,
    pub taker_fee: f64,
    pub multiplier: f64,
    /// Per-venue fee model (fee model 2/5). `None` (the `new` path) keeps the flat
    /// `maker_fee`/`taker_fee` × `ContractValue::fee` primitive byte-identical (the r7 gate + every
    /// existing caller). `Some` (the `with_fee_schedule` path) applies [`FeeSchedule::commission`]
    /// instead — the per-venue schedule the mount looks up.
    fee_schedule: Option<FeeSchedule>,
    /// Optional underlying/index-price source for the ACCURATE Deribit-options fee (fee model
    /// follow-up 2). `None` on every constructor today => byte-identical legacy commission. `Some`
    /// (via [`Self::with_underlying_source`]) routes a [`FeeSchedule::PercentOfUnderlying`] fill
    /// through [`FeeSchedule::commission_with_underlying`] when it prices `(venue, symbol, ts)`;
    /// see [`UnderlyingSource`] and [`Self::commission_for`].
    ///
    /// TODO(deribit-options-fee-wiring): no production caller supplies this yet — the seam is built
    /// and tested but `vike_tradehub_core::make_engine` does not thread an index-price feed into the paper
    /// Deribit book. Wiring the actual source (which index/mark feed, at what freshness) waits for the
    /// datahub lane work; until then the book REFUSES a Deribit USDC option order
    /// ([`unpriced_option_refusal`]) rather than charge it on the premium.
    underlying_source: Option<UnderlyingSource>,
    pending: Vec<(String, OrderResting)>, // (order client id, resting order)
    /// OTO/OCO/OUO linkage (brackets, order lists, ladders), keyed by order client id — the shared
    /// [`vike_execution::ContingencyBook`] resolver (ONE law for paper, the core and the backtest
    /// OrderBrokerSimulator). Entries exist ONLY for contingent legs, so plain orders are untouched
    /// (byte-identical r7 path). A held OTO exit awaits its parent fill; on a leg's fill its
    /// children arm, its OCO siblings cancel and its OUO siblings shrink in place. See
    /// `apply_contingency`.
    contingency: ContingencyBook,
    /// Time-in-force deadlines, keyed by order client id — present ONLY for orders submitted with an
    /// EXPIRING TIF (`Gtd`/`Day`). A `Gtc` order (the default everywhere) inserts nothing, so this
    /// map stays empty and `expire_due` is a no-op ⇒ the r7 gate and every existing caller are
    /// byte-identical. See [`OrderExpiry`].
    expiry: IndexMap<String, OrderExpiry>,
    /// Resting multi-leg combos, keyed by order client id (combo PR-3). EMPTY on every non-combo run — and
    /// the whole combo path is gated on it being non-empty, so the r7 gate, `leg_marks` and the
    /// single-symbol fill pass are byte-identical when combos are unused. See [`RestingCombo`].
    combos: Vec<(String, RestingCombo)>,
    /// Last observed bar per symbol — the per-leg mark source a combo prices its legs from.
    /// Recorded ONLY while a combo is resting (see `on_bar`), so a plain run allocates nothing —
    /// and DROPPED the moment the combo book empties (`drop_marks_if_idle`), so a combo submitted
    /// later can never price off a mark recorded during an earlier combo's life.
    ///
    /// A combo's legs are OTHER instruments than this book's own `symbol`, so the driver must feed
    /// this book symbol-tagged bars for every leg; `on_bar` never filters by symbol. A leg with no
    /// mark, or a mark older than [`Self::combo_mark_staleness_ms`] at trigger time, simply blocks
    /// the fill (all-or-none) — it never fills at a stale or guessed price.
    leg_marks: IndexMap<String, Bar>,
    /// Freshness bound (ms) every leg's mark must satisfy at combo-TRIGGER time, mirroring the
    /// engine's `StrategyEngineConfig::max_price_staleness_ms` convention exactly (the shared
    /// [`vike_fills::staleness::is_stale`]: stale iff `pass_ts - mark.ts` STRICTLY exceeds the bound; a
    /// negative bound clamps to 0; a future-stamped mark is never stale).
    ///
    /// Default **0** — the strictest setting: every leg must have printed at the SAME ts as the
    /// triggering pass, so the net the trigger tests is one whose leg prices actually coexisted.
    /// Unlike the engine's knob this is NOT `Option` and has no off state, deliberately: "off"
    /// means a resting combo whose leg went dark keeps triggering off `A(t) + B(t−N)` — a net
    /// that never existed — and (with `leg_marks` retention) a later combo filling in full at
    /// arbitrarily old prices stamped with the current ts. A driver whose leg bar streams
    /// interleave across ts (e.g. per-leg feeds a few ms apart) widens this to its bar interval
    /// instead of turning the discipline off.
    pub combo_mark_staleness_ms: i64,
    /// The HALT sentinel this book consults at [`ExecutionClient::submit`], or `None` (the default
    /// on EVERY constructor) to consult none.
    ///
    /// ⚠ **Opt-in is the whole design, not caution.** This client wears two hats: it is the paper
    /// exchange a live DAEMON mounts (`vike_tradehub_core::make_engine`'s paper arms,
    /// `vike_tradehub_core::build_paper_strategy_core_with`), and it is the SIMULATION primitive
    /// `vike-simulator`'s `tests/r7_gate.rs` drives to prove backtest == paper bit-for-bit. Reading a
    /// process-wide sentinel unconditionally would make a backtest's fills depend on a file lying
    /// around in `<project>/settings/state/`, so a stray `HALT` on a dev box or a CI runner would
    /// silently change results and redden the equivalence gate for a reason no one would find. A
    /// MOUNT arms it; a simulation never does, and `None` is byte-identical to the behaviour before
    /// this field existed.
    ///
    /// See [`Self::with_halt_path`] for why a mount passes a resolved path rather than this crate
    /// resolving one itself.
    halt_path: Option<PathBuf>,
    events: VecDeque<Event>,
    /// What the other side of the market offered at the last bar this book saw: `(ask-or-close for
    /// a buy, bid-or-close for a sell)` — [`vike_fills::fill_model::post_only_reference`]. Read at
    /// `submit` ONLY by a post-only order (the one order whose fate depends on the price at
    /// arrival); `None` until the first bar, when a post-only order cannot be judged and rests.
    last_taker: Option<(f64, f64)>,
    /// The close of the last bar of this book's own symbol, `(for a buy, for a sell)`: the price a
    /// trailing order submitted after it trails from (`submit`), as the simulator's trails from its
    /// mark. Both are the bar's close, except on a bar with quote sides, where each is its side's
    /// close (a sell trails the bid, `vike_fills::fill_model::side_bar`). `None` until the first such
    /// bar, when a trailing order is refused.
    last_close: Option<(f64, f64)>,
    /// The order client ids of resting POST-ONLY orders with a release, not judged yet: each is judged once,
    /// at the bar that releases it, against what the market offered as that bar arrived (`on_bar`).
    /// Empty on every run without one, so nothing else reads it.
    post_only_at_release: Vec<String>,
    /// **What one unit of a symbol is worth, as the engine stated it from the venue's facts**
    /// (`ExecutionClient::on_contract_value`), kept only for a symbol whose NAME fixes no unit
    /// ([`paper_contract_value`] refuses it: an IG epic). Empty on every run that states none (every
    /// backtest, the r7 gate), so the book answers by the name alone, as it always did.
    stated_values: IndexMap<String, ContractValue>,
    /// The CURRENT quote of this book's symbol (`on_quote`): `(bid, ask, timestamp)`, a side `None` when the
    /// last quote stated none (not a finite price above zero). `None` until the first, and on every run no
    /// quote reaches (every stored bar, the r7 gate).
    quote: Option<(Option<f64>, Option<f64>, i64)>,
    /// A quote judged the book since its last own bar (decision 0285's amendment of 2026-11-16): the
    /// quotes then ARE the clock, so that bar judges no order (its path holds prices from before an
    /// order placed inside it), and a new order meets the standing quote at once (`submit`). Cleared by
    /// each own bar, so an order a strategy places on a bar waits for the next bar's first quote, and a
    /// series no quote reaches fills on its bars exactly as before.
    quoted: bool,
    /// The START of the window of the last bar of this book's symbol: its sides' stamp (a side path is
    /// stamped with its window's start, whatever stamp the bar beside it carries), else the bar's own
    /// stamp (a venue candle is stamped at its start). `fill_at_close` stamps a fill at this plus the
    /// bar's width: the bar's END on a start-stamped candle and on a synthesizer's end-stamped bar alike.
    last_window_start: Option<i64>,
    /// `true` while the core drains a `fill = "close"` strategy's bar orders (`hold_for_close`): `submit`
    /// then leaves a new order for `fill_at_close` instead of meeting it at the standing quote.
    held_for_close: bool,
    /// The fixed price amount every fill no limit bounds moves against the trader, after `slippage`
    /// (`vike_fills::broker_sim::adverse_fill_amount`: a slippage table's `ticks` model, through
    /// [`Self::with_slippage_terms`]). `0.0` on every constructor, which returns the price untouched.
    slippage_amount: f64,
    /// The opt-in market-impact model a taker fill is charged by, with the bars it measures the market
    /// by (`client/slippage.rs`). `None` on every constructor: the book keeps no bar and every fill pays
    /// `slippage` alone, byte for byte.
    impact: Option<PaperImpact>,
    /// signed position tracked locally (resolve_intrabar_fills needs it for brackets)
    position: f64,
    fill_n: u64,
    /// every fill in emit order — the r7 gate's comparison stream (shared: the
    /// client moves into the core thread; the test keeps a clone)
    pub fills: Arc<Mutex<Vec<Fill>>>,
}

impl ExecutionClientPaper {
    pub fn new(venue: &str, symbol: &str, slippage: f64, maker_fee: f64, taker_fee: f64) -> Self {
        ExecutionClientPaper {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            slippage,
            maker_fee,
            taker_fee,
            multiplier: 1.0,
            fee_schedule: None,
            underlying_source: None,
            pending: Vec::new(),
            contingency: ContingencyBook::new(),
            expiry: IndexMap::new(),
            combos: Vec::new(),
            leg_marks: IndexMap::new(),
            combo_mark_staleness_ms: 0,
            halt_path: None,
            events: VecDeque::new(),
            last_taker: None,
            last_close: None,
            post_only_at_release: Vec::new(),
            stated_values: IndexMap::new(),
            quote: None,
            quoted: false,
            last_window_start: None,
            held_for_close: false,
            slippage_amount: 0.0,
            impact: None,
            position: 0.0,
            fill_n: 0,
            fills: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Construct a paper book that computes commission from a per-venue [`FeeSchedule`] instead of
    /// the flat `maker_fee`/`taker_fee` rates (fee model 2/5). `slippage` stays a SEPARATE cost
    /// (never folded into the fee model). Used by `vike_tradehub_core::make_engine` with
    /// `vike_venues::fee_schedule_for(venue)`; the r7 gate and unit tests keep using [`Self::new`]
    /// (the byte-identical flat-rate path).
    pub fn with_fee_schedule(
        venue: &str,
        symbol: &str,
        slippage: f64,
        fee_schedule: FeeSchedule,
    ) -> Self {
        let mut c = Self::new(venue, symbol, slippage, 0.0, 0.0);
        c.fee_schedule = Some(fee_schedule);
        c
    }

    /// Thread an underlying/index-price source into this book so a Deribit-options
    /// [`FeeSchedule::PercentOfUnderlying`] fill books the ACCURATE
    /// `min(0.03% × underlying, 12.5% × premium)` commission (fee model follow-up 2). Mirrors
    /// `vike_simulator::StrategyEngineConfig::properties`' `(venue, symbol, ts) -> Option<_>` seam.
    ///
    /// **This is a deliberate BEHAVIOR CHANGE, gated entirely on the presence of the source**: a
    /// `PercentOfUnderlying` fill whose `(venue, symbol, ts)` the source prices is charged
    /// ~20x more than the premium-only approximation (0.03%-of-UNDERLYING vs 0.03%-of-premium). With
    /// NO source (every constructor above), or when the source returns `None` at submit, an order on
    /// such an option is refused ([`unpriced_option_refusal`]); every other fee shape and symbol is
    /// byte-identical to before. See [`Self::commission_for`].
    pub fn with_underlying_source(mut self, source: UnderlyingSource) -> Self {
        self.underlying_source = Some(source);
        self
    }

    /// Price this book's fills by a slippage table (the `config.paper_slippage_table` row's), as a
    /// backtest prices a strategy's slippage: `terms` is
    /// `vike_fills::market_impact::slippage_terms`' answer, the ONE resolution both read. Its
    /// `fraction` replaces the constructor's `slippage`, its `amount` is the `ticks` model's fixed price
    /// move (every fill no limit bounds), and its `impact` arms the market-impact model on taker fills,
    /// measured over the last `window` bars of the fill's symbol that closed before it (`client/slippage.rs`).
    ///
    /// OPT-IN: a book never handed terms keeps no bar and fills as it always did. Terms of no table
    /// (`slippage_terms(None)`) are no slippage at all, `0.0`, which is NOT the paper mount's default
    /// flat figure: a mount hands terms only for a table it was given.
    pub fn with_slippage_terms(
        mut self,
        terms: vike_fills::market_impact::ProfileEngineSlippageTerms,
    ) -> Self {
        self.slippage = terms.fraction;
        self.slippage_amount = terms.amount;
        self.impact = terms.impact.map(|(model, window)| PaperImpact::new(model, window));
        self
    }

    /// Arm the operator HALT kill switch on this book: while `path` EXISTS (or cannot be checked), a
    /// submit that opens or adds risk is refused with a terminal `OrderRejected`
    /// ([`vike_execution::halt::HALT_REJECT_REASON`]), and a `reduce_only` submit still passes
    /// ([`vike_execution::halt::halt_admits_submit`] — the same predicate the venue adapters use, so paper
    /// and live cannot disagree about what a halt lets out).
    ///
    /// **Only a MOUNT calls this**, and that is the point. A paper daemon is where an operator
    /// REHEARSES the kill switch, and before this existed `touch <project>/settings/state/HALT` on a paper mount
    /// did nothing at all, silently — the switch appeared armed and was not. A switch that works on
    /// some mounts and not others is worse than one that works nowhere, because the operator cannot
    /// tell which they have. `vike-backtest` and the r7 gate never call this, so the equivalence law
    /// is untouched (see [`Self::halt_path`]'s field doc).
    ///
    /// ⚠ It takes a RESOLVED path instead of resolving one here, for two reasons. This
    /// workspace's standing rule is that libraries take configuration as parameters and only
    /// binaries read the environment (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN`
    /// ratchets that down and would refuse a new library read); and the resolution is not a plain
    /// variable lookup but a precedence (the declared project, else the exe directory) with an
    /// armability probe and a once-per-process report, which lives — deliberately, once — in
    /// `crates/vike-boot/src/halt.rs`'s `halt_path_from_env`. The mount, which owns that module,
    /// passes its answer in, so both mounts and every venue adapter consult ONE path.
    pub fn with_halt_path(mut self, path: PathBuf) -> Self {
        self.halt_path = Some(path);
        self
    }

    /// The sentinel this book was armed with, or `None` when it is a simulation.
    ///
    /// Exists so a MOUNT SEAM can be gated on having armed one, on the CONSTRUCTED book: "every
    /// mount arms the kill switch" is otherwise a property held only by each call site remembering
    /// to — which is exactly how the paper mount came to observe no HALT file at all — and reading
    /// the source text cannot tell an armed construction from an unarmed one.
    ///
    /// ⚠ **A per-seam assertion is not a gate over the SET of seams, and this doc used to claim it
    /// was.** It said `vike_tradehub_core::paper_client` and `vike_run::paper_client_for` "each assert on
    /// this, so adding a third mount seam that forgets would fail" — while
    /// `crates/vike-tradehub-core/src/xemm/paper.rs`'s `build_paper_xemm_core` was ALREADY that third seam, unarmed.
    /// An assertion inside seam A is evidence about A. Nothing was looking at the set, so nothing
    /// could fail. The two halves now in place:
    ///
    /// - **the roster gate** — `crates/vike-ops/tests/architecture/paper_mount_arming_gate.rs` walks every
    ///   crate's `src/` for calls to this client's constructors and fails on a file it does not
    ///   classify as MOUNT (must arm) or SIMULATION (must not). A FOURTH seam reddens there before
    ///   anyone has to notice it is a mount;
    /// - **the per-seam assertions**, which answer the question the roster gate cannot — is THIS
    ///   book armed: `vike_tradehub_core`'s `the_paper_fallback_every_venue_arm_uses_is_halt_armed`,
    ///   `vike_tradehub_core`'s `the_paper_maker_mount_is_halt_armed_on_both_fee_paths` and
    ///   `the_xemm_paper_mount_arms_both_books`.
    pub fn halt_path(&self) -> Option<&Path> {
        self.halt_path.as_deref()
    }

    /// True when this book has an armed sentinel and it is ENGAGED now ⇒ new order placement must be
    /// refused. The answer is `vike_execution::halt::sentinel_engaged`'s, the one rule every enforcing
    /// client asks, so a sentinel that cannot be CHECKED counts as engaged here too (never `exists()`,
    /// which reads a permission or I/O error as "no file"). `None` (every non-mount caller)
    /// short-circuits without touching the filesystem, which is what keeps a backtest allocation- and
    /// syscall-free on this path, and without that function's "not wired" line: a simulation is not a
    /// mount that forgot its path.
    fn halt_engaged(&self) -> bool {
        self.halt_path.as_deref().is_some_and(|p| vike_execution::halt::sentinel_engaged(Some(p)))
    }
}

#[cfg(test)]
mod modify_tests;

#[cfg(test)]
mod fee_schedule_tests;

#[cfg(test)]
mod time_in_force_expiry_tests;

#[cfg(test)]
mod bracket_tests;

#[cfg(test)]
mod combo_tests;

#[cfg(test)]
mod quote_tests;

#[cfg(test)]
mod post_only_tests;

#[cfg(test)]
mod trigger_kinds_tests;

#[cfg(test)]
mod trailing_kinds_tests;

#[cfg(test)]
mod entry_terms_tests;

#[cfg(test)]
mod attributes_tests;

#[cfg(test)]
mod inverse_tests;

#[cfg(test)]
mod side_fill_tests;

#[cfg(test)]
mod quote_clock_tests;

#[cfg(test)]
mod slippage_tests;

#[cfg(test)]
mod test_support;
