//! `server::symbol_admission` — the control door lets an order name ANY symbol of its exchange: it
//! spells the symbol the venue's way, resolves the symbol's facts, and hands the engine an
//! `AdmitSymbol` ahead of the order (decision 0173; owner ruling 2026-10-03/04: an ACCOUNT trades
//! any symbol of its exchange at any time, no symbol glued to an account or a mount).
//!
//! # The four classes of venue, one explicit table
//!
//! [`SYMBOL_RULES`] says, per roster venue, what its adapter does with an order's symbol (read in
//! each adapter's order builder):
//!
//! * **[`VenueSymbolRule::Admits`]: bybit, binance, aster, okx, hyperliquid, deribit, alpaca, ctrader, oanda, ig, ibkr, dukascopy, fxcm, polymarket.** The adapter signs each order
//!   on the ORDER's symbol, on that symbol's own grid (`crates/vike-bridge-core/src/per_symbol.rs`'s
//!   `VenueRestPerSymbol`; okx's also on that instrument's own contract size; hyperliquid's through its loaded
//!   `Symbology`, which keys the asset index by the order's coin; deribit's through one client that
//!   resolves each instrument's row on first use), so the door admits the symbol into the engine first.
//!   **deribit admits PERPETUALS, DATED FUTURES and USDC OPTIONS of the engine's own money model only**
//!   (`money_model_refusal`, by the kept rows of the mount and the symbol): an engine counts money one way, so an inverse
//!   engine (an amount of US dollars) takes other inverse futures and a linear one (an amount of the base
//!   coin) other linear futures and USDC options; a coin-settled option and a spot pair are refused by name
//!   (decision 0233 says why and what lifting each would take; `docs/ops/deribit-any-symbol-plan.md`).
//!   **alpaca admits
//!   equities and crypto pairs**: every instrument is a USD-quoted share or coin
//!   with a multiplier of 1, so the engine's `qty x price` is the real USD exposure whatever the symbol;
//!   its facts are read with the account's own credentials (`docs/ops/any-symbol-remaining-venues-plan.md`).
//!   **ctrader admits FOREX PAIRS and the CFD classes whose unit of volume was measured** (metals, indices,
//!   oil and commodities, crypto: `vike_ctrader::catalog::MEASURED_CFD_CLASSES`; the broker's own name is its kept row's
//!   wire, its bridge's alone): the
//!   adapter resolves each order's symbol in the account's own symbol list and snaps it on that symbol's
//!   volume grid. An instrument quoted in the account's deposit currency is admitted as it is; one quoted in another
//!   currency is admitted for a `demo` account with an account-wide ceiling armed, with the conversion the
//!   facts carry (decision 0183: the gatewayhub reads the rate from a one-shot session of the account's own,
//!   `ctrader_symbol_facts_quoted`). What a share, a bond or a dated-futures CFD's unit means is unmeasured, so the gatewayhub's facts (read
//!   from the account's own demo session, `ctrader_symbol_facts`) refuse every other instrument by name. Its price handle is its MOUNT's
//!   `StrategyLanesLive::Ctrader` or, on a box that mounts no ctrader, the PRICE-ONLY feed
//!   (`crate::feeds::wire_price_only_feeds`), and its reconcile client reads every admitted symbol.
//!   **oanda admits instruments QUOTED IN THE ACCOUNT'S CURRENCY** (the
//!   gatewayhub's plain facts refuse any other, because at par a USDJPY position on a USD account would be
//!   valued about 150 times too high; since decision 0183 an account on the `demo` tier with an account-wide
//!   exposure ceiling is the exception, `quote_conversion_refusal`: its node asks for OANDA's own
//!   conversion rate and the engine books the pair's money through it), whose unit step is 1 (the
//!   adapter rounds to whole units) and, for a currency pair, whatever price precision its own row states
//!   (the adapter writes exactly that many decimals: five for EURUSD, three for a JPY pair; any other class
//!   keeps the five decimals the adapter was proven on); its price handle is its MOUNT's
//!   `StrategyLanesLive::Oanda` or, on a box that mounts no OANDA market, the PRICE-ONLY feed
//!   (`crate::feeds::wire_price_only_feeds`; without it an unmounted account would lose every foreign
//!   pair), and its reconcile client reads every admitted instrument.
//!   **ig admits an epic of a measured type (a currency pair, an index, a commodity, a coin, a bond future, dated
//!   or not) quoted in a USD account's currency** (by its user name, `EURUSD`, the epic being its kept row's; and, for a
//!   `demo` account with an account-wide ceiling armed, in another currency too, converted at the rate
//!   IG's market details state and a price confirms: its `vike_venues::venue_currency` row, decision 0183): the facts
//!   (`vike_ig::instrument`, over the account's own login) refuse by name a share, an option and a spread bet,
//!   and admit an epic only when its own payload agrees about the contract multiplier; its price handle is its MOUNT's `StrategyLanesLive::Ig` or, on a box that
//!   mounts no ig, the PRICE-ONLY feed (a quote lane per admitted epic, at most `vike_gatewayhub_client::credentialed_limits::IG`), and its reconcile client reads every
//!   admitted epic.
//!   **ibkr admits a stock, a forex pair or a dated future by its user name** (`AAPL`, `USDJPY`, `ES_261218`: decision
//!   0290 over 0234; the operator's kept row states its exchange and currency, and an option is refused by its shape): IBKR
//!   publishes no instrument list, so the gatewayhub asks the account's gateway (a symbol search, contract
//!   details, market rules) and refuses an ambiguous ticker, another currency than the engine's, and every other
//!   kind of contract by name. Its price lane is a price-only feed (`feeds/ibkr.rs`) whose tap prices a two-sided quote of
//!   any LABELLED stream, a delayed or frozen one included (decision 0140's amendment of 2026-10-26: the label is
//!   recorded with the order, never a gate), and with no ibkr lane on the datahub the door answers `no price feed`
//!   to an opening order on a new stock. ⚠ The node's own door has run end to end on the paper gateway for a new
//!   stock in the base currency (2026-10-27); a FROZEN label has not been seen, and a stock in another currency is
//!   refused at the rate (`docs/ops/ibkr-socket-gateway.md`, "Delayed data prices a new stock").
//!   A `demo` account with an account-wide ceiling armed, whose ledger the engine's assembly anchored to the
//!   base currency IBKR states (decision 0183, `unanchorable_mount`), is also admitted a stock of ANOTHER
//!   currency, with the conversion the facts carry (`stock_conversion_missing`).
//!   **dukascopy admits a currency PAIR** (an FX or metal row of its kept list; quoted in the engine's own money, or
//!   in another one for a converting `demo` account, below):
//!   its facts are the committed JForex SDK table's (static, offline: `crates/bridges/dukascopy/src/instrument/any_symbol.rs`'s
//!   `instrument_facts`); a metal (`XAUUSD`) or a CFD, spelled with the SDK's slashed name (`COPPER.CMD/USD`, never the
//!   enum-style `COPPERCMDUSD`), is answered on the venue's OWN terms of it, which the converting lane reads off the
//!   sidecar (`instrument_facts_quoted` and `facts_from_terms` in that file), and a coin, a CFD in pence and
//!   every name the table lacks is refused by name there. Its price lane is a
//!   price-only feed (`feeds/dukascopy.rs`) over the quotes of the JForex sidecar the gatewayhub already runs: a
//!   pair is priced only while a sidecar whose jar announces `ticks` is up, from a real-time two-sided quote,
//!   and with none the door answers `cannot subscribe dukascopy's prices for ...: ...` (or `no price feed` when
//!   the datahub advertises no dukascopy lane). It has run on the JForex demo login since the drills of 2026-10-26
//!   and 10-27: the node admitted USDJPY, priced it from the sidecar's ticks and traded it
//!   (`docs/ops/known-limitations-2026-10.md`, the dukascopy rows); its tick cadence is still unmeasured.
//!   A pair quoted in ANOTHER money (`USDJPY`, `EURGBP`) is admitted for a `demo` account with an account-wide
//!   ceiling armed (decision 0183), only with the currency and the rate the facts state: the gatewayhub prices
//!   it from the sidecar's own ticks of the conversion pair, and the fence above is what that relaxes.
//!   **fxcm admits a regular forex PAIR** (as dukascopy) under the SAME fence: its facts are the account's own running reconcile session's Offers table
//!   (`vike_fxcm::symbol_properties`, which the gatewayhub calls; no second login), which also admits every other class the SDK names (a metal,
//!   an index, a commodity, a CFD: any name the venue's table lists, found by its upper-case spelling without a
//!   slash, and judged by its instrument type; such a name is not a six-letter pair, so the pair fence below does
//!   not read it and its facts alone state its currency and rate), and a
//!   pair quoted in another currency than the ENGINE's carries FXCM's own conversion rate, admitted only for a
//!   `demo` account with an account-wide exposure ceiling armed ([`quote_conversion_refusal`]). A pair in the
//!   account's own money is still refused into an engine whose mounted pair counts another money (the facts
//!   state no currency for it, and `foreign_money` below demands one). Its price lane is a price-only feed
//!   (`feeds/fxcm.rs`) over the same session's Offers table, polled for the pairs asked and no others: a pair is
//!   priced only from a live two-sided quote of an open market no older than two minutes by its own time, and
//!   with no reconcile session the door answers `cannot subscribe fxcm's prices for ...: ...` (or `no price feed`
//!   when the datahub advertises no fxcm lane). Only a `demo` account runs (the adapter dials the demo login
//!   alone); a `paper` engine is refused and a `live` one does not exist. ⚠ Nothing of it has run against a
//!   ForexConnect session. Its reconcile client reads every admitted pair (it parses the whole-account Orders and
//!   Trades snapshots once per told pair: `RECONCILE_READS_ADMITTED` names the venue).
//!   **polymarket admits an outcome TOKEN, typed as its id or as a market NAME** ([`VenueSymbolSpelling::OutcomeToken`]): the
//!   adapter signs each order on the order's token (it resolves the NegRisk domain and the tick per token), and
//!   the engine of a Polymarket account is mounted on NO symbol (an account-wide mount: `VENUE_WIRING`'s `EngineMount::Account`, an empty
//!   symbol), so EVERY token is one the door admits first, [`account_wide`] being the one place the planner reads
//!   an empty engine symbol as "no mount" instead of "not said". The token id (20 or more digits) is the one name:
//!   fills, positions and every order carry it. What an operator may type instead (`slug:Outcome`, or the market's
//!   polymarket.com address) is turned into it by [`SymbolAdmitter::resolve`], which the production source asks of the
//!   gatewayhub over the account's trade lease: Gamma is a venue socket, and this node opens none. The facts (the
//!   market's tick and minimum size in shares, the share lot, the NegRisk flag) are the gatewayhub's, from Gamma's
//!   point lookup by token (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`), and a market that
//!   takes no orders is refused by name. Its price lane is a price-only feed (`feeds.rs`'s
//!   `PRICE_ONLY_ARMS`, a quote subscription per token over the strategy lane), on a box that mounts no
//!   Polymarket token; its reconcile client is account-wide (`RECONCILE_READS_ADMITTED` names the venue). ⚠ Offline
//!   tests only: nothing of it has run against the live CLOB.
//! * **[`VenueSymbolRule::MountedOnly`]: no roster venue today.** An adapter that signs EVERY order on the
//!   symbol its engine was mounted on, whatever the order names, has this row: the door REFUSES an
//!   order on any other symbol, with the row's reason, until that adapter is finished. Deleting a row
//!   here is the whole change that opens a venue, once its adapter reads the order's symbol (deribit's
//!   row was the last one).
//! * **[`VenueSymbolRule::PerOrder`]: no roster venue today** (polymarket's was the last, until it became `Admits`).
//!   An adapter that already reads the order's own symbol, whose engine judges it well enough, has this row;
//!   the door says nothing about WHICH symbol.
//!   `per_order_refusal` keeps two fences for a symbol other than the engine's own, for a venue that
//!   would hold this row (oanda did, until it became `Admits`): a paper account is refused (one book,
//!   fills stamped with the mounted symbol), and an opening order with no limit price is refused (no
//!   price watcher, so the engine would judge it at notional 0). A reduce-only order is not touched.
//! * **[`VenueSymbolRule::PerOrderUnsafe`]: no roster venue today** (fxcm's was the last, until its price lane).
//!   An adapter that reads the order's symbol but whose ENGINE cannot judge a foreign one (it rounds on the
//!   mounted symbol's grid, knows no multiplier or has no price) has this row: the door REFUSES a foreign
//!   symbol with the row's reason until admission supports the venue (an interim fence; deleting the row
//!   opens it).
//!
//! # What the door does for an `Admits` venue ([`plan_submit_symbol`])
//!
//! 1. The engine's OWN symbol, spelled as the engine spells it, passes UNTOUCHED: no extra command,
//!    byte-identical, no price asked and no wait, with ONE exception: an OPENING order with no price of its own on a
//!    `demo`/`live` engine that no strategy mount prices is held for the symbol's first price, and refused by name if
//!    none comes ([`own_first_price`]; the owner's ruling, "we cant trade until we get price": at notional zero every
//!    cap passes, on a grid with no `min_notional` nothing else stops it). Since decision 0197 a COMMITTING surface
//!    also tells the source the engine's own symbol is being ordered in ([`keep_own_lane`],
//!    [`SymbolAdmitter::hold_lane`]): the source keeps that symbol's price lane open while the engine holds something
//!    in it, off the order's thread. A declared leg or an earlier admission (this run's, or a restored one) passes
//!    the spelling and admission steps but still asks the price lane (step 6) on a `demo`/`live` engine.
//! 2. Otherwise the symbol is read as its USER name (decision 0290, `vike_catalog::door_user_symbol`, which
//!    `vike-cli account set-symbol` reads too): the typed text trimmed and upper-cased, a name refused by its
//!    shape alone (an option off deribit, a deribit coin-settled option, hyperliquid's `@N` wire name) refused in its
//!    words, then resolved through the venue's KEPT row ([`SymbolAdmitter::kept_rows`]): one grammar per class on
//!    every venue (FX `USDJPY`, spot `BTCUSDT`, perpetual `BTCUSDT.P`, dated `BTCUSDT_261225`, stock `AAPL`, stock CFD
//!    `AAPL.CFD`), and the row's canonical user name is what the order and the admission carry. No list (its words name
//!    the refresh that keeps one), no row or two rows is a refusal; an old spelling (`BTC-PERPETUAL`, `EUR_USD`,
//!    `AAPL.SMART.USD`) names no row. The venue's own spelling is its bridge's. One instrument is one user name in any
//!    case, so the engine's own symbol or one it trades, typed in another case, is that symbol on every tier.
//! 3. Only a `demo` or `live` engine takes a foreign symbol: a paper engine is one book.
//! 4. A binance or aster engine is ONE lane (spot or USDⓈ-M perp, by its mounted symbol): a symbol of
//!    the other lane is refused here, by name, instead of by a late `OrderRejected` (only the mounted
//!    lane has a user-data stream, so its fills would never arrive).
//! 5. The facts come from the [`SymbolAdmitter`] the surface was given, off the fold thread. Nothing is
//!    ever guessed: a symbol the venue does not list, a failed lookup, a grid without a tick or a lot,
//!    a surface with no source and a 17th extra symbol are each a refusal naming the cause. An OPENING
//!    order for a venue this node holds no price watcher for ([`SymbolAdmitter::price_source`]) is refused
//!    BEFORE the lookup (ibkr's lookup is a budgeted data connect), in step 6's words.
//!    The facts also state the symbol's contract KIND (linear or inverse); an engine counts money ONE way, so
//!    a symbol of the other kind than the engine's mounted symbol is refused by name ([`contract_kind_refusal`]),
//!    for every venue: admitting it would make the engine drop the fills of the other kind. An engine also refuses
//!    a contract of its kind that settles in another coin ([`settle_coin_refusal`], decision 0258: one settle coin
//!    per engine, linear engines included), both coins read off the kept rows by `vike_model::MoneyModel`, for every
//!    venue.
//! 6. An OPENING order on a symbol other than the engine's own needs a PRICE (the collar, the exposure
//!    caps and the account margin are judged at the mark). Asking [`SymbolAdmitter::price`] subscribes the
//!    symbol's quote feed the first time; the door holds the order up to [`PRICE_WAIT`] for the first
//!    price and refuses it, naming the cause, if none arrives. A reduce-only order is never held up.
//!
//! A LABELLED account (`bybit#DEMO`) is admitted a symbol exactly like the default one, under its own
//! route key: the core fans a venue's quote, trade and book out to every engine admitted the symbol
//! (`vike-core`'s `CoreThread::admitted_fanout`), where it used to reach the default engine only, so
//! the door no longer refuses it by name.
//!
//! The verdict is a [`SymbolPlan`]: `accept_command` sends the admission, then the order, on the one
//! command lane; the Preview surfaces call [`submit_symbol_refusal`] and see the same refusal.

use std::sync::Arc;

// The door's reading of a symbol and its rule table are vike-catalog's, shared with `vike-cli account set-symbol`.
use vike_catalog::{SYMBOL_RULES, VenueSymbolRule, VenueSymbolSpelling, is_outcome_token};
use vike_execution::{MAX_EXTRA_SYMBOLS, SymbolAdmission, VenueBlock};
use vike_gatewayhub_client::credentialed_limits::CredentialedStreamLogin;
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;
use vike_hub_protocol::refusal::HubRefusal;
use vike_model::accounts::account_keys::{
    AccountLabel, RESERVED_DEFAULT_LABEL, parse_wire_account, route_key_of,
};
use vike_model::{AccountTier, OrderType, VenueRead, VenueReadKind};
use vike_model::{AssetClass, Contract, ContractCfdUnderlying, Instrument};
use vike_tradehub_client::wire::{
    CommandWire, PriceLinkDownWire, TradeLinkDownWire, VenueLinkDownWire,
};
use vike_utility::errors::CarriedRefusal;
use vike_venues::venue_currency::{
    VENUE_CURRENCY, VenueCurrencyRead, VenueEngineMoney, VenueQuoteConversion,
    VenueRateLedgerStamp, venue_currency_for,
};

use crate::refusals::SymbolRefusal;

/// What the door knows of an account whose engine has not been given the venue's balance
/// ([`AccountReadiness::awaiting_balance`]): how long it has waited and why the leg's pass has not answered, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalanceWait {
    /// Since the start (the boot's or the account's) announced the wait.
    pub since: std::time::Duration,
    /// The last thing the reconcile leg said: not reached yet, not ready, a failed fetch, a suppressed feed.
    pub why: String,
}

/// **Where a surface gets the FACTS of a symbol its engine was not mounted on, its PRICE, and its ADMISSION**: the
/// lookup, the price lane, the commit to the daemon and the give-back. Blocking (a lookup may cross the network), so
/// it is called on a connection thread, never on the core's fold; a production source caches. An admission is
/// judged against the account it goes into (a live account's ceiling, a lane twin's identity), so an admitter is
/// also the [`AccountReadiness`] of its accounts.
pub trait SymbolAdmitter: AccountReadiness + Send + Sync {
    /// Whether this node can price ANY symbol of `venue` at all (a price watcher exists for it), judged
    /// WITHOUT a network call. `Err` is the reason, in an operator's words. The door asks it BEFORE a facts
    /// lookup for an opening order on a new symbol: a lookup can cost a real connect (ibkr's data
    /// connection is budgeted per hour), and an order no price can ever back must not spend it. The default
    /// answers `Ok`: a source that does not know does not block ([`Self::price`] still refuses later).
    fn price_source(&self, _venue: &str) -> Result<(), String> {
        Ok(())
    }

    /// **The canonical symbol of what the operator TYPED, for a venue whose symbols also have NAMES** (polymarket:
    /// `slug:Outcome`, or the market's polymarket.com address, for the outcome's 20-or-more-digit token id). The
    /// resolution is a venue read (Gamma's), and this node opens no venue socket, so a production source asks the
    /// gatewayhub over the account's trade lease; the answer is the venue's one spelling, which the door then
    /// plans as if the operator had typed it (the same facts, the same admission, the same order). `Err` is the
    /// reason, in an operator's words: a slug Gamma does not list, a market with several outcomes (the refusal
    /// lists the spellings that name one), a closed market. The door asks it only for [`VenueSymbolSpelling::OutcomeToken`],
    /// and only for text that is not already a token id. The default answers the text itself: a source that
    /// resolves nothing leaves the canonical spelling to refuse it.
    fn resolve(&self, _route_key: &str, _venue: &str, typed: &str) -> Result<String, String> {
        Ok(typed.to_string())
    }

    /// **`venue`'s KEPT rows**, the list a typed symbol's user name is resolved through (decision 0290), `None` when
    /// none is kept. The default is this process's read of the datahub's lists ([`door_kept_rows`]), which every
    /// production source answers with; a test source holds its own.
    fn kept_rows(&self, venue: &str) -> Option<Arc<Vec<Instrument>>> {
        door_kept_rows(venue)
    }

    /// The admission that puts `symbol` (already canonical for `venue`) into the engine addressed by
    /// `route_key`: its `grid` from the venue's own instrument row and its contract value (`None` for
    /// the default). `Err` is the reason, in words an operator reads verbatim: an unknown symbol, an
    /// unreachable venue, an adapter that answers nothing; a refusal of the gatewayhub's carries its CODE
    /// ([`SymbolPropertiesRequestError::code`]), which is what a caller decides by, never the words.
    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError>;

    /// The latest price this node holds for `symbol`. With `subscribe`, the first ask starts the
    /// subscription that delivers one (an admitted symbol has no feed until then); WITHOUT it the ask
    /// is a read and starts nothing (the Preview surfaces: a control peer must not be able to open
    /// price subscriptions by previewing). `Ok(None)` is "none yet"; `Err` is the reason no price can
    /// come. Never holds a lock across a network call, so a slow lookup does not queue other orders.
    fn price(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
        subscribe: bool,
    ) -> Result<Option<f64>, String>;

    /// **When the price [`Self::price`] answered as `px` was MADE**, epoch ms on this node's clock: the venue's own
    /// time where its lane states one, else when the price reached this node. `None` when the source does not know,
    /// or holds another price for `symbol` now (a newer tick is never paired with an older price). A READ that
    /// starts nothing. An admission carries it as its first mark's time (`SymbolAdmission::initial_mark_ms`); the
    /// default knows nothing, and the engine then stamps that mark when the admission folds.
    fn price_made_ms(&self, _venue: &str, _symbol: &str, _price: f64) -> Option<i64> {
        None
    }

    /// **The best bid and best ask the venue last stated for `symbol`**, as `(bid, ask)`, or `None` when this node
    /// holds no real top of book for it (no two-sided quote or book arrived lately, or only trade prints did: a
    /// mid or a last price is never turned into one). A READ that starts nothing, like [`Self::price`] without
    /// `subscribe`. The door's post-only check meets a limit price with it
    /// (`crates/vike-tradehub/src/server/post_only_door.rs`'s `post_only_cross_refusal`); `None` leaves the order
    /// to the venue's own post-only guard. The default is a source that holds no book.
    fn top_of_book(&self, _route_key: &str, _venue: &str, _symbol: &str) -> Option<(f64, f64)> {
        None
    }

    /// **The last trade print the venue's lane delivered for `symbol`**, or `None` when none arrived lately. A READ
    /// that starts nothing. A `Price` alert compares against it where the core holds no bar series of the pair
    /// (`crate::alerts`). The default is a source that holds no lane.
    fn last_trade(&self, _venue: &str, _symbol: &str) -> Option<f64> {
        None
    }

    /// **Open the bar lane of each `(venue, symbol, interval)` in `wanted` the node does not hear yet**, the way a
    /// mount the boot did not wire gets its candles (decision 0245): a `Price` alert asks it for its pair, so the
    /// alert reads the bar close the chart draws (`crate::alerts`). Idempotent, never blocks. The default opens nothing.
    fn keep_bars(&self, _wanted: &[vike_execution::SeriesKey]) {}

    /// **The grid this source already HOLDS for `symbol`, asking nobody**: what the account's start stated for the
    /// symbol it mounted, or a facts answer still in the cache. The door and the Previews judge an order on a symbol
    /// the engine already trades with it ([`held_grid_refusal`], [`rounding_note`]). `None` refuses nothing and says
    /// nothing: asking here would put a venue round trip (on ibkr a rationed connect) in front of every order on a
    /// mounted symbol. The default holds nothing. ⚠ A wrapper must forward this verb like every other.
    fn held_grid(&self, _route_key: &str, _symbol: &str) -> Option<vike_model::SymbolGrid> {
        None
    }

    /// The door sent `AdmitSymbol` for `symbol` (or the boot did): the source may now keep its price
    /// subscription for good and refresh its facts later ([`Self::refresh`]).
    fn admitted(&self, _route_key: &str, _venue: &str, _symbol: &str) {}

    /// **The door sent the engine a statement that GIVES `symbol` BACK** (`SymbolAdmission::release`, decision
    /// 0205), and the source is to watch for the engine to let go of it and forget everything it kept for it: the
    /// facts, the price lane, the idle stamps and the gatewayhub's reconcile read (`TradeLeaseRegistry::settle_releases`).
    /// The engine decides (it refuses a symbol it still holds a position or an order in), so nothing is dropped
    /// here and now; a source that finds the symbol still listed after a wait drops the request instead. The default
    /// is a source that kept nothing.
    fn release_requested(&self, _route_key: &str, _venue: &str, _symbol: &str) {}

    /// The door gave up on `symbol` BEFORE admitting it (no price came): drop the price subscription
    /// [`Self::price`] opened for it. A no-op for a symbol that was admitted meanwhile.
    fn release(&self, _route_key: &str, _venue: &str, _symbol: &str) {}

    /// **An order names the engine's OWN symbol** (decision 0197): keep that symbol's price lane open while the
    /// engine holds a position or an order in it. The engine's own symbol is mounted, so the door asks no price
    /// for it ([`plan_submit_symbol`] passes it), and nothing else subscribes it unless a strategy is mounted
    /// there. A source that holds the lane opens it off the caller's thread: this call NEVER WAITS for a price,
    /// and never refuses (a limit order a closed market accepts must stay accepted; an opening market order's wait
    /// for a first price is the plan's, [`own_first_price`]). Called only for a committing surface, a `demo`/`live`
    /// engine and an order that names the symbol; idempotent. The default is a source that holds no lanes.
    fn hold_lane(&self, _route_key: &str, _venue: &str, _symbol: &str) {}

    /// **Whether a `demo` account of `venue` runs here with an open lease**: the DEMO LOGIN a `paper` account of a
    /// venue priced through it (`crate::feeds::paper_quotes_through_demo`) is priced and filled by (the owner's ruling
    /// of 2026-11-16). With none, every order of such a paper account is refused in [`no_demo_login`]'s words. The
    /// default is a source that does not know, which does not block.
    fn demo_login(&self, _venue: &str) -> bool {
        true
    }

    /// **Whether a bar series of `symbol` on `venue` reaches this node**: a boot strategy mount's feed, or a bar lane
    /// opened for a mount since (the bar keeper). A `paper` account fills on its symbol's closed bars, so one no series
    /// reaches is refused in [`paper_without_bars`]' words instead of resting silent. The default is a source that
    /// does not know, which does not block.
    fn bar_series(&self, _venue: &str, _symbol: &str) -> bool {
        true
    }

    /// **Whether a strategy mount of this boot prices `symbol` of `venue`**: its feed marks the engine, on a lane
    /// the door's tap may never see (a mount that runs on candle closes alone), so no hold for a first price applies
    /// ([`own_first_price`]). The default is a source that knows of no mount. ⚠ A wrapper must forward this verb
    /// like every other.
    fn priced_by_mount(&self, _venue: &str, _symbol: &str) -> bool {
        false
    }

    /// A FRESH admission for a symbol this source admitted earlier, when what it holds is older than
    /// its refresh interval (a venue changes a tick or lot rarely, and announces it): the door sends
    /// it as an ordinary `AdmitSymbol` refresh ahead of the order. `None` when nothing is due or the
    /// lookup failed (the engine keeps the grid it has).
    fn refresh(&self, _route_key: &str, _venue: &str, _symbol: &str) -> Option<SymbolAdmission> {
        None
    }

    /// **A fresh RATE for the engine's OWN symbol, when the engine books it through one and the rate has aged**
    /// (decision 0183, the mounted-symbol amendment): an order that names the symbol the engine was started on
    /// (declared quoted in another currency than the ledger's, `foreign_mount_money`) rides an ordinary
    /// `AdmitSymbol` of it ahead of the order, so a rate that stopped being renewed is renewed at the door
    /// instead of denying the order `quote-rate-stale`. `engine` is the engine's published block, whose
    /// `quote_rates` row for the mount's currency says how old the engine's rate is. `None` when the engine
    /// books its own symbol at par (every account but a non-USD one on a USD-quoted mount), the rate is fresh, or
    /// the lookup failed (the engine then fails closed by its own staleness lane, which a covered reduce passes).
    /// A look at the facts, never a commit: the symbol is not an admission of the door (no slot, no price lane, the
    /// daemon is told nothing). Called only by a committing surface. The default is a source that rates nothing.
    fn own_symbol_rate(
        &self,
        _route_key: &str,
        _venue: &str,
        _symbol: &str,
        _engine: &VenueBlock,
    ) -> Option<SymbolAdmission> {
        None
    }

    /// Whether the door may hold an order for a price ([`await_price`]). Every production source holds
    /// (the default); a source that answers `false` gets one look and a prompt "no price yet, retry"
    /// instead, which the door's tests use to skip the hold.
    fn waits(&self) -> bool {
        true
    }

    /// **The price lane's own verdict on `symbol`, when it has one and it is not "no quote yet"**: the venue's
    /// stream for the symbol is in a GAP (the venue refused the subscription, or dropped the connection and the
    /// bridge has not got it back), in an operator's words. The door asks it when its hold for a first price
    /// ran out ([`await_price`]) and says it instead of "retry in a moment": a retry cannot help while the
    /// stream is down, and an asynchronous refusal after the subscribe call (IG's `REQERR`) reaches the door
    /// only as this gap. `None` is "nothing to add": the lane is up (or this source cannot tell) and the first
    /// quote simply has not come. The default is a source that cannot tell.
    fn lane_verdict(&self, _route_key: &str, _venue: &str, _symbol: &str) -> Option<String> {
        None
    }

    /// **Tell the daemon again that this node admitted the symbol** and SAY whether it took: what
    /// [`Self::admitted`] does for the daemon, with the commit's outcome returned instead of warned
    /// about. Idempotent, and it sends the engine nothing. The boot's retry calls it for a symbol whose
    /// admission was sent but whose commit may have stalled, and counts the symbol healed only on `Ok`.
    /// The default is a source with no daemon to tell.
    fn tell_daemon(&self, route_key: &str, venue: &str, symbol: &str) -> Result<(), String> {
        self.admitted(route_key, venue, symbol);
        Ok(())
    }

    /// **The leverage the VENUE was SEEN holding for `symbol`** when this source last committed the
    /// admission to the daemon ([`Self::admitted`], [`Self::tell_daemon`]) and the daemon made the venue
    /// hold the account's figure for it (decision 0182's per-symbol twin: bybit, okx, binance). Only a
    /// COMMIT can bring one: a look ([`Self::admission`]) writes nothing to an exchange. `None` is
    /// "nothing confirmed" (a spot symbol, a refusal at the venue, no figure asked, an older daemon), and
    /// the symbol then stays at the engine's own rate, exactly as before. The default is a source that
    /// confirms nothing.
    fn confirmed_leverage(&self, _route_key: &str, _symbol: &str) -> Option<f64> {
        None
    }
}

/// **What a source knows of an ACCOUNT, by its route key**: its exposure ceiling, its wait for the venue balance, what
/// its lane twin is, which accounts are configured, its engine's generation, the balance held back from it and why its
/// last start was refused.
pub trait AccountReadiness: Send + Sync {
    /// Whether the account `route_key` runs with an ACCOUNT-wide exposure ceiling armed
    /// (`policy.max_account_exposure`, or the account's own `max_exposure` row). `max_total_exposure`
    /// bounds ONE symbol, so a live account holding seventeen would carry seventeen times it; the door
    /// admits a symbol into a `live` account only when this is `true`. The default answers `true`: a
    /// source that does not know does not block (the production source answers from the settings).
    fn exposure_cap_armed(&self, _route_key: &str) -> bool {
        true
    }

    /// **Whether the account `route_key` is still waiting for its venue balance** (decisions 0169 and 0246):
    /// `Some(how long since the start, and why the leg's pass has not answered)` while its engine judges
    /// orders on the seed cash instead of what the venue holds, `None` once the balance landed and for every
    /// account that never waits (a `paper` account, a venue with no reconcile leg). It covers the boot's
    /// engines and a hot start's alike; the start waits a bounded time for the answer first, so `Some` here
    /// is the account the venue did not answer in time. The door refuses an OPENING order for such an
    /// account ([`super::refusal::awaiting_balance_refusal`]). The default is `None`: a source that does not
    /// know holds nothing.
    fn awaiting_balance(&self, _route_key: &str) -> Option<BalanceWait> {
        None
    }

    /// The account's ENGINE GENERATION: moves whenever the node starts or stops the account's engine.
    /// Work done on an engine's behalf later than the engine's own start (the boot's retry of the
    /// symbols it missed) captures it and ends when it moves. A source that never sees an engine
    /// change answers a constant. REQUIRED, no default: an implementor that forgot it would silently
    /// switch the retry's guard off.
    fn generation(&self, route_key: &str) -> u64;

    /// **Whether the engine under `route_key` is an operator's REAL account that spells a lane twin's label**
    /// (`vike_venues::lane_twin`), as the account watcher last read the rows. The published block cannot say:
    /// a real `PERP` account mounted on `BTCUSDT.P` has the derived twin's label, symbol and (at the demo tier)
    /// tier, and `derive_lane_twins` stands down for it. The door must not treat such an engine as the twin
    /// ([`lane_addressed`] would send the default account's perpetual orders to another account's keys). The
    /// default is `false`: a source that does not know leaves the judgement to the block alone.
    fn real_account_under_twin_label(&self, _route_key: &str) -> bool {
        false
    }

    /// **Whether the engine under `route_key` is a lane twin this node DERIVED and runs**, as the account
    /// watcher last said (`crate::account_follow`): the POSITIVE evidence a `live` engine needs before the door
    /// treats it as the twin ([`lane_addressed`] rewrites the default account's perpetual orders onto it). The
    /// published block cannot say it, and "no real account was reported under the label" is an absence: at
    /// `live` an older real `PERP` account mounted on `BTCUSDT.P` has the derived twin's label, symbol and tier
    /// to the letter, and its orders would fill on a DIFFERENT real account's keys. The demo tier keeps the
    /// absence rule (it always had it). The default is `false`: a source that does not know never makes a
    /// `live` engine the twin.
    fn derived_lane_twin(&self, _route_key: &str) -> bool {
        false
    }

    /// **The route key of every account CONFIGURED for `venue`, running or not**, as the account watcher last read
    /// the rows (decision 0254; a derived lane twin is no row, so it is none), or why the rows could not be read.
    /// `None`: this source follows no rows, and the door counts what the node itself lists.
    fn configured_accounts(&self, _venue: &str) -> Option<Result<Vec<String>, String>> {
        None
    }

    /// **Whether the lane twin `route_key` is kept up only to be CLOSED**: its derived row is gone (the margin
    /// leverage is no longer stated) while it still holds a position or an order, so the watcher does not stop
    /// it ([`crate::account_follow`]'s `follow_lane_twins`). It takes no new position: the door refuses an
    /// opening order for it ([`super::refusal::lane_twin_closing_refusal`]) and lets every reducing one through.
    /// The default is `false`.
    fn lane_twin_closing_only(&self, _route_key: &str) -> bool {
        false
    }

    /// **The accounts whose venue balance this node HOLDS BACK from their engine** (the node-side balance gate,
    /// `venue_backend::gateway::balance_gate`), as `(route key, short reason)` in route-key order: the reason is the
    /// words an operator surface shows (`assumed USD`, `stated EUR not anchored`, `untold foreign stock`).
    /// The publisher stamps it on every frame ([`vike_tradehub_client::wire::CoreSnapshotWire::balance_withheld`]). The
    /// default is none: a source with no reconcile gate holds nothing.
    fn balance_held(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// **Why the last start of the account `route_key` was refused**, in the refusal's own words: the cause behind the
    /// one word `adapter failed` ([`vike_tradehub_client::wire::ArmingNotRunningViewWire::detail`]). The default is none.
    /// ⚠ A wrapper must forward this verb like every other.
    fn start_refusal(&self, _route_key: &str) -> Option<String> {
        None
    }
}

/// **The node's links, as the publisher stamps them on every frame**: the accounts whose venue path is down or up, the
/// trade links to the gatewayhub and the price lanes to the datahub that are lost, and the price dials that hold a
/// session. A READING: no order, cancel or halt follows from it.
pub trait LinkHealth: Send + Sync {
    /// **The accounts whose VENUE PATH is down right now**, as the gatewayhub's status report last said it
    /// (`TradeStatusReport::venue_link`, polled by the lease's worker: `venue_backend::gateway::reteach`), in route-key
    /// order. The publisher stamps it on every frame
    /// ([`vike_tradehub_client::wire::CoreSnapshotWire::venue_link_down`]) and `trade health`'s engine row reads it. The
    /// default is none: a source with no lease reports no path. ⚠ A wrapper must forward this verb like every
    /// other, or the production source answers the default.
    fn venue_link_down(&self) -> Vec<VenueLinkDownWire> {
        Vec::new()
    }

    /// **The accounts whose venue path the gatewayhub said is up, on a fresh answer**, in route-key order
    /// ([`vike_tradehub_client::wire::CoreSnapshotWire::venue_path_up`]). The default is none: not known, never down.
    /// ⚠ A wrapper must forward this verb like every other.
    fn venue_path_up(&self) -> Vec<String> {
        Vec::new()
    }

    /// **How many price dials hold a session with the datahub now**
    /// ([`vike_tradehub_client::wire::CoreSnapshotWire::price_dials_up`]). The default is `None`: not said, never zero.
    /// ⚠ A wrapper must forward this verb like every other.
    fn price_dials_up(&self) -> Option<u32> {
        None
    }

    /// **The open leases whose trade link to the gatewayhub is not up** (redialling, or closed for good), in
    /// route-key order. The publisher stamps it on every frame, for the accounts that run
    /// ([`vike_tradehub_client::wire::CoreSnapshotWire::trade_link_down`]). The default is none: a source with no lease
    /// has no link to lose. ⚠ A wrapper must forward this verb like every other.
    fn trade_link_down(&self) -> Vec<TradeLinkDownWire> {
        Vec::new()
    }

    /// **The price feeds whose strategy lane to the datahub is lost**, in venue order
    /// ([`vike_tradehub_client::wire::CoreSnapshotWire::price_link_down`]). The default is none: a source with no feed
    /// has no lane to lose. ⚠ A wrapper must forward this verb like every other.
    fn price_link_down(&self) -> Vec<PriceLinkDownWire> {
        Vec::new()
    }
}

/// **An operator's READ of an account at its venue, over the account's open trade lease** (`Request::VenueRead`,
/// `crate::server::venue_read`): the read, and the wire ids that let it name our own orders by our ids.
pub trait VenueReads: Send + Sync {
    /// **An operator's READ of the account `route_key` at its venue** (`Request::VenueRead`): its balance, or the
    /// venue's own list of its open orders, asked over the account's OPEN trade lease. Blocking (a venue round
    /// trip), so it runs on a connection thread. `Err` is the operator's sentence: no lease (the account is not
    /// running here), the bridge offers no such read, asked too soon, the venue's refusal, under the bridge's id
    /// when it declares one. It writes nothing at the venue and folds nothing into any book. The default is a
    /// source with no lease to ask through.
    /// ⚠ A wrapper must forward this verb like every other, or the production source answers the default.
    fn venue_read(
        &self,
        route_key: &str,
        _kind: VenueReadKind,
    ) -> Result<VenueRead, CarriedRefusal> {
        Err(format!(
            "{route_key}: this node holds no trade lease registry (it is not running behind a gatewayhub), \
             so there is no path to the venue to read"
        )
        .into())
    }

    /// **`(order client id, wire id)` for each of `order_client_ids` the venue of `route_key` names by an id that is not the order client id**
    /// (hyperliquid's cloid), asked over the account's open lease (`TradeUp::WireIds`): what lets a venue read name
    /// our own orders by our order client id (`crate::server::venue_read`). The default is none: a source with no lease
    /// translates nothing. ⚠ A wrapper must forward this verb like every other.
    fn wire_ids(
        &self,
        _route_key: &str,
        _order_client_ids: &[String],
    ) -> Result<Vec<(String, String)>, String> {
        Ok(Vec::new())
    }
}

/// **The one source behind the control door, a field per trait** (production: the gatewayhub backend, `TradeLeaseRegistry`,
/// one `Arc` cast to each by [`Self::of`]): each holder keeps the faces it reads. A `None` field is a node with no
/// such source (no gatewayhub lease to ask).
#[derive(Clone, Default)]
pub struct SymbolSources {
    /// The door's facts, prices and admissions (and, through it, its accounts' readiness).
    pub admitter: Option<Arc<dyn SymbolAdmitter>>,
    /// What the publisher stamps of each account (`balance_withheld`, `start_refusal`).
    pub readiness: Option<Arc<dyn AccountReadiness>>,
    /// What the publisher stamps of the node's links.
    pub link_health: Option<Arc<dyn LinkHealth>>,
    /// An operator's venue reads.
    pub venue_reads: Option<Arc<dyn VenueReads>>,
}

impl SymbolSources {
    /// One source that is all four traits, cast to each.
    pub fn of<S: SymbolAdmitter + LinkHealth + VenueReads + 'static>(source: Arc<S>) -> Self {
        SymbolSources {
            admitter: Some(source.clone()),
            readiness: Some(source.clone()),
            link_health: Some(source.clone()),
            venue_reads: Some(source),
        }
    }
}

/// **The admission to send AFTER `admission` when the daemon's commit confirmed a leverage for its
/// symbol**: `admission` restated with `initial_margin_requirement = 1 / leverage` (the symbol's own margin rate,
/// which the engine accepts on a refresh and writes to its per-symbol row), or `None` when the source
/// confirmed nothing, the figure is not a leverage (below 1x or not finite: the engine would refuse the
/// whole admission with `bad-im-requirement`), or `admission` already says that rate. The caller sends
/// it ahead of the order it is judging, on the lane the first admission rode.
pub(crate) fn with_confirmed_leverage(
    source: &dyn SymbolAdmitter,
    admission: &SymbolAdmission,
) -> Option<SymbolAdmission> {
    let leverage = source.confirmed_leverage(&admission.route_key, &admission.symbol)?;
    if !(leverage.is_finite() && leverage >= 1.0) {
        return None;
    }
    let initial_margin = 1.0 / leverage;
    (admission.initial_margin_requirement != Some(initial_margin)).then(|| SymbolAdmission {
        initial_margin_requirement: Some(initial_margin),
        ..admission.clone()
    })
}

/// **Whether `rule` is a venue whose engine is mounted on NO symbol by design**: polymarket's account-wide mount
/// (`vike_venues::venue_wiring::EngineMount::Account`; exec, the fill pump and reconcile key off the wallet, not a market). For every
/// other venue an engine block with an empty symbol means "this node did not say what it trades" and the planner
/// leaves the order alone; for this one it means "every token is foreign", so each is admitted first.
fn account_wide(rule: VenueSymbolRule) -> bool {
    matches!(rule, VenueSymbolRule::Admits(VenueSymbolSpelling::OutcomeToken))
}

/// The venues the door admits symbols for (the `Admits` rows): the root hands them to the mount as
/// `AdapterPolicy::fail_closed_symbol_venues`, so exactly these engines deny a symbol they were never
/// told about.
pub fn admitting_venues() -> Vec<String> {
    SYMBOL_RULES
        .iter()
        .filter(|(_, rule)| matches!(rule, VenueSymbolRule::Admits(_)))
        .map(|(venue, _)| (*venue).to_string())
        .collect()
}

/// **Why this account may NOT hold a symbol quoted in a currency other than its own, or `None`** (decision
/// 0183, phase 1): the venue must have a rate source (its `vike_venues::venue_currency` row's `quote_conversion`;
/// the engine's assembly reads the same row) and the account must be on its practice tier with an account-wide
/// exposure ceiling armed (`vike_tradehub_core::quote_conversion_tier_refusal`, the half the assembly reads too). The
/// tier gate IS the switch: there is no setting and no environment variable beside it. The sentence lists the
/// rate-source venues in the table's order.
pub(crate) fn quote_conversion_refusal(
    venue: &str,
    tier: Option<AccountTier>,
    account_ceiling_armed: bool,
) -> Option<String> {
    if venue_currency_for(venue).quote_conversion == VenueQuoteConversion::None {
        let converting: Vec<&str> = VENUE_CURRENCY
            .iter()
            .filter(|row| row.quote_conversion != VenueQuoteConversion::None)
            .map(|row| row.venue)
            .collect();
        return Some(format!(
            "this node converts a quote currency for the practice accounts of {} only, and {venue} \
             has no rate source: only instruments quoted in the account's own currency are admitted",
            converting.join(", ")
        ));
    }
    vike_tradehub_core::quote_conversion_tier_refusal(tier, account_ceiling_armed)
}

/// **Whether a lease of `venue` at `tier` asks the gatewayhub to convert** (`TradeLease::convert_quotes`):
/// the venue and tier the rule names (the ceiling half is judged where the facts land), and for ibkr only when
/// the account has an exposure ceiling armed (`ceiling`: the opt-in, so an account that never converts takes the
/// old plain path and costs what it always cost), the venue STATED the account's currency and the engine's
/// mounted contract counts money in it ([`unanchorable_mount`]). An ibkr gatewayhub answers every stock of a
/// converting lease against the account's base currency, so a lease whose ledger cannot be anchored to it asks
/// the old way, which fences stocks to the mounted contract's own currency and changes nothing for an account
/// that never converted. ig takes the same gate for the same reason: a converting ig lease costs the venue a
/// login, `GET /accounts` and one to three market reads per epic it is asked about, and the node refuses every
/// such answer for an account whose currency it was not told or whose ceiling is not armed, so the work is
/// asked for only when it can be used. oanda is unchanged: its rate is the venue's own statement, free of any
/// budget. Which venues' currency read is budgeted is their row's `currency_read` (`vike_venues::venue_currency`).
pub(crate) fn quote_lease_converts(
    venue: &str,
    tier: AccountTier,
    anchored: bool,
    ceiling: bool,
) -> bool {
    quote_conversion_refusal(venue, Some(tier), true).is_none()
        && (venue_currency_for(venue).currency_read == VenueCurrencyRead::Free
            || (anchored && ceiling))
}

/// **Whether the start of a lease of `venue` at `tier` asks the gatewayhub to READ the account's currency**
/// (`AdapterStart::read_account_currency`): only for a venue whose statement of the currency costs the venue a budget
/// (its row's `currency_read` is `Budgeted`: ibkr, a dial of the data lane, rationed by the hour; ig, a login and a read
/// of the account allowance), and only for an account the node will convert for (the rule's venue and tier, with an
/// exposure ceiling armed). An ibkr or ig account without a ceiling reads nothing at its start, as before the
/// conversion existed.
pub(crate) fn quote_lease_reads_currency(venue: &str, tier: AccountTier, ceiling: bool) -> bool {
    venue_currency_for(venue).currency_read == VenueCurrencyRead::Budgeted
        && account_converts(venue, tier, ceiling)
}

/// **Whether the account converts a quote currency TODAY**: the venue and tier the rule names, with an exposure
/// ceiling armed (the opt-in). The one question behind both the budgeted read of the currency
/// ([`quote_lease_reads_currency`]) and the journal's fallback anchor ([`anchor_with_journal`]).
pub(crate) fn account_converts(venue: &str, tier: AccountTier, ceiling: bool) -> bool {
    ceiling && quote_conversion_refusal(venue, Some(tier), true).is_none()
}

/// **The currency a lease is anchored to when the gatewayhub could not state one at its start**, or the one it
/// stated. `stated` is the gatewayhub's word; `journaled` is the currency the previous run's ledger was anchored
/// to for this account (the fx table of the engine snapshot the journal restores: present exactly when the engine
/// held a symbol quoted in another currency); `converts` is [`account_converts`]. A stated currency always wins.
///
/// THE RULE for the journal's (every converting venue: ibkr, ctrader, dukascopy, ig, oanda, fxcm). It is used only
/// when (1) the account converts today, so a re-pointed account that no longer does is never anchored from the
/// past; and (2) the venue stated none; the assembly's mount fence then judges it like a stated one too
/// (`unanchorable_mount` of the engine and of the door).
/// WHY it is used at all: the restored engine's rates are expressed in it, and the core restores a default
/// account's journaled engine only when its ledger label equals the journaled one (a snapshot with no route key is
/// matched on `quote_asset`), so an engine assembled unanchored after one missed read at a restart would be skipped,
/// flat while the venue holds the position.
/// WHAT bounds a stale one (the owner re-points the account to another currency): a venue whose rates state the
/// ledger they are expressed in (ibkr's, `SymbolPropertiesWire::account_currency`) has every rate checked against the anchor
/// by the door; for a venue whose rates do not, the anchor is UNCONFIRMED (`TradeLeaseRegistry::anchor_unconfirmed`) and the
/// door admits no symbol quoted in another currency on it, until the account is started again and states its own.
pub(crate) fn anchor_with_journal(
    converts: bool,
    stated: Option<String>,
    journaled: Option<&str>,
) -> Option<String> {
    if stated.is_some() || !converts {
        return stated;
    }
    journaled.and_then(vike_model::currency_code)
}

/// **The currency a lease is anchored to, and whether it is only ASSUMED**: the VENUE's word, else the JOURNAL's
/// ([`anchor_with_journal`], under its fences), else [`vike_model::DEFAULT_CURRENCY`] (owner ruling 2026-10-19, repeated:
/// "the account currency is what the venue API states; if the API does not state it, or the read fails: USD. Never
/// refuse because a currency is unknown"). `stated_assumed` is the lane's own flag on `stated` (the gatewayhub
/// fills in USD itself when a venue is silent, fxcm's and now every converting venue's, and says so): that is the
/// venue's SILENCE, so it ranks below the journal and travels flagged when nothing outranks it. The default is
/// applied only to an account that converts today (`converts`, [`account_converts`]): a `live` or `paper` account,
/// an account with no exposure ceiling and a venue with no rate source are left as they were, so nothing opens
/// there. A blank statement is no statement. The assembly's mount fence then judges a defaulted currency like a
/// stated one (`unanchorable_mount`). A journal that names a money other than USD is evidence the account is not
/// a dollar one, so it is the anchor and nothing is defaulted over it (ig's journaled pound or euro included: ig
/// no longer refuses a non-USD account, it converts for it like every other venue). A journal that names the
/// DEFAULT while the venue says nothing only records the previous run's assumption, so it is returned ASSUMED too
/// (it trades like the first run).
pub(crate) fn anchor_with_journal_or_usd(
    converts: bool,
    stated: Option<String>,
    stated_assumed: bool,
    journaled: Option<&str>,
) -> Option<(String, bool)> {
    let stated = stated.filter(|currency| !currency.trim().is_empty());
    if !converts {
        return stated.map(|currency| (currency, stated_assumed));
    }
    // The venue's OWN word. A lane's assumption (`stated_assumed`) is the venue's SILENCE that the gatewayhub filled
    // in: it ranks below the journal (venue > journal > USD assumed, whoever assumed it).
    let venue_word = stated.clone().filter(|_| !stated_assumed);
    let venue_silent = venue_word.is_none();
    match anchor_with_journal(true, venue_word, journaled) {
        // A journal that names the DEFAULT while the venue says nothing only records the previous run's
        // assumption (the fx table carries the ledger currency exactly when that run held a foreign-quoted
        // symbol): it is the same assumed USD with the same rights and the same warning, not an "unconfirmed"
        // journal anchor that would lock the default out of every run after the first.
        Some(currency) => {
            let recorded_assumption =
                venue_silent && vike_model::same_currency(&currency, vike_model::DEFAULT_CURRENCY);
            Some((currency, recorded_assumption))
        }
        None => Some((stated.unwrap_or_else(|| vike_model::DEFAULT_CURRENCY.to_string()), true)),
    }
}

/// **Whether the anchor of a lease is UNCONFIRMED**: nothing the venue said stands behind it, and no rate of the
/// venue states its ledger to check it against, so the door admits no symbol quoted in another currency on it. That
/// is the JOURNAL's anchor (the previous run's ledger, used because the read missed at this start: the owner may
/// have re-pointed the account since) for a venue whose rates do not state their ledger (its row's
/// `rate_ledger_stamp` is `Optional`, `vike_venues::venue_currency`: ibkr's gatewayhub stamps every rate and an unstamped
/// one is refused, while oanda's, ctrader's and fxcm's stamp theirs and an older gatewayhub's unstamped answer is still
/// admitted). The venue's own word is confirmed; so is an ASSUMED currency (the owner's ruling of 2026-10-19,
/// repeated: the USD default must TRADE, inside the conversion's own gates, as FXCM's assumed USD does), whose
/// assumption is flagged and warned instead. `venue_stated`: the venue itself named the currency (a lane's own
/// assumption does not count); `assumed`: the bind flags it assumed.
pub(crate) fn anchor_is_unconfirmed(
    venue: &str,
    anchored: bool,
    assumed: bool,
    venue_stated: bool,
) -> bool {
    anchored
        && !venue_stated
        && !assumed
        && venue_currency_for(venue).rate_ledger_stamp == VenueRateLedgerStamp::Optional
}

/// **Whether `money` is a currency the venue may quote in a MINOR unit** (IBKR: London in pence, Tel Aviv in agorot,
/// Johannesburg in cents): a major a contract carries such a listing under (`GBP` for a pence-priced LSE stock, `ILS`,
/// `ZAR`) or the minor code itself (`GBX`, `ILA`, `ZAC`/`ZAc`), read off the row's `minor_units`. The gatewayhub refuses
/// a RATE for the three major units (`vike_ibkr::account::quote_rate`'s `minor_unit_refusal`); here they refuse an
/// ANCHOR.
fn quotes_in_a_minor_unit(minor_units: &[vike_model::MinorUnit], money: &str) -> bool {
    minor_units.iter().any(|unit| {
        vike_model::same_currency(unit.major, money) || vike_model::same_currency(unit.minor, money)
    })
}

/// **Why the engine mounted on `mounted` cannot have its ledger anchored to `currency`, the currency the venue
/// states for the account, or `None`.** Only a venue whose engine counts money in the currency of its MOUNTED contract
/// has a rule of its own (its `vike_venues::venue_currency` row's `engine_money`: ibkr, the cash row its balance seed
/// reads, `ibkr_engine_money`), so a base currency that is another one would label that cash, and value the mounted
/// stock, in a currency it is not in; and a mounted forex pair is a CASH position, whose profit this margin engine does
/// not book (`FOREX_REFUSAL` of the bridge); and a mount whose money is a currency the venue may quote in its minor
/// unit ([`quotes_in_a_minor_unit`]) is not anchored either. Every other venue is judged by the ENGINE's own fence
/// (`vike_tradehub_core::unanchorable_mount`: a mount quoted in another money than `currency`, or whose name says none,
/// is not a `currency` ledger; a venue with no rate source is not judged): ONE decision, so the door never anchors a
/// lease, nor asks a budgeted venue for a rate, on a currency the engine's assembly then declines to be told.
pub(crate) fn unanchorable_mount(venue: &str, mounted: &str, currency: &str) -> Option<String> {
    let row = venue_currency_for(venue);
    if row.engine_money != VenueEngineMoney::MountedContract {
        return vike_tradehub_core::unanchorable_mount(venue, mounted, currency);
    }
    // shortcut: the MountedContract helpers (`ibkr_forex`, `ibkr_engine_money`) read IBKR's kept rows by name; a second
    // MountedContract venue generalises them.
    let rows = door_kept_rows(venue).unwrap_or_default();
    if ibkr_forex(&rows, mounted) {
        return Some(format!(
            "the engine is mounted on the forex pair `{mounted}`, a CASH position whose cash the engine's \
             one-currency balance does not follow (a pair is admitted into an engine mounted on a stock, \
             never mounted), so its ledger is not anchored to the account's currency"
        ));
    }
    match ibkr_engine_money(&rows, mounted) {
        // Whether IBKR prices this mount in pounds or in pence is not measured: anchoring the ledger to the
        // currency would book a pence price as pounds, 100 times too high.
        Some(money) if quotes_in_a_minor_unit(row.minor_units, money) => Some(format!(
            "the engine is mounted on `{mounted}`, which counts its money in {money}, a currency IBKR may \
             quote in its MINOR unit (pence, agorot, cents), and which listings it does so for is not \
             measured: its prices could be booked 100 times too high, so the ledger is not anchored to {currency}"
        )),
        Some(money) if vike_model::same_currency(money, currency) => None,
        Some(money) => Some(format!(
            "the engine is mounted on `{mounted}`, which counts its money in {money}, and the account's \
             base currency is {currency}: the balance the engine reads is the {money} cash, so the ledger is \
             not anchored to {currency}. Mount a {currency} contract to trade other currencies"
        )),
        None => Some(format!(
            "the engine is mounted on `{mounted}`, whose kept row states no currency (or no ibkr kept list holds \
             it), so its ledger is not anchored to the account's currency ({currency})"
        )),
    }
}

/// **The pair a venue's engine is mounted on INSTEAD of `symbol`, so that its ledger can be anchored to `currency`**
/// (the currency the VENUE states for the account), or `None` to keep `symbol`. `pair` is the account's own row: its
/// `currency_symbol` fact (`vike_secrets::AccountFact::CurrencySymbol`; owner, 2026-11-13: what used to be the
/// per-venue `CURRENCY_MOUNTS` table is the account's row), which the caller hands over only for a start on the
/// symbol that same row STATES (a mount somebody chose is their word, and a labelled account's mount is its
/// strategy's).
///
/// The mount fence ([`unanchorable_mount`]) declines a ledger the mounted symbol's money is not, which left FXCM's
/// demo account (GBP, measured 2026-10-20) with an unanchored ledger on `EURUSD`: its balance held back and every
/// foreign-quoted pair refused. An engine is mounted where its venue session was STARTED, and the currency is read by
/// that start, so `VenueBackendGateway::start_adapter` starts the session again on the pair this returns and reports
/// it in `AdapterBind::mount_symbol` (`moved_mount` is its one caller). fxcm's pound account states `EURGBP`: a
/// regular pair quoted in pounds, so its ledger books the mount at par in the account's own money, and the dollar
/// pairs convert at GBPUSD.
///
/// The rule is deliberately narrow, so that every start it does not name is byte-identical to the one before it: the
/// mounted symbol must NOT already carry the ledger (a USD account stays on `EURUSD`), and the stated pair must. An
/// unstated currency never reaches it (the node assumes USD), and an account that states no pair, or one that does not
/// count in the venue's currency, keeps today's behaviour: unanchored ledger, balance held, foreign-quoted pairs
/// refused.
pub(crate) fn currency_mount(
    venue: &str,
    symbol: &str,
    currency: &str,
    pair: Option<&str>,
) -> Option<String> {
    let pair = pair?;
    unanchorable_mount(venue, symbol, currency)?;
    unanchorable_mount(venue, pair, currency).is_none().then(|| pair.to_string())
}

fn rule_of(venue: &str) -> Option<VenueSymbolRule> {
    SYMBOL_RULES.iter().find(|(v, _)| *v == venue).map(|(_, rule)| *rule)
}

/// The door's verdict on a plain Submit's symbol.
#[derive(Debug, PartialEq)]
pub(crate) enum SymbolPlan {
    /// Not this gate's, or already placeable exactly as written: send the order untouched.
    Pass,
    /// Refused: the refusal frame the caller gets (`crate::refusals::SymbolRefusal`, its id and its words).
    Refuse(HubRefusal),
    /// Send the order on `symbol` (the canonical spelling); `admit`, when present, goes first; `wait`,
    /// when present, says the symbol has no price yet and the order is held for one.
    Place { symbol: String, admit: Option<Box<SymbolAdmission>>, wait: Option<PriceWait> },
}

/// Where to ask for the price an opening order is waiting on.
#[derive(Debug, PartialEq)]
pub(crate) struct PriceWait {
    pub(crate) route_key: String,
    pub(crate) venue: String,
}

/// The engine's trade list as the refusal prints it (an engine mounted on no symbol lists only what it admitted;
/// empty when it admitted none).
fn trades_text(engine: &VenueBlock) -> String {
    std::iter::once(&engine.symbol)
        .filter(|s| !s.is_empty())
        .chain(engine.extra_symbols.iter())
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// **`venue`'s kept rows as this process reads them, for a door** (`vike_catalog::kept_list`), read from its file again
/// when the last read found none: a list the datahub has just written counts at the next ask, with no restart, so a
/// symbol refused for want of a list is admitted once the refresh its words name has run.
pub(crate) fn door_kept_rows(venue: &str) -> Option<Arc<Vec<Instrument>>> {
    // This lib's unit tests all read one fixture set of kept lists (`crate::testutil::door_rows`).
    #[cfg(test)]
    crate::testutil::hold_door_lists();
    vike_catalog::kept_list(venue).or_else(|| vike_catalog::kept_list_now(venue))
}

/// The kept row of the user name `symbol` on `venue` among `rows` (`vike_catalog::kept_row_of_user_name`), `None` for
/// none or two: what states a symbol's class, money and settle coin at this door, never its name (decision 0290).
fn row_of<'a>(rows: &'a [Instrument], venue: &str, symbol: &str) -> Option<&'a Instrument> {
    vike_catalog::kept_row_of_user_name(rows, venue, symbol).ok()
}

/// A kept fiat quote (`vike_catalog::kept_fiat_quote`) as this door compares and prints it: the code, a minor
/// unit's own code (`GBX`), `None` when the money is not known, which equals no money.
fn money_word(quote: vike_model::FiatQuote) -> Option<&'static str> {
    match quote {
        vike_model::FiatQuote::Code(code) => Some(code),
        vike_model::FiatQuote::Minor(unit) => Some(unit.minor),
        vike_model::FiatQuote::Unknown => None,
    }
}

/// The money a pair engine (dukascopy, fxcm) counts in, or a pair is quoted in, for a currency pair or a CFD on a
/// currency or metal pair (`EURUSD` is USD, `XAUUSD` USD): its kept row's fiat quote as the engine reads it
/// (`vike_catalog::kept_fiat_quote`; [`money_word`] says it). `None` for any other row or none, which this door's
/// pair gate does not judge.
fn pair_money(rows: &[Instrument], venue: &str, symbol: &str) -> Option<vike_model::FiatQuote> {
    let row = row_of(rows, venue, symbol)?;
    let pair = match &row.contract {
        Contract::Fx(_) => true,
        Contract::Cfd(cfd) => {
            matches!(cfd.underlying_kind, ContractCfdUnderlying::Fx | ContractCfdUnderlying::Metal)
        }
        _ => false,
    };
    if !pair {
        return None;
    }
    vike_catalog::kept_fiat_quote(venue, symbol, Some(row))
}

/// The currency an IBKR engine counts its money in, or a contract is quoted in: the fiat quote its kept row states
/// (decision 0290's O-10: the operator's row holds the exchange and the currency the user name no longer carries;
/// `vike_catalog::kept_fiat_quote`, [`money_word`]), which for the mounted contract is the cash row its balance
/// seed reads (`crates/bridges/vike-ibkr/src/reconcile/socket.rs`'s `parse_socket_balance`). `None` when no row states
/// one.
fn ibkr_engine_money(rows: &[Instrument], symbol: &str) -> Option<&'static str> {
    vike_catalog::kept_fiat_quote("ibkr", symbol, row_of(rows, "ibkr", symbol)).and_then(money_word)
}

/// Whether an IBKR symbol's kept row is a forex pair: a CASH position, not a stock (decision 0183).
fn ibkr_forex(rows: &[Instrument], symbol: &str) -> bool {
    row_of(rows, "ibkr", symbol).is_some_and(|row| row.asset_class() == AssetClass::Fx)
}

/// Whether the deribit engine symbol `symbol` is a PERPETUAL or a DATED future (not an option, a spot pair or a
/// combo), by its kept row's class: the door and the adapter read the row.
fn deribit_mount_is_a_future(rows: &[Instrument], symbol: &str) -> bool {
    row_of(rows, "deribit", symbol).is_some_and(|row| {
        matches!(row.asset_class(), AssetClass::CryptoPerp | AssetClass::CryptoFuture)
    })
}

/// Why a deribit engine cannot hold the instrument `admission` describes, or `None`. An engine counts
/// money ONE way (decision 0170 and `docs/ops/deribit-any-symbol-plan.md`): an instrument whose amount is
/// US dollars (inverse) and one whose amount is the base coin (linear) are valued, margined and settled
/// differently, and the engine's ledger is one currency on one balance mode (decision 0233 says what
/// holding both would take). So the instruments an engine may add are the ones of ITS kind, and an engine
/// mounted on anything but a perpetual or a dated future (an option) adds none. The adapter checks the
/// same rule again, and a linear instrument's settlement currency as well, which the door cannot see. `head` is
/// [`engine_words`]'s.
fn money_model_refusal(
    rows: &[Instrument],
    engine: &VenueBlock,
    admission: &SymbolAdmission,
    head: &str,
) -> Option<SymbolRefusal> {
    if !deribit_mount_is_a_future(rows, &engine.symbol) {
        return Some(SymbolRefusal::DeribitMountNotAFuture {
            engine: head.to_string(),
            mounted: engine.symbol.clone(),
        });
    }
    use vike_model::NotionalKind::{Inverse, Linear};
    let mounted = engine.contract_value_of(&engine.symbol).kind;
    let theirs = admission.contract_value.unwrap_or_default().kind;
    // An inverse contract of another COIN is [`settle_coin_refusal`]'s, the rule every venue shares.
    let word = |k| match k {
        Inverse => "INVERSE (its amount counts US dollars)",
        Linear => "LINEAR (its amount counts the base coin)",
    };
    (mounted != theirs).then(|| SymbolRefusal::DeribitMoneyModel {
        engine: head.to_string(),
        symbol: admission.symbol.clone(),
        theirs: word(theirs),
        mounted: engine.symbol.clone(),
        ours: word(mounted),
        kind: theirs.word(),
    })
}

/// **ONE SETTLE COIN per engine** (decision 0258, item 7; the owner's ruling of 2026-11-22 includes LINEAR engines),
/// for every venue: an engine keeps its books in its mounted contract's settle coin, and the venue keeps one wallet per
/// coin, so a contract of the same kind that settles in ANOTHER coin is refused, naming both contracts and both coins
/// (`BTCUSDC.P` into an engine mounted on `BTCUSDT.P`, `ETHUSD.P` into one on `BTCUSD.P`). The rule is
/// `vike_model::MoneyModel::admits`, both models read off the KEPT rows (`MoneyModel::of_contract`: a user name such as
/// `BTCUSD.P` states no coin on deribit, binance or okx, decision 0290). `None` when either row is missing or names no
/// settle coin (a spot pair), when the kinds differ (the kind fence below judges that, on the engine's own grid), or
/// when the coins agree. The bridges hold it again (deribit's adapter, binance's and bybit's `coin_fence`). `head` is
/// [`engine_words`]'s.
fn settle_coin_refusal(
    rows: &[Instrument],
    venue: &str,
    engine: &VenueBlock,
    admission: &SymbolAdmission,
    head: &str,
) -> Option<SymbolRefusal> {
    let model =
        |symbol: &str| vike_model::MoneyModel::of_contract(&row_of(rows, venue, symbol)?.contract);
    let (ours, other) = (model(&engine.symbol)?, model(&admission.symbol)?);
    (ours.admits(&other) == Err(vike_model::MoneyModelRefusal::SettleCoin)).then(|| {
        SymbolRefusal::SettleCoin {
            engine: head.to_string(),
            symbol: admission.symbol.clone(),
            other: other.settle.clone(),
            mounted: engine.symbol.clone(),
            ours: ours.settle.clone(),
        }
    })
}

/// Why `engine` cannot hold the symbol `admission` describes because its contract KIND is not the engine's, or
/// `None` (decision 0233, item 5, for every venue; bybit's coin-margined contracts are what reached it, decision 0231).
///
/// `vike_execution::Account::kind_is_unknown_for` DROPS a fill (and says so at `error!`) on a symbol with no row in the
/// contract-value grid once the account trades any INVERSE symbol, and the grid lists only values that differ from
/// the default linear one. So an inverse symbol admitted into a linear engine makes EVERY linear symbol of the
/// engine unknown, its mounted one included, and a linear symbol admitted into an inverse engine is unknown from
/// the first fill: the venue fills, the node books nothing. Keeping both kinds in one engine changes that per-fill
/// call (a the latency box latency A/B is owed), so the door refuses the symbol instead, on the cold path.
///
/// The engine's kind is its MOUNTED symbol's (absent from the grid is linear); a symbol's is the facts' (none is
/// linear), so a spot pair, a dated linear future and a multiplier contract are all linear. A symbol the engine
/// already trades never gets here (`already_trading`), so a restored admission is not judged again. `head` is
/// [`engine_words`]'s.
fn contract_kind_refusal(
    venue: &str,
    engine: &VenueBlock,
    admission: &SymbolAdmission,
    head: &str,
) -> Option<SymbolRefusal> {
    use vike_model::NotionalKind::{Inverse, Linear};
    let mounted = engine.contract_value_of(&engine.symbol).kind;
    let theirs = admission.contract_value.unwrap_or_default().kind;
    if mounted == theirs {
        return None;
    }
    let word = |k| match k {
        Inverse => "INVERSE (a coin-margined contract: its amount counts US dollars)",
        Linear => "LINEAR (its amount counts the base coin)",
    };
    let (kind, drops) = match theirs {
        Inverse => (
            "an inverse",
            format!(
                "the fills of every linear symbol it trades, its mounted `{}` included",
                engine.symbol
            ),
        ),
        Linear => ("a linear", format!("the fills of `{}`", admission.symbol)),
    };
    Some(SymbolRefusal::ContractKind {
        engine: head.to_string(),
        symbol: admission.symbol.clone(),
        theirs: word(theirs),
        venue: venue.to_string(),
        mounted: engine.symbol.clone(),
        ours: word(mounted),
        drops,
        kind,
    })
}

/// Whether `engine`, published under `key`, is `venue`'s LANE TWIN: the second engine over the default
/// account's row, for the exec lane its default engine does not trade (`vike_venues::lane_twin`).
/// Judged by the table's label, its mount symbol AND the tiers it is derived at (`AccountRowLaneTwin::tiers`: demo and,
/// since the owner opened it on 2026-10-19, live): an account an operator labelled the same and mounted
/// elsewhere is not mistaken for it, and a block that does not say its tier is not judged to be one. And
/// `facts` may know what the block cannot: that the account under the key is an operator's REAL one that
/// spells the label ([`AccountReadiness::real_account_under_twin_label`]; the node's rows derive no twin for it),
/// which a default account's perpetual order must never be rewritten onto.
///
/// ⚠ **At `live` "no real account was reported" is not enough** (review r2, finding 7): the engine must also be
/// POSITIVELY known as the derived twin ([`AccountReadiness::derived_lane_twin`], the watcher's word), because an
/// older real `live` account spelling the label and mounted on the perpetual symbol has the twin's shape to the
/// letter and this answer decides whose keys a default account's order is rewritten onto. The demo tier keeps
/// the absence rule. A restriction that only ADDS a check (the twin's price rule, [`is_twin_shaped`]) does not
/// need the evidence: it fails closed on the shape alone.
pub(super) fn is_lane_twin_engine(
    venue: &str,
    key: &str,
    engine: &VenueBlock,
    facts: Option<&dyn SymbolAdmitter>,
) -> bool {
    is_twin_shaped(venue, key, engine, facts)
        && (engine.tier != Some(AccountTier::Live)
            || facts.is_some_and(|source| source.derived_lane_twin(key)))
}

/// Whether `engine` has the SHAPE of `venue`'s lane twin: the table's label, mount symbol and a tier the table
/// opens it at, and not an account the watcher reported as an operator's real one. The shape is all the
/// door needs to ADD a restriction (an opening order on its own symbol needs a price the engine can be judged
/// at, [`already_trading`]); it is not enough to move another account's order onto it
/// ([`is_lane_twin_engine`]).
fn is_twin_shaped(
    venue: &str,
    key: &str,
    engine: &VenueBlock,
    facts: Option<&dyn SymbolAdmitter>,
) -> bool {
    vike_venues::lane_twin(venue).is_some_and(|twin| {
        twin.is_shaped(key, &engine.symbol, engine.tier)
            && !facts.is_some_and(|source| source.real_account_under_twin_label(key))
    })
}

/// What the lane refusal tells the operator to do about the lane the engine does not trade.
///
/// A venue with a lane twin (`vike_venues::lane_twin`) answers it with the account's OWN other engine:
/// a default (spot) engine is told how to get its perpetual lane (state a margin leverage, which derives
/// the twin; no restart), and the twin's engine is told its spot symbols go to the account's own spot
/// engine. Any other venue keeps the sentence this refusal always carried.
fn other_lane_hint(
    venue: &str,
    key: &str,
    engine: &VenueBlock,
    want_perp: bool,
    facts: Option<&dyn SymbolAdmitter>,
) -> String {
    let Some(twin) = vike_venues::lane_twin(venue) else {
        let other = if want_perp { "perp" } else { "spot" };
        return format!("or an account mounted on the {other} lane");
    };
    if is_lane_twin_engine(venue, key, engine, facts) {
        return format!(
            "or leave the account unnamed: a spot symbol goes to this account's own spot engine, \
             `{venue}`, and `{key}` is only its perpetual lane"
        );
    }
    if key == venue && want_perp {
        // The tiers are the TABLE's (`AccountRowLaneTwin::tiers`), so opening or closing one never leaves this
        // sentence naming the wrong ones.
        let tiers: Vec<&str> = twin.tiers.iter().map(|tier| tier.as_str()).collect();
        return format!(
            "or give this account its perpetual lane: state a margin leverage on its row \
             (`vike-cli account set-leverage --id N --leverage X`, {} accounts only) \
             and the node starts a second engine over the same row and keys, `{venue}#{}`, on \
             the perpetual lane, with no restart; a `.P` order then reaches it by itself",
            tiers.join(" or "),
            twin.label
        );
    }
    let other = if want_perp { "perp" } else { "spot" };
    format!("or an account mounted on the {other} lane")
}

/// **The command as the door addresses it**: a plain Submit (or a Bracket) for a venue whose ONE account
/// runs TWO engines (a lane twin is up, `vike_venues::lane_twin`) is put on the engine of its lane. A perpetual
/// (`.P`) goes to the twin, every other symbol to the default engine, and the address is written out in
/// both cases (`DEFAULT` for the default account).
///
/// WRITTEN OUT, because the core refuses an order that names no account on a venue it runs two engines of
/// (it will not pick one), and a client that has always left the default account unnamed would otherwise
/// have every binance order refused after its `Ack` the moment a twin starts. Only an order for the
/// DEFAULT account (absent, or `DEFAULT`) is touched: another account's order is that account's. A twin
/// that is not up leaves the order exactly as it came, and the plan's refusal names what to do.
///
/// ONE function for both paths that judge a Submit (the command and the Preview), applied
/// before any gate reads the address, so a Preview and a command cannot disagree.
///
/// A `MountStrategy` that names no account is the second thing it addresses (the match arm says why):
/// without it, stating a margin leverage would refuse every strategy mounted at run time on the venue.
pub(crate) fn lane_addressed(
    mut cmd: CommandWire,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> CommandWire {
    match &mut cmd {
        CommandWire::Submit(req) => {
            if let Some(account) =
                lane_account(&req.venue, &req.symbol, req.account.as_deref(), blocks, facts)
            {
                req.account = Some(account);
            }
        }
        // A BRACKET is a plain order's twin here, and for the same reason: the core refuses an account-less
        // bracket on a venue it runs two engines of, and a `.P` bracket of the default account belongs on
        // the twin (the only engine of that venue that can hold its stop-loss).
        CommandWire::Bracket(b) => {
            if let Some(account) =
                lane_account(&b.venue, &b.symbol, b.account.as_deref(), blocks, facts)
            {
                b.account = Some(account);
            }
        }
        // An ORDER LIST, the bracket's twin: every leg is on one symbol, so the list goes to that symbol's lane.
        CommandWire::OrderList(l) => {
            if let Some(account) =
                lane_account(&l.venue, &l.symbol, l.account.as_deref(), blocks, facts)
            {
                l.account = Some(account);
            }
        }
        // A strategy mount that names no account means the venue's DEFAULT account (every shipped mount
        // does), and the core refuses an account-less mount on a venue it runs two engines of. `DEFAULT`
        // names the unlabelled account and routes to it at any engine count: the engine the mount reached
        // before the twin was up.
        CommandWire::MountStrategy { venue, account, .. }
            if account.is_none() && lane_twin_up(venue, blocks, facts) =>
        {
            *account = Some(RESERVED_DEFAULT_LABEL.to_string());
        }
        _ => {}
    }
    cmd
}

/// Whether `venue` runs both its default engine and its lane twin, as the published blocks say.
fn lane_twin_up(venue: &str, blocks: &[VenueBlock], facts: Option<&dyn SymbolAdmitter>) -> bool {
    let Some(twin) = vike_venues::lane_twin(venue) else { return false };
    let twin_key = twin.route_key();
    let published = |key: &str| blocks.iter().find(|b| b.route_key == key);
    published(venue).is_some()
        && published(&twin_key).is_some_and(|b| is_lane_twin_engine(venue, &twin_key, b, facts))
}

/// [`lane_addressed`]'s verdict for an order or a bracket on (`venue`, `symbol`) naming `account`: the
/// account text it is for, or `None` to leave it as it is.
fn lane_account(
    venue: &str,
    symbol: &str,
    account: Option<&str>,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> Option<String> {
    let twin = vike_venues::lane_twin(venue)?;
    if !matches!(account, None | Some(RESERVED_DEFAULT_LABEL)) {
        return None;
    }
    let published = |key: &str| blocks.iter().find(|b| b.route_key == key);
    let twin_key = twin.route_key();
    published(&twin_key).filter(|b| is_lane_twin_engine(venue, &twin_key, b, facts))?;
    if vike_catalog::on_futures_lane(venue, symbol) {
        Some(twin.label.to_string())
    } else {
        published(venue).map(|_| RESERVED_DEFAULT_LABEL.to_string())
    }
}

/// What the refusal of `symbol` on the engine at `key` says of that engine, naming what it does trade: the
/// `engine` of every `crate::refusals::SymbolRefusal` that refuses a symbol the engine does not trade.
fn engine_words(key: &str, engine: &VenueBlock, symbol: &str) -> String {
    let trades = trades_text(engine);
    if trades.is_empty() {
        // An engine mounted on no symbol (polymarket's) that has admitted none yet.
        format!(
            "this node's `{key}` engine was started on no symbol and has admitted none, so `{symbol}` \
             cannot be traded"
        )
    } else {
        format!("this node's `{key}` engine trades {trades} only, not `{symbol}`")
    }
}

/// **A Polymarket order typed by its market NAME goes on the outcome's token id from the first gate on.** Its position
/// is booked under the id, so every gate that asks whether the order closes ([`order_closes`]: the ceiling, the balance
/// wait, the price wait) read a SELL by `slug:Outcome` as flat: an over-ceiling close limit was refused, and with no
/// price held the close was refused "retry", while the same order by its id went. So the name is resolved once, ahead of
/// them, and everything after reads the id, as if the operator had typed it, the plan included.
///
/// Only a book holding something has anything to close: an order on a flat one keeps the plan's own resolution, after
/// its refusals that cost no lookup. A name the source cannot resolve is the plan's refusal, in its words.
pub(crate) fn outcome_name_resolved(
    cmd: &mut CommandWire,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> Result<(), HubRefusal> {
    let (CommandWire::Submit(req), Some(source)) = (cmd, facts) else { return Ok(()) };
    let typed = req.symbol.trim();
    if !matches!(
        rule_of(&req.venue),
        Some(VenueSymbolRule::Admits(VenueSymbolSpelling::OutcomeToken))
    ) || typed.is_empty()
        || is_outcome_token(typed)
    {
        return Ok(());
    }
    let label = match req.account.as_deref() {
        None => AccountLabel::Default,
        Some(text) => match parse_wire_account(text) {
            Ok(label) => label,
            Err(_) => return Ok(()),
        },
    };
    let key = route_key_of(&req.venue, &label);
    let Some(engine) = blocks.iter().find(|b| b.route_key == key) else { return Ok(()) };
    if !matches!(engine.tier, Some(AccountTier::Demo | AccountTier::Live))
        || engine.positions.iter().all(|p| p.size == 0.0)
    {
        return Ok(());
    }
    match source.resolve(&key, &req.venue, typed) {
        Ok(token) => {
            req.symbol = token;
            Ok(())
        }
        Err(why) => {
            Err(SymbolRefusal::NameUnresolved { engine: engine_words(&key, engine, typed), why }
                .into())
        }
    }
}

/// polymarket signs every order as a limit at its price, so a MARKET order that names one would rest or fill at that
/// number while the ceilings size it at the market. Its bridge refuses it after the `Ack` (the backstop): said here,
/// a Preview says it too.
fn market_price_refusal(req: &vike_tradehub_client::wire::OrderRequestWire) -> Option<HubRefusal> {
    (matches!(
        rule_of(&req.venue),
        Some(VenueSymbolRule::Admits(VenueSymbolSpelling::OutcomeToken))
    ) && matches!(OrderType::parse(&req.order_type), Ok(OrderType::Market))
        && req.price.is_some())
    .then(|| SymbolRefusal::MarketOrderPriced { venue: req.venue.clone() }.into())
}

/// **The admission a reconcile CONFIRM needs first, or `Ok(None)`** (owner, 2026-11-10: a held
/// `PositionOnlyExternal` on a polymarket wallet token the engine never traded folded nothing). `row` is the held
/// row the confirm names; when its fills book a symbol the account's engine does not trade, the symbol is planned
/// as the opening MARKET order of the row's size would be ([`plan_submit_symbol`], `commit: false`): the same
/// facts lookup and every symbol refusal an order meets, as a LOOK. Nothing is committed to the gatewayhub (no
/// `SymbolAdmitter::admitted`, so no venue write such as a posted leverage), no price lane is opened and no order
/// exists. `Err` is the operator's sentence; the caller then sends nothing and the row stays held. A row that books
/// no fill, an engine that trades the symbol and an account the snapshot does not list are `Ok(None)`: the core's
/// confirm judges those itself.
pub(crate) fn plan_confirm_admission(
    row: &vike_execution::ReconcileAlertView,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> Result<Option<Box<SymbolAdmission>>, HubRefusal> {
    let Some(symbol) = row.fill_symbol else { return Ok(None) };
    let Some(engine) = blocks.iter().find(|b| b.route_key == row.account) else { return Ok(None) };
    if symbol == engine.symbol || engine.trades(&symbol) {
        return Ok(None);
    }
    let order = vike_tradehub_client::wire::OrderRequestWire {
        order_client_id: String::new(),
        venue: engine.venue.clone(),
        symbol: symbol.to_string(),
        side: if row.fill_qty < 0.0 { -1 } else { 1 },
        qty: row.fill_qty.abs(),
        order_type: "market".into(),
        price: None,
        trigger_price: None,
        reduce_only: false,
        account: engine.account.as_ref().and_then(|l| l.text()).map(|l| l.to_string()),
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    };
    match plan_submit_symbol(&CommandWire::Submit(order), blocks, facts, false) {
        SymbolPlan::Refuse(why) => Err(SymbolRefusal::ConfirmUnadmittable {
            id: row.id,
            symbol: symbol.to_string(),
            why: why.text,
        }
        .into()),
        SymbolPlan::Place { admit, .. } => Ok(admit),
        SymbolPlan::Pass => Ok(None),
    }
}

/// **The plan for a plain Submit's symbol**, judged from the engine's PUBLISHED block (the same
/// composer of the route key as `account_refusal`). Anything that is not a Submit, a venue this gate
/// does not own, an engine that did not say what it trades and an address nobody can parse are
/// somebody else's: `Pass`.
pub(crate) fn plan_submit_symbol(
    cmd: &CommandWire,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
    commit: bool,
) -> SymbolPlan {
    // A BRACKET is planned as the opening order its ENTRY is: the same price wait for the lane twin's own
    // symbol (mounted on no strategy, so no feed marks it and an entry judged with no mark vacates the
    // exposure caps) and the same refresh of an admitted symbol's facts. Its exits are never planned: they
    // are held by the core and reduce-only, and a covered reduce is never held up for a price.
    let entry;
    let req = match cmd {
        CommandWire::Submit(req) => req,
        CommandWire::Bracket(b) => {
            entry = bracket_entry_order(b);
            &entry
        }
        // An ORDER LIST: EVERY opening leg meets the leg-shaped refusal below, and the plan, which is the symbol's (one
        // account, one symbol), is made once, for the leg that needs a price wait when one does (`planned_order`).
        // Its reducing legs are never held up for a price; a list whose every leg reduces is not planned.
        CommandWire::OrderList(l) => {
            let opening = super::order_list::opening_orders(l);
            if let Some(why) = opening.iter().find_map(market_price_refusal) {
                return SymbolPlan::Refuse(why);
            }
            match super::order_list::planned_order(l) {
                Some(planned) => {
                    entry = planned;
                    &entry
                }
                None => return SymbolPlan::Pass,
            }
        }
        _ => return SymbolPlan::Pass,
    };
    let Some(rule) = rule_of(&req.venue) else { return SymbolPlan::Pass };
    if let Some(why) = market_price_refusal(req) {
        return SymbolPlan::Refuse(why);
    }
    let per_order = matches!(rule, VenueSymbolRule::PerOrder | VenueSymbolRule::PerOrderUnsafe(_));
    // A label this gate cannot parse belongs to `lower_command`'s refusal, as in `account_refusal`.
    let label = match req.account.as_deref() {
        None => AccountLabel::Default,
        Some(text) => match parse_wire_account(text) {
            Ok(label) => label,
            Err(_) => return SymbolPlan::Pass,
        },
    };
    let key = route_key_of(&req.venue, &label);
    let Some(engine) = blocks.iter().find(|b| b.route_key == key) else { return SymbolPlan::Pass };
    // A paper account priced through its venue's demo login with that login not running, or one no bar series reaches:
    // nothing could ever fill it, so it is said instead of resting silent. A symbol the engine does not trade is the
    // planner's below (a paper account trades only its own).
    let traded =
        if engine.trades(&req.symbol) { req.symbol.as_str() } else { engine.symbol.as_str() };
    if let Some(why) = paper_without_demo_login(facts, engine)
        .or_else(|| paper_without_bars(facts, engine, traded))
    {
        return SymbolPlan::Refuse(why);
    }
    // A lane twin's OWN symbol is not passed untouched: it is mounted on no strategy, so no boot feed
    // prices it, and it goes down the same road as an admitted symbol that is already trading (the door
    // asks its price lane, once, and holds an opening order for the first price).
    let twin_own = is_twin_shaped(&req.venue, &key, engine, facts);
    if (engine.symbol.is_empty() && !account_wide(rule))
        || (req.symbol == engine.symbol && !twin_own)
    {
        keep_own_lane(facts, commit, &key, &req.venue, engine);
        let wait = match own_first_price(req, blocks, facts, commit, &key, engine) {
            Ok(wait) => wait,
            Err(why) => return SymbolPlan::Refuse(why),
        };
        // A mount booked through a rate whose rate has aged is renewed ahead of the order (decision 0183, the
        // mounted-symbol amendment). That and the first price are the only ways this plan is not the untouched pass.
        return match rerate_own_symbol(facts, commit, &key, req, blocks, engine) {
            None if wait.is_none() => SymbolPlan::Pass,
            admit => SymbolPlan::Place { symbol: req.symbol.clone(), admit, wait },
        };
    }
    // A per-order venue's own symbol in another case, a declared leg and no symbol at all are not foreign.
    if per_order
        && (req.symbol.is_empty()
            || req.symbol.eq_ignore_ascii_case(&engine.symbol)
            || engine.trades(&req.symbol))
    {
        return SymbolPlan::Pass;
    }
    let venue = req.venue.as_str();
    // What every refusal below says of the engine: what it trades, and the symbol the order named.
    let head = engine_words(&key, engine, &req.symbol);
    let refuse = |refusal: SymbolRefusal| SymbolPlan::Refuse(refusal.into());
    if per_order {
        return per_order_refusal(rule, req, engine, &head).map_or(SymbolPlan::Pass, refuse);
    }
    let spelling = match rule {
        VenueSymbolRule::MountedOnly(reason) => {
            // Compared with the engine's OWN symbol and nothing else: a combo leg in `extra_symbols`
            // is a symbol this adapter would still sign on the mounted instrument.
            return refuse(SymbolRefusal::MountedOnly {
                engine: head,
                venue: venue.to_string(),
                reason,
            });
        }
        VenueSymbolRule::Admits(spelling) => spelling,
        VenueSymbolRule::PerOrder | VenueSymbolRule::PerOrderUnsafe(_) => return SymbolPlan::Pass,
    };
    // An `Admits` venue's engine already trading a symbol (a declared leg, or an earlier admission)
    // takes the order as written; an admission of ours that has grown stale is refreshed first.
    if engine.trades(&req.symbol) {
        return already_trading(facts, &key, venue, req.symbol.clone(), req, engine, commit);
    }
    // One instrument is one user name in any case (decision 0290): the engine's own symbol, or one it trades, typed in
    // another case is THAT symbol on every tier, placed on the engine's spelling with no lookup and no second symbol.
    if let Some(own) = std::iter::once(&engine.symbol)
        .chain(engine.extra_symbols.iter())
        .find(|own| !own.is_empty() && own.eq_ignore_ascii_case(req.symbol.trim()))
    {
        return if *own == engine.symbol && !twin_own {
            keep_own_lane(facts, commit, &key, venue, engine);
            match own_first_price(req, blocks, facts, commit, &key, engine) {
                Ok(wait) => {
                    let admit = rerate_own_symbol(facts, commit, &key, req, blocks, engine);
                    SymbolPlan::Place { symbol: own.clone(), admit, wait }
                }
                Err(why) => SymbolPlan::Refuse(why),
            }
        } else {
            already_trading(facts, &key, venue, own.clone(), req, engine, commit)
        };
    }
    // Only a real account takes a foreign symbol: a paper engine is one book (the simulator stamps
    // its own symbol on a fill), and a block that names no tier is never wider than the old gate.
    match engine.tier {
        Some(AccountTier::Demo) => {}
        Some(AccountTier::Live) => {
            // `max_total_exposure` bounds ONE symbol; an account admitted seventeen would carry
            // seventeen times it, with leverage left as the venue holds it. A live account takes a new
            // symbol only with an ACCOUNT-wide ceiling armed.
            if !facts.is_some_and(|source| source.exposure_cap_armed(&key)) {
                return refuse(SymbolRefusal::LiveWithoutAccountCeiling { engine: head });
            }
        }
        // An ACCOUNT-WIDE paper account (polymarket's, mounted on no token) trades every token it is asked for: each is
        // admitted with the facts the datahub states for it (a paper account holds no trade lease:
        // `TradeLeaseRegistry::bind_keyless_facts`) and gets a book of its own (the owner, 2026-11-16: the same for
        // ALL). A paper engine mounted on a token is one book, and a surface with no source admits nothing.
        Some(AccountTier::Paper)
            if account_wide(rule) && engine.symbol.is_empty() && facts.is_some() => {}
        Some(AccountTier::Paper) => {
            return refuse(SymbolRefusal::PaperOneBook { engine: head });
        }
        None => {
            return refuse(SymbolRefusal::TierUnknown { engine: head });
        }
    }
    // polymarket: what the operator typed may be a market NAME (`slug:Outcome`, the market's address). Gamma is a
    // venue socket and this node opens none, so the facts source resolves it to the outcome's token id (the
    // gatewayhub reads Gamma over the account's lease) and everything below plans the id, exactly as if the
    // operator had typed it. An id is never looked up: it names itself.
    let typed =
        if spelling == VenueSymbolSpelling::OutcomeToken && !is_outcome_token(req.symbol.trim()) {
            let Some(source) = facts else {
                return refuse(SymbolRefusal::NoNameLookup { engine: head });
            };
            match source.resolve(&key, venue, req.symbol.trim()) {
                Ok(token) => token,
                Err(why) => return refuse(SymbolRefusal::NameUnresolved { engine: head, why }),
            }
        } else {
            req.symbol.clone()
        };
    // The user name the venue's kept row answers (decision 0290): no list, no row or two rows is the refusal, in words.
    let rows = facts
        .map_or_else(|| door_kept_rows(venue), |source| source.kept_rows(venue))
        .unwrap_or_default();
    let symbol = match vike_catalog::door_user_symbol(venue, &typed, &rows) {
        Ok(symbol) => symbol,
        Err(why) => return refuse(SymbolRefusal::UserNameRefused { engine: head, why }),
    };
    if symbol == engine.symbol && !twin_own {
        keep_own_lane(facts, commit, &key, venue, engine);
        let wait = match own_first_price(req, blocks, facts, commit, &key, engine) {
            Ok(wait) => wait,
            Err(why) => return SymbolPlan::Refuse(why),
        };
        let admit = rerate_own_symbol(facts, commit, &key, req, blocks, engine);
        return SymbolPlan::Place { symbol, admit, wait };
    }
    if engine.trades(&symbol) {
        return already_trading(facts, &key, venue, symbol, req, engine, commit);
    }
    let engine_money = venue_currency_for(venue).engine_money;
    if engine_money == VenueEngineMoney::MountedContract {
        // ibkr: the engine counts money in the currency of its own contract, and nothing converts, EXCEPT for an
        // account that converts a quote currency (decision 0183): then a stock of another currency is looked
        // up, and admitted only with the conversion the facts carry (`quote_refusal` and
        // `stock_conversion_missing` below). Both currencies are the kept rows' (decision 0290's O-10).
        let money = ibkr_engine_money(&rows, &engine.symbol);
        let stock_money = ibkr_engine_money(&rows, &symbol);
        if money.is_none() || money != stock_money {
            // The account's REAL ceiling bit: a demo account with a ceiling armed, mounted on a forex pair or a
            // contract that states no currency, is not told it "needs a ceiling" (it has one; its mount is why).
            let ceiling = facts.is_some_and(|source| source.exposure_cap_armed(&key));
            let converting = money.is_some()
                && stock_money.is_some()
                && !ibkr_forex(&rows, &engine.symbol)
                && quote_conversion_refusal(venue, engine.tier, ceiling).is_none();
            if !converting {
                let hint = quote_conversion_refusal(venue, engine.tier, ceiling)
                    .map(|why| format!(" (a stock of another currency needs a conversion: {why})"))
                    .unwrap_or_default();
                return refuse(SymbolRefusal::StockMoneyDiffers {
                    engine: head,
                    symbol,
                    stock_money: stock_money.unwrap_or("?").to_string(),
                    mounted: engine.symbol.clone(),
                    money: money.unwrap_or("an unknown currency").to_string(),
                    wanted: stock_money.unwrap_or("matching").to_string(),
                    hint,
                });
            }
        }
    }
    // dukascopy and fxcm (`MountedPairQuote`): the engine counts money in the quote of its own pair and nothing
    // converts (decision 0170), so a pair is admitted only when it is quoted in the same money. A name that is not a
    // pair is the gatewayhub table's to refuse by name. EXCEPT for an account that converts (decision 0183: a `demo` account
    // with an account-wide ceiling armed, [`quote_conversion_refusal`]): a pair quoted in another money is then
    // the facts' to judge, and it is admitted only with the currency and the rate they state (checked below, so
    // a gatewayhub that states neither never lets a foreign pair through at par).
    let mut foreign_money: Option<String> = None;
    let pair_venue = engine_money == VenueEngineMoney::MountedPairQuote;
    let quote = pair_money(&rows, venue, &symbol);
    if pair_venue && quote.is_some() {
        // The money the LEDGER counts in. An engine that was told a non-dollar currency (`VenueBlock::ledger_currency`,
        // empty for the dollar group and for an engine nobody told) counts in THAT whatever its mount is quoted in:
        // before decision 0183's mounted-symbol amendment the two were the same by the mount fence (a ledger was
        // told only the money its mount is quoted in), and now a EUR ledger over a USD-quoted mount converts the
        // mount like any pair. Every other engine counts in its mount's money, as it always did.
        // A pair quoted in no known money (`None` here) equals no ledger: it is refused, never converted.
        let quote = quote.and_then(money_word);
        let money = if engine.ledger_currency.is_empty() {
            pair_money(&rows, venue, &engine.symbol).and_then(money_word)
        } else {
            Some(engine.ledger_currency.as_str())
        };
        let converts = facts.is_some_and(|source| {
            quote_conversion_refusal(venue, engine.tier, source.exposure_cap_armed(&key)).is_none()
        });
        if money.is_some() && quote.is_some() && money != quote && converts {
            foreign_money = quote.map(str::to_string);
        } else if money.is_none() || money != quote {
            return refuse(SymbolRefusal::PairMoneyDiffers {
                engine: head,
                symbol,
                quote: quote.unwrap_or("?").to_string(),
                mounted: engine.symbol.clone(),
                money: money.unwrap_or("an unknown currency").to_string(),
                wanted: quote.unwrap_or("the same money").to_string(),
            });
        }
    }
    if vike_venues::spot_pair_lanes_for(venue).lanes == vike_venues::SpotPairLanes::TwoExecLanes {
        // binance and aster: the lane is the kept row's class (decision 0290): a spot row is the spot lane, a
        // perpetual or a dated future the futures lane (`BTCUSDT_261225` carries no `.P`).
        let futures_lane = |name: &str| {
            row_of(&rows, venue, name).map(|row| row.asset_class() != AssetClass::CryptoSpot)
        };
        let Some(engine_perp) = futures_lane(&engine.symbol) else {
            return refuse(SymbolRefusal::LaneUnknown {
                engine: head,
                mounted: engine.symbol.clone(),
                venue: venue.to_string(),
            });
        };
        let want_perp = futures_lane(&symbol).unwrap_or(engine_perp);
        if engine_perp != want_perp {
            let (lane, other) = if engine_perp { ("perp", "spot") } else { ("spot", "perp") };
            return refuse(SymbolRefusal::OtherLane {
                engine: head,
                symbol,
                other,
                venue: venue.to_string(),
                lane,
                hint: other_lane_hint(venue, &key, engine, want_perp, facts),
            });
        }
    }
    // (A hyperliquid engine of ANY mounted product takes a perpetual, a spot pair or a builder-dex coin as an
    // admitted symbol since decision 0218: the adapter signs each order by the ORDER's symbol on that symbol's
    // own asset id, grid and product, and the engine books the symbol's margin mode from the facts. The engine's
    // BALANCE is still its mount's product's, so a spot pair on a perp-mounted standard account is refused by the
    // venue for want of spot funds, never mis-sized here.)
    if engine.extra_symbols.len() >= MAX_EXTRA_SYMBOLS {
        return refuse(SymbolRefusal::TooManySymbols { engine: head, cap: MAX_EXTRA_SYMBOLS });
    }
    let Some(source) = facts else {
        return refuse(SymbolRefusal::NoFactsSource { engine: head });
    };
    // An opening order needs a price at the end ([`with_price`]); a venue this node cannot price at all
    // refuses it now, BEFORE the lookup spends a venue connection on an order that can never succeed.
    if !req.reduce_only
        && let Err(why) = source.price_source(venue)
    {
        return refuse(SymbolRefusal::NewNoPriceFeed { engine: head, symbol, why });
    }
    let admission = match source.admission(&key, venue, &symbol) {
        Ok(admission) => admission,
        Err(why) => {
            return refuse(SymbolRefusal::FactsRefused {
                engine: head,
                symbol,
                why: why.carried(),
            });
        }
    };
    if admission.route_key != key || admission.symbol != symbol {
        return refuse(SymbolRefusal::FactsMismatch {
            engine: head,
            answered: admission.symbol,
            answered_key: admission.route_key,
            symbol,
            key,
        });
    }
    if admission.grid.tick_size.is_none() || admission.grid.lot_size.is_none() {
        let missing = match (admission.grid.tick_size, admission.grid.lot_size) {
            (None, None) => "tick size and lot size",
            (None, Some(_)) => "tick size",
            _ => "lot size",
        };
        return refuse(SymbolRefusal::NoGrid {
            engine: head,
            venue: venue.to_string(),
            missing,
            symbol,
        });
    }
    // deribit counts money in two ways, and an engine in one (the module doc).
    if venue == "deribit"
        && let Some(why) = money_model_refusal(&rows, engine, &admission, &head)
    {
        return refuse(why);
    }
    // Every venue: an inverse engine holds one settle coin (decision 0258), read off both kept rows.
    if let Some(why) = settle_coin_refusal(&rows, venue, engine, &admission, &head) {
        return refuse(why);
    }
    // Every venue: one engine, one contract kind (deribit's own words above fire first for its engines).
    if let Some(why) = contract_kind_refusal(venue, engine, &admission, &head) {
        return refuse(why);
    }
    // A symbol quoted in a currency other than the account's needs a rate and an account that converts.
    if let Some(why) = quote_refusal(venue, engine, &admission, source, &key, &head) {
        return refuse(why);
    }
    // ibkr: a stock of a currency other than the engine's money was let past the lookup only because the
    // account converts, and is admitted only with the conversion in the facts. The facts of a gatewayhub that
    // does not convert (an older one, or a lease that asked the old way) carry none, and would put the stock
    // in the ledger at par.
    if engine_money == VenueEngineMoney::MountedContract
        && let Some(why) = stock_conversion_missing(&rows, engine, &admission, &head)
    {
        return refuse(why);
    }
    // A pair the door read as quoted in another money than the engine's (dukascopy, above) is admitted only
    // when the facts state THAT currency: an answer with none is a gatewayhub that did not convert, and the
    // pair would be booked at par.
    if let Some(expected) = &foreign_money
        && admission.quote_currency.as_deref() != Some(expected.as_str())
    {
        return refuse(SymbolRefusal::FactsCurrencyDiffers {
            engine: head,
            symbol,
            expected: expected.clone(),
            stated: admission.quote_currency.as_deref().unwrap_or("no currency").to_string(),
        });
    }
    // A pair in the MOUNT's own money for which the facts nonetheless state a currency (the review's F-1). An
    // anchored engine has mount money == account money, and the facts state a currency only for a pair quoted in
    // something other than the account's, so a stated currency here means this engine is NOT anchored (the mount's
    // money is not the account's): the engine would refuse the admission (`account-currency-unset`) and deny the order,
    // but only AFTER the node had committed the symbol (`TradeLeaseRegistry::admitted` makes its price pump permanent and
    // a place of the venue's lane is held until a restart). Refused here, before any of that.
    if pair_venue
        && quote.is_some()
        && foreign_money.is_none()
        && let Some(currency) = admission.quote_currency.as_deref()
    {
        return refuse(SymbolRefusal::LedgerNotAnchored {
            engine: head,
            symbol,
            currency: currency.to_string(),
            mounted: engine.symbol.clone(),
        });
    }
    // The slot is spent only by an order worth admitting for. An admission lasts until the operator gives the
    // symbol back (`unadmit-symbol`, decision 0205; the engine caps a venue account at `MAX_EXTRA_SYMBOLS`), so a
    // reduce-only order (it can only fail: the engine holds nothing in a symbol it was never
    // told about) and an order the venue's own grid would reject must not be able to burn one.
    if req.reduce_only {
        return refuse(SymbolRefusal::ReduceOnlyNewSymbol { engine: head });
    }
    // The floor is the venue's, in the venue's currency, so the order is valued at the venue's own unit (the
    // facts' value, before any rate): an ig epic of 10,000 or an inverse contract of one dollar is the money the
    // gate judges, not `qty · price`.
    let unit = admission.contract_value.unwrap_or_default();
    if let Some(short) = grid_refusal(req, &admission.grid, unit) {
        return refuse(match short {
            GridShort::MinQty { typed, min } => {
                SymbolRefusal::NewBelowMinQty { engine: head, typed, min }
            }
            GridShort::Lot { typed, lot } => {
                SymbolRefusal::NewBelowOneLot { engine: head, typed, lot }
            }
            GridShort::MinNotional { notional, min } => {
                SymbolRefusal::NewBelowMinNotional { engine: head, notional, min }
            }
        });
    }
    let named = symbol.clone();
    with_price(req, facts, &key, venue, symbol, Some(Box::new(admission)), commit).unwrap_or_else(
        |why| refuse(SymbolRefusal::NewNoPriceFeed { engine: head, symbol: named, why }),
    )
}

/// **Why a symbol quoted in a currency other than the account's cannot be admitted, or `None`** (decision
/// 0183, phase 1). An admission that names no currency is quoted in the account's own (the daemon's fence
/// guarantees it) and must carry no rate either. One that names a currency is admitted only with a usable
/// rate (finite, positive, stamped: the engine refuses anything else, and so does the door, first, in
/// words), and only for an account that converts at all: OANDA's practice account with an account-wide
/// exposure ceiling armed ([`quote_conversion_refusal`], whose tier half the engine's assembly reads too).
/// Freshness is the facts source's to judge against its clock (`TradeLeaseRegistry`); the engine judges it again
/// against its own. `head` is [`engine_words`]'s.
fn quote_refusal(
    venue: &str,
    engine: &VenueBlock,
    admission: &SymbolAdmission,
    source: &dyn SymbolAdmitter,
    key: &str,
    head: &str,
) -> Option<SymbolRefusal> {
    let symbol = admission.symbol.clone();
    let Some(currency) = admission.quote_currency.clone() else {
        return (admission.quote_rate.is_some() || admission.rate_as_of_ms.is_some()).then(|| {
            SymbolRefusal::RateWithoutCurrency {
                engine: head.to_string(),
                venue: venue.to_string(),
                symbol,
            }
        });
    };
    let usable = admission.quote_rate.is_some_and(|rate| rate.is_finite() && rate > 0.0)
        && admission.rate_as_of_ms.is_some();
    if !usable {
        return Some(SymbolRefusal::NoUsableRate { engine: head.to_string(), symbol, currency });
    }
    quote_conversion_refusal(venue, engine.tier, source.exposure_cap_armed(key))
        .map(|why| SymbolRefusal::NoConversion { engine: head.to_string(), symbol, currency, why })
}

/// Why an ibkr stock of another currency than the engine's money is not admitted because the facts carry no
/// conversion into it, or `None`. A stock in the engine's own currency needs none. An absent currency is never
/// read as the engine's: the facts must NAME the stock's currency as the one they convert. `head` is
/// [`engine_words`]'s.
fn stock_conversion_missing(
    rows: &[Instrument],
    engine: &VenueBlock,
    admission: &SymbolAdmission,
    head: &str,
) -> Option<SymbolRefusal> {
    let money = ibkr_engine_money(rows, &engine.symbol)?;
    let stock = ibkr_engine_money(rows, &admission.symbol)?;
    if vike_model::same_currency(money, stock) {
        return None;
    }
    let converts =
        admission.quote_currency.as_deref().is_some_and(|c| vike_model::same_currency(c, stock))
            && admission.quote_rate.is_some();
    (!converts).then(|| SymbolRefusal::StockConversionMissing {
        engine: head.to_string(),
        symbol: admission.symbol.clone(),
        stock: stock.to_string(),
        money: money.to_string(),
    })
}

/// `admission`'s contract value in the ACCOUNT's currency: the venue's unit times the conversion rate when
/// the admission carries one (`ContractValue::in_account`, the engine's own function), the facts' value
/// untouched when it does not.
fn effective_contract_value(admission: &SymbolAdmission) -> Option<vike_model::ContractValue> {
    match admission.quote_rate {
        None => admission.contract_value,
        Some(rate) => admission.contract_value.unwrap_or_default().in_account(rate),
    }
}

/// Why the venue's own grid would reject the order, judged from the facts the door looked up: a
/// quantity under the minimum or under one lot, or a priced order under the minimum notional. A
/// market order has no price to size, so it is judged on quantity alone.
///
/// ⚠ **On what the gate would SEND, with the gate's own arithmetic** (`RiskGate::check_inner`: `round_to`, half to even,
/// on the symbol's lot and tick, then `below-min-qty`, `non-positive-size` and `below-min-notional`), so the door
/// refuses only what the gate would deny: 6 on a lot of 10 goes out as 10 ([`rounding_note`] says so), and 1 rounds to
/// nothing and is refused. The comparison is the gate's too, in an f64 and with no tolerance: a grid multiple one ULP
/// under a minimum (`crates/vike-bridge-core/src/format.rs`'s `grid_multiple_image`) is denied by both. The notional
/// is the gate's function, `ContractValue::order_notional` of `unit` (the venue's, in the floor's currency):
/// `qty · |price|` bit for bit at the default unit. Its two callers word what fell short (`SymbolRefusal`).
fn grid_refusal(
    req: &vike_tradehub_client::wire::OrderRequestWire,
    grid: &vike_model::SymbolGrid,
    unit: vike_model::ContractValue,
) -> Option<GridShort> {
    let typed = req.qty.abs();
    let qty = vike_model::round_to(typed, grid.lot_size);
    if let Some(min) = grid.min_qty
        && qty < min
    {
        return Some(GridShort::MinQty { typed, min });
    }
    if let Some(lot) = grid.lot_size
        && qty <= 0.0
    {
        return Some(GridShort::Lot { typed, lot });
    }
    let price = req.limit_price().map(|p| vike_model::round_to(p, grid.tick_size));
    let price = price.or(req.trigger_price)?;
    let min = grid.min_notional?;
    let notional = unit.order_notional(qty, price);
    (notional < min).then_some(GridShort::MinNotional { notional, min })
}

/// What [`grid_refusal`] found short of the venue's grid: the quantity typed under the minimum or under one lot, or
/// the order's value under the minimum notional.
enum GridShort {
    MinQty { typed: f64, min: f64 },
    Lot { typed: f64, lot: f64 },
    MinNotional { notional: f64, min: f64 },
}

/// The terms a `Modify` would leave the order it names at (`orders`: the door's named open order). The published
/// order carries no reduce-only flag, so `reduce_only` here says what the position does: an order on the side that
/// closes the net position counts as one, and a close is never blocked by a minimum. `None`: no such open order.
fn modified_order(
    order_client_id: &str,
    new_qty: Option<f64>,
    (new_price, new_trigger_price): (Option<f64>, Option<f64>),
    orders: &[vike_execution::OrderView],
    blocks: &[VenueBlock],
) -> Option<vike_tradehub_client::wire::OrderRequestWire> {
    let order =
        orders.iter().find(|o| o.order_client_id == order_client_id && !o.status.is_terminal())?;
    let label = order.account.clone().unwrap_or_default();
    let key = route_key_of(&order.venue, &label);
    let position = super::refusal::net_position(blocks, &key, &order.symbol);
    let (limit, trigger) =
        vike_model::modified_prices(&order.order_type, new_price, new_trigger_price);
    Some(vike_tradehub_client::wire::OrderRequestWire {
        order_client_id: order_client_id.to_string(),
        venue: order.venue.clone(),
        symbol: order.symbol.clone(),
        side: order.side,
        qty: new_qty.unwrap_or(order.qty),
        order_type: String::new(),
        price: limit.or(order.price),
        trigger_price: trigger.or(order.trigger_price),
        reduce_only: vike_model::is_reducing_direction(order.side, position),
        account: label.text().map(str::to_string),
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    })
}

/// The order a Submit, a Bracket's entry or a Modify comes to, the route key of its engine and the grid held for its
/// symbol (`symbol`: the canonical spelling, when the plan moved it). `None`: another verb, or no grid held.
fn order_on_held_grid(
    cmd: &CommandWire,
    symbol: Option<&str>,
    orders: &[vike_execution::OrderView],
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> Option<(vike_tradehub_client::wire::OrderRequestWire, String, vike_model::SymbolGrid)> {
    let req = match cmd {
        CommandWire::Submit(req) => req.clone(),
        CommandWire::Bracket(b) => bracket_entry_order(b),
        CommandWire::Modify { order_client_id, new_qty, new_price, new_trigger_price } => {
            modified_order(
                order_client_id,
                *new_qty,
                (*new_price, *new_trigger_price),
                orders,
                blocks,
            )?
        }
        _ => return None,
    };
    let label = match req.account.as_deref() {
        None => AccountLabel::Default,
        Some(text) => parse_wire_account(text).ok()?,
    };
    let key = route_key_of(&req.venue, &label);
    let grid = facts?.held_grid(&key, symbol.unwrap_or(&req.symbol))?;
    Some((req, key, grid))
}

/// The signed net position the published blocks hold in the symbol `req` names, whatever spelling it was typed in: the
/// door reads this ahead of the symbol plan as well as after it. A blank symbol, or the engine's own in another case,
/// is the engine's spelling; any other is the venue's canonical one (a name only the facts source resolves stays as
/// typed and reads flat).
fn position_named_by(
    req: &vike_tradehub_client::wire::OrderRequestWire,
    blocks: &[VenueBlock],
) -> f64 {
    let label = match req.account.as_deref() {
        None => AccountLabel::Default,
        Some(text) => match parse_wire_account(text) {
            Ok(label) => label,
            Err(_) => return 0.0,
        },
    };
    let key = route_key_of(&req.venue, &label);
    let typed = req.symbol.trim();
    let symbol = blocks
        .iter()
        .rfind(|b| b.route_key == key)
        .filter(|b| typed.is_empty() || typed.eq_ignore_ascii_case(&b.symbol))
        .map(|b| b.symbol.clone())
        .or_else(|| {
            let rows = door_kept_rows(&req.venue).unwrap_or_default();
            vike_catalog::door_user_symbol(&req.venue, typed, &rows).ok()
        })
        .unwrap_or_else(|| typed.to_string());
    super::refusal::net_position(blocks, &key, &symbol)
}

/// **Whether the door takes `req` for a CLOSE**: no ceiling sizes it, no minimum refuses it and no price wait holds it,
/// because a check here must never stop a real close. The published position says so, with the flag or without it, by
/// the core's own reading (`vike_model::is_covered_reduce`: the order opposes the position and the position covers
/// ALL of it, so an order past the position is an open, whole, as the gate judges it).
///
/// **The flag alone says it only where the venue enforces it** (`vike_venues::capabilities_for`'s `supports_reduce_only`, the
/// question the engine's `cover_position_size` asks): such a venue cannot open with it, and the snapshot may be a fill
/// behind, so a stale one must not size a real close. Everywhere else (ibkr, alpaca, a netting oanda: "never reads the
/// flag") a flagged order goes out plain and can open, 1,000 on a flat book or whatever it holds past a small position,
/// so the position alone decides there: a close over the ceiling goes when the position shows it, and a fill the
/// snapshot has not caught can have a real close sized until it has, one retry.
pub(crate) fn order_closes(
    req: &vike_tradehub_client::wire::OrderRequestWire,
    blocks: &[VenueBlock],
) -> bool {
    vike_model::is_covered_reduce(req.side, position_named_by(req, blocks), req.qty)
        || (req.reduce_only && vike_venues::capabilities_for(&req.venue).supports_reduce_only)
}

/// **Whether the door takes an ORDER LIST for a CLOSE** (D4 of 2026-11-24): every leg reduce-only and each covered by
/// the position, as [`order_closes`] reads a plain order (an exit OCO on a covered long: a take-profit and a stop, each
/// the whole position). Such a list is never sized, so no ceiling blocks the exit. A leg past the position, or one that
/// is not reduce-only, makes the list an open, sized leg by leg. The client's twin is `vike_client::addressed`'s `closes`,
/// held to this by `crates/vike-tradehub/src/server/tests/client_door.rs`.
pub(crate) fn order_list_closes(
    l: &vike_tradehub_client::wire::OrderListWire,
    blocks: &[VenueBlock],
) -> bool {
    !l.legs.is_empty()
        && l.legs.iter().enumerate().all(|(i, leg)| {
            let position = position_named_by(&super::order_list::plain_order(l, i), blocks);
            leg.reduce_only && vike_model::is_covered_reduce(leg.side, position, leg.qty)
        })
}

/// **Whether a `Modify` to `new_qty` leaves the OPEN order `order_client_id` closing the position** (`orders`: the door's named
/// order): it opposes the position, which covers the new size. The published order carries no flag, so the position
/// is all there is to say it, on any venue: a plain order raised past the position is an open.
pub(crate) fn modify_closes(
    order_client_id: &str,
    new_qty: f64,
    orders: &[vike_execution::OrderView],
    blocks: &[VenueBlock],
) -> bool {
    modified_order(order_client_id, Some(new_qty), (None, None), orders, blocks).is_some_and(
        |req| vike_model::is_covered_reduce(req.side, position_named_by(&req, blocks), req.qty),
    )
}

/// **Why the venue's grid refuses this opening order, judged from the grid this node already holds**
/// ([`SymbolAdmitter::held_grid`]; none held, nothing is refused). The door and the Preview call it, so an order the
/// engine's gate would only deny after the answer (`non-positive-size`, `below-min-qty`) is refused by name first.
///
/// Only an operator's order comes here: a strategy's order goes from its mount to the engine's gate and never
/// through the control door.
///
/// ⚠ **A close is never blocked by a minimum** ([`order_closes`], as the gate's floors read `covered_reduce`):
/// [`grid_refusal`] knows no close (the new-symbol plan refuses one before it asks), and flatten and market exit are
/// other verbs.
pub(crate) fn held_grid_refusal(
    cmd: &CommandWire,
    symbol: Option<&str>,
    orders: &[vike_execution::OrderView],
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> Option<HubRefusal> {
    // An ORDER LIST: every opening leg is judged on the grid as the plain order it is, the first refused one named.
    if let CommandWire::OrderList(l) = cmd {
        return super::order_list::opening_orders(l).into_iter().find_map(|leg| {
            held_grid_refusal(&CommandWire::Submit(leg), symbol, orders, blocks, facts)
        });
    }
    let (req, key, mut grid) = order_on_held_grid(cmd, symbol, orders, blocks, facts)?;
    if order_closes(&req, blocks) {
        return None;
    }
    let name = symbol.unwrap_or(&req.symbol);
    // The block publishes the EFFECTIVE value (the rate folded in), not the venue's unit the floor is stated in,
    // so only a plain linear contract is judged here: on any other (inverse, a multiplier, booked through a rate)
    // the money floor stays with the engine, which values it.
    if blocks
        .iter()
        .find(|b| b.route_key == key)
        .is_none_or(|b| b.contract_value_of(name) != vike_model::ContractValue::default())
    {
        grid.min_notional = None;
    }
    let verb = if matches!(cmd, CommandWire::Modify { .. }) { "modify" } else { "submit" };
    let refusal = match grid_refusal(&req, &grid, vike_model::ContractValue::default())? {
        GridShort::MinQty { typed, min } => {
            SymbolRefusal::BelowMinQty { verb, name: name.to_string(), typed, min }
        }
        GridShort::Lot { typed, lot } => {
            SymbolRefusal::BelowOneLot { verb, name: name.to_string(), typed, lot }
        }
        GridShort::MinNotional { notional, min } => {
            SymbolRefusal::BelowMinNotional { verb, name: name.to_string(), notional, min }
        }
    };
    Some(refusal.into())
}

/// **What the order will actually carry, when the venue's step moves it.** The engine's gate rounds a quantity to
/// the lot, half to even (`vike_model::round_to`, the grid multiple the wire owes), and that stays: this says so to
/// whoever sends it, from the grid this node holds, at the door's answer and the Preview's. Never a refusal.
pub(crate) fn rounding_note(
    cmd: &CommandWire,
    symbol: Option<&str>,
    orders: &[vike_execution::OrderView],
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> Option<String> {
    // An ORDER LIST: what each opening leg will carry, in one note.
    if let CommandWire::OrderList(l) = cmd {
        let notes: Vec<String> = super::order_list::opening_orders(l)
            .into_iter()
            .filter_map(|leg| {
                rounding_note(&CommandWire::Submit(leg), symbol, orders, blocks, facts)
            })
            .collect();
        return (!notes.is_empty()).then(|| notes.join("; "));
    }
    let (req, _, grid) = order_on_held_grid(cmd, symbol, orders, blocks, facts)?;
    let step = grid.lot_size.filter(|step| *step > 0.0)?;
    let sent = vike_model::round_to(req.qty, Some(step));
    // Compared in steps: `3 · 0.1` is not `0.3` in an f64, and that order is on the grid.
    if (req.qty / step - sent / step).abs() < 1e-9 {
        return None;
    }
    let places = step.to_string().split('.').nth(1).map_or(0, str::len);
    Some(if sent > 0.0 {
        format!("qty {} goes out as {sent:.places$}, the venue's step is {step}", req.qty)
    } else {
        // Only a close gets here (an opening one was refused): the engine denies it.
        format!(
            "qty {} rounds to nothing on the venue's step {step}: the engine will deny it",
            req.qty
        )
    })
}

/// Why a per-order venue's engine cannot take `req`, a symbol it was not started on, or `None`: its
/// adapter reads the order's symbol, but the engine is still ONE book on the mounted symbol's grid. `head` is
/// [`engine_words`]'s.
fn per_order_refusal(
    rule: VenueSymbolRule,
    req: &vike_tradehub_client::wire::OrderRequestWire,
    engine: &VenueBlock,
    head: &str,
) -> Option<SymbolRefusal> {
    // The paper client stamps its own symbol on every fill and runs one single-symbol book.
    if engine.tier == Some(AccountTier::Paper) {
        return Some(SymbolRefusal::PaperOneBook { engine: head.to_string() });
    }
    if let VenueSymbolRule::PerOrderUnsafe(reason) = rule {
        return Some(SymbolRefusal::PerOrderUnsafe {
            engine: head.to_string(),
            venue: req.venue.clone(),
            reason,
        });
    }
    // No per-venue price watcher exists for these venues: a priceless opening order is judged at
    // notional 0 and passes every ceiling, margin check and collar. (Reduce-only closes what exists.)
    (!req.reduce_only && req.limit_price().is_none()).then(|| SymbolRefusal::PerOrderUnpriced {
        engine: head.to_string(),
        venue: req.venue.clone(),
    })
}

/// **The order names the engine's OWN symbol: keep its price lane** (decision 0197; [`SymbolAdmitter::hold_lane`]).
/// The engine's own symbol passes the door untouched (no price asked, no admission, no wait), and a `demo`/`live`
/// engine that goes on to hold a position or an order in it would be priced by nobody unless a strategy is mounted
/// there. So a COMMITTING surface (never a Preview) asks the source to hold the lane, and this call asks for no quote
/// and refuses nothing: a limit order a closed market accepts stays accepted. A `paper` engine on a venue priced through
/// its demo login (`crate::readmit::holds_own_lane`) is held the same way, through that login; any other `paper` engine,
/// an engine with no symbol and a surface with no source ask nothing.
fn keep_own_lane(
    facts: Option<&dyn SymbolAdmitter>,
    commit: bool,
    key: &str,
    venue: &str,
    engine: &VenueBlock,
) {
    if commit
        && !engine.symbol.is_empty()
        && crate::readmit::holds_own_lane(engine)
        && let Some(source) = facts
    {
        source.hold_lane(key, venue, &engine.symbol);
    }
}

/// **The words a `paper` account of a venue priced through its demo login says when no demo login runs** (the owner's
/// ruling of 2026-11-16): on the refusal of each of its orders ([`paper_without_demo_login`]) and, through the account's
/// venue-path statement, in `trade health` (`TradeLeaseRegistry::sync_paper_quotes`).
#[must_use]
pub fn no_demo_login(venue: &str) -> String {
    format!("no price: no demo login for {venue}")
}

/// **The words a `paper` account waiting for its venue's market to open says** (`crate::readmit::paper_waits_for_open`,
/// `open_ms` its next open): the live accounts' own sentence for the closed FX week
/// (`vike_venues::session::fx_login_closed_words`), then why it binds a paper account. The door's refusal of its orders
/// ([`paper_market_closed_refusal`]), its venue-path statement in `trade health`
/// (`TradeLeaseRegistry::sync_paper_quotes`) and the ig unit stater's one line (`TradeLeaseRegistry::state_paper_units`)
/// all say it.
#[must_use]
pub fn paper_market_closed(venue: &str, open_ms: i64) -> String {
    format!(
        "{}: a paper account of {venue} follows {venue}'s market clock and trades only while that market is open (no \
         price of {venue} reaches it before the open)",
        vike_venues::session::fx_login_closed_words(open_ms)
    )
}

/// **Refuse an order to a `paper` account whose venue's market is closed at `now_ms`, in words**
/// (`crate::readmit::paper_waits_for_open`; the owner, 2026-11-21: a paper account lives by its venue's clock). A paper
/// book meets a new order at the last quote it holds, which on a closed market is the one before the close, so it would
/// fill on a stale price (decision 0285's amendment of 2026-11-17: a paper market order fills at the CURRENT quote or not
/// at all), or rest on a book no quote reaches until the open. Decision 0236's note-never-a-refusal is about an order
/// SENT to a venue that decides itself; a paper book is the node's own. The four verbs that place an
/// order (a submit, a bracket, an order list, a combo) are judged, on the newest published block of the account they
/// address; a cancel, a modify and the reducing plane pass. The door and the Preview ask it before the plan.
pub(crate) fn paper_market_closed_refusal(
    cmd: &CommandWire,
    blocks: &[VenueBlock],
    now_ms: i64,
) -> Option<HubRefusal> {
    let (venue, account) = match cmd {
        CommandWire::Submit(req) => (&req.venue, &req.account),
        CommandWire::Bracket(b) => (&b.venue, &b.account),
        CommandWire::OrderList(l) => (&l.venue, &l.account),
        CommandWire::Combo(c) => (&c.venue, &c.account),
        _ => return None,
    };
    // A label this gate cannot parse belongs to `lower_command`'s refusal, as in `account_refusal`.
    let label = match account.as_deref() {
        None => AccountLabel::Default,
        Some(text) => parse_wire_account(text).ok()?,
    };
    let key = route_key_of(venue, &label);
    let engine = blocks.iter().rev().find(|b| b.route_key == key)?;
    let open = crate::readmit::paper_waits_for_open(engine.tier, &engine.venue, now_ms)?;
    let closed = paper_market_closed(&engine.venue, open);
    Some(SymbolRefusal::PaperMarketClosed { closed }.into())
}

/// **The account `venue`'s price stream needs running, and what to do, in words**: named by whose login the stream rides
/// at the gatewayhub (`crate::feeds::paper_price_login`, the rule `TradeLeaseRegistry`'s `pick_demo_login` checks), so
/// the refusal names the very account the stream would use.
#[must_use]
pub(crate) fn demo_login_needed(venue: &str) -> String {
    match crate::feeds::paper_price_login(venue) {
        Some(CredentialedStreamLogin::DefaultAccount) => format!(
            "{venue}'s price stream logs in as its DEFAULT account (`{venue}`) at the demo tier, and that account does \
             not run here at demo (a labelled demo account does not carry this stream). Run the DEFAULT {venue} \
             account at demo (`account.active = 1` on its `demo` row)"
        ),
        _ => format!(
            "{venue}'s price stream rides the session a running demo account of {venue} holds, and no demo account \
             of {venue} runs here. Arm one (`account.active = 1` on its `demo` row)"
        ),
    }
}

/// **Why every order of `engine` is refused: it is a `paper` account of a venue whose paper book is priced and filled
/// from the venue's DEMO login** (`crate::feeds::paper_quotes_through_demo`: dukascopy, fxcm, ibkr), **and no demo
/// account of the venue runs here** ([`SymbolAdmitter::demo_login`]; a node with no source has none). Such an order
/// would rest unfilled for ever, so it is refused in words instead (the owner: it must never sit silent). `None` for
/// every other engine.
fn paper_without_demo_login(
    facts: Option<&dyn SymbolAdmitter>,
    engine: &VenueBlock,
) -> Option<HubRefusal> {
    let venue = engine.venue.as_str();
    (engine.tier == Some(AccountTier::Paper)
        && crate::feeds::paper_quotes_through_demo(venue)
        && facts.is_none_or(|source| !source.demo_login(venue)))
    .then(|| {
        SymbolRefusal::PaperWithoutDemoLogin {
            no_login: no_demo_login(venue),
            venue: venue.to_string(),
            needed: demo_login_needed(venue),
        }
        .into()
    })
}

/// **Why every order of `engine` on `symbol` is refused: it is a `paper` account no strategy mount's feed prices AND no
/// quote lane of its venue can reach** (the venue is one the node never opens a lane of on its own initiative,
/// `NO_HELD_LANE`; or no price feed of the venue runs here, [`SymbolAdmitter::price_source`]), or `None`. Every other
/// paper account is priced from its own symbol's HELD quote lane (`crate::readmit::holds_own_lane`): a manual order
/// fills at the CURRENT ask or bid on every venue, mounted or not, never at a past or a bar price (the owner, 2026-11-16:
/// "it has to be the same for ALL"). A source that cannot tell ([`SymbolAdmitter::bar_series`]'s default) and a node
/// with no source refuse nothing; a venue priced through its demo login is [`paper_without_demo_login`]'s, and an engine
/// mounted on no symbol (polymarket's account-wide one) is left to the planner.
fn paper_without_bars(
    facts: Option<&dyn SymbolAdmitter>,
    engine: &VenueBlock,
    symbol: &str,
) -> Option<HubRefusal> {
    let venue = engine.venue.as_str();
    if engine.tier != Some(AccountTier::Paper)
        || engine.symbol.is_empty()
        || crate::feeds::paper_quotes_through_demo(venue)
    {
        return None;
    }
    let source = facts.filter(|source| !source.bar_series(venue, symbol))?;
    let held = crate::venue_backend::gateway::trade_lease_registry::NO_HELD_LANE
        .iter()
        .find(|(name, _)| *name == venue);
    let why = match held {
        Some((_, why)) => (*why).to_string(),
        None if crate::feeds::paper_quote_lane(venue) => source.price_source(venue).err()?,
        None => format!("this node has no price feed arm for {venue}"),
    };
    Some(no_quote_reaches(symbol, venue, why))
}

/// The refusal of a `paper` order whose symbol no quote can reach ([`paper_without_bars`]): `why` names the cause.
fn no_quote_reaches(symbol: &str, venue: &str, why: String) -> HubRefusal {
    SymbolRefusal::PaperNoQuote { symbol: symbol.to_string(), venue: venue.to_string(), why }.into()
}

/// The refusal of a `paper` order on a symbol no strategy mount trades on this node ([`paper_without_bars`],
/// [`paper_unmounted_refusal`]).
fn no_mount_trades(symbol: &str, venue: &str) -> HubRefusal {
    SymbolRefusal::PaperUnmounted { symbol: symbol.to_string(), venue: venue.to_string() }.into()
}

/// The refusal of a `paper` order on a paper-only node whose feeds reach no series of `symbol` ([`paper_unmounted_refusal`]):
/// a mount trades it, but one made while the node runs, or as a declared leg, and the feeds were wired at the start.
fn no_feed_reaches(symbol: &str, venue: &str) -> HubRefusal {
    SymbolRefusal::PaperUnfed { symbol: symbol.to_string(), venue: venue.to_string() }.into()
}

/// **[`paper_without_bars`] for a node with NO source to ask** (the paper-only arm, `flags.tradehub_live` off, holds no
/// lease registry: the default setup of a new box), decided from the strategy mounts the node itself publishes: a
/// `paper` engine's order on a symbol no published mount trades (as its own symbol or a declared leg) is refused in the
/// same words. `mounts` are the published rows the door holds for an order (`crate::publish`'s `named_mounts`). On a
/// paper-only node `fed` holds the series its feeds were wired for at its start (`ControlLimits::fed_series`): a symbol a
/// mount trades that none of them reaches (a runtime mount's, a declared leg) is refused too ([`no_feed_reaches`]). A
/// combo is judged leg by leg. A node WITH a source is judged by it inside [`plan_submit_symbol`] instead, and a venue
/// priced through its demo login by [`paper_without_demo_login`] there. The door and the Preview both ask it, just
/// before the plan.
pub(crate) fn paper_unmounted_refusal(
    cmd: &CommandWire,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
    mounts: &[vike_execution::MountView],
    fed: Option<&[vike_execution::SeriesKey]>,
) -> Option<HubRefusal> {
    if facts.is_some() {
        return None;
    }
    let one = |req: vike_tradehub_client::wire::OrderRequestWire| {
        (req.venue, req.account, vec![req.symbol])
    };
    let (venue, account, symbols): (String, Option<String>, Vec<String>) = match cmd {
        CommandWire::Submit(req) => one(req.clone()),
        CommandWire::Bracket(b) => one(bracket_entry_order(b)),
        CommandWire::OrderList(l) => one(super::order_list::planned_order(l)?),
        CommandWire::Combo(c) => (
            c.venue.clone(),
            c.account.clone(),
            c.legs.iter().map(|leg| leg.symbol.clone()).collect(),
        ),
        _ => return None,
    };
    let label = match account.as_deref() {
        None => AccountLabel::Default,
        Some(text) => parse_wire_account(text).ok()?,
    };
    let key = route_key_of(&venue, &label);
    let engine = blocks.iter().find(|b| b.route_key == key)?;
    let venue = engine.venue.as_str();
    if engine.tier != Some(AccountTier::Paper)
        || engine.symbol.is_empty()
        || crate::feeds::paper_quotes_through_demo(venue)
    {
        return None;
    }
    symbols.iter().find_map(|wanted| {
        let symbol = if engine.trades(wanted) { wanted.as_str() } else { engine.symbol.as_str() };
        let mounted =
            mounts.iter().filter(|m| m.kind == vike_execution::MountViewKind::Mount).any(|m| {
                (m.venue == venue && m.symbol == symbol)
                    || m.legs
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .any(|(v, s)| v == venue && s == symbol)
            });
        if !mounted {
            return Some(no_mount_trades(symbol, venue));
        }
        let reached = fed.is_none_or(|fed| fed.iter().any(|(v, s, _)| v == venue && s == symbol));
        (!reached).then(|| no_feed_reaches(symbol, venue))
    })
}

/// **The wait an OPENING order with no price of its own owes on the engine's OWN symbol: no trading without a price**
/// (the owner, 2026-10-30). With no quote in the tap the engine holds no mark, so such an order is judged at notional
/// zero: every cap passes, and on a grid with no `min_notional` (ctrader) nothing else stops it. So a committing
/// surface holds it for the first price, as a lane twin's own symbol is held, and [`await_price`] refuses it by name if
/// none comes. A close is never held ([`order_closes`]), a priced or triggered order names its reference price, a
/// strategy mount's feed prices its own engine ([`SymbolAdmitter::priced_by_mount`]), and a surface with no source has
/// nobody to ask (`market_reason` is all that binds it). A Preview subscribes nothing and holds nothing: it says what
/// the send would refuse, a feed that cannot be had, and "none yet" is [`no_price_note`]'s.
fn own_first_price(
    req: &vike_tradehub_client::wire::OrderRequestWire,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
    commit: bool,
    key: &str,
    engine: &VenueBlock,
) -> Result<Option<PriceWait>, HubRefusal> {
    let Some(source) = facts else { return Ok(None) };
    if engine.symbol.is_empty()
        || !crate::readmit::holds_own_lane(engine)
        || req.own_price().is_some()
        || order_closes(req, blocks)
        || source.priced_by_mount(&req.venue, &engine.symbol)
    {
        return Ok(None);
    }
    if commit {
        return Ok(Some(PriceWait { route_key: key.to_string(), venue: req.venue.clone() }));
    }
    match source.price(key, &req.venue, &engine.symbol, false) {
        Err(why) => Err(SymbolRefusal::NoPriceFeed { symbol: engine.symbol.clone(), why }.into()),
        Ok(_) => Ok(None),
    }
}

/// **The statement that makes `px` the engine's first mark for its own `symbol`, when the door held an order for it on
/// such an engine** ([`own_first_price`]): it rides the lane ahead of the order, so the first mark does not race the
/// tick, and a labelled engine, which no quote reaches on its own symbol, has one at all. No admission (the mounted arm
/// of `Engine::admit_symbol` writes this mark and nothing else, and the source spends no slot and tells the
/// daemon nothing), no facts look-up. `None` for an engine that books its own symbol through a rate (such a statement
/// must name the currency, and the engine would refuse it) and for a lane twin, whose wait carries an admission.
pub(crate) fn own_first_mark(
    wait: &PriceWait,
    symbol: &str,
    px: f64,
    made_ms: Option<i64>,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> Option<SymbolAdmission> {
    let engine = blocks.iter().find(|b| b.route_key == wait.route_key)?;
    (engine.symbol == symbol
        && engine.quote_rates.is_empty()
        && !is_twin_shaped(&wait.venue, &wait.route_key, engine, facts))
    .then(|| SymbolAdmission {
        route_key: wait.route_key.clone(),
        symbol: symbol.to_string(),
        grid: vike_model::SymbolGrid::default(),
        contract_value: None,
        initial_mark: Some(px),
        initial_mark_ms: made_ms,
        quote_currency: None,
        quote_rate: None,
        rate_as_of_ms: None,
        initial_margin_requirement: None,
        release: false,
        margin_mode: None,
    })
}

/// **The order names the engine's OWN symbol and the engine books it through a rate that has aged: the fresh
/// admission to send ahead of the order** (decision 0183, the mounted-symbol amendment; [`SymbolAdmitter::own_symbol_rate`]).
/// Only a COMMITTING surface (a Preview judges the same verdict without the lookup) and only a `demo` engine (a
/// conversion is demo-only: `quote_conversion_tier_refusal`) ask; every engine that books its own symbol at par
/// (all but a non-USD account on a USD-quoted mount) gets `None` from the source and the plan is the untouched pass. A
/// close asks nothing: the read is a gatewayhub round trip, and a stale rate denies only an opening order.
fn rerate_own_symbol(
    facts: Option<&dyn SymbolAdmitter>,
    commit: bool,
    key: &str,
    req: &vike_tradehub_client::wire::OrderRequestWire,
    blocks: &[VenueBlock],
    engine: &VenueBlock,
) -> Option<Box<SymbolAdmission>> {
    if commit
        && !engine.symbol.is_empty()
        && engine.tier == Some(AccountTier::Demo)
        && !order_closes(req, blocks)
        && let Some(source) = facts
    {
        source.own_symbol_rate(key, &req.venue, &engine.symbol, engine).map(Box::new)
    } else {
        None
    }
}

/// The plan for a symbol the engine ALREADY trades (a declared leg, or an earlier admission): the
/// order goes as written. An admission of ours that has grown stale (a venue changes a tick or lot
/// rarely, and announces it) is refreshed ahead of it by an ordinary `AdmitSymbol`; a Preview
/// (`commit` false) changes nothing.
///
/// **An engine that trades a symbol says nothing about its PRICE lane**: marks are not journaled, so a
/// symbol an earlier run admitted comes back from the restore trading and with no mark, and the lane is
/// subscribed only if the boot's re-admission reached it (v0.1.66 on the CI box: a restored flat ETHUSDT was
/// denied `no-reference-price` for ever). A real account's opening order therefore ASKS the price lane here too
/// (idempotent: a subscribed symbol is left alone), and with no price yet is held for the first one
/// ([`PriceWait`]) exactly as for a new symbol. A failed ask changes nothing: the engine's own
/// `no-reference-price` stays the fail-closed answer. A close asks nothing, and a due refresh waits for the next order
/// that opens: on a symbol booked through a rate it folds at the rate the engine holds, which the timer keeps fresh.
///
/// ⚠ **EXCEPT for a lane twin's own symbol, where it is no answer at all.** `no-reference-price` asks only of an
/// `on_demand` row and the twin's own symbol is its MOUNTED one, so an engine with no mark there judges a market
/// order at notional zero (every cap passes) and a limit order against itself. An opening order on it is therefore
/// refused when the price cannot be asked for, in words ([`engine_own_price_refusal`]), and the price it waits for
/// is the tap's `door_price`, which is "none yet" until the core was handed the symbol's mark (`DataClientSinkPriceTap`).
fn already_trading(
    facts: Option<&dyn SymbolAdmitter>,
    key: &str,
    venue: &str,
    symbol: String,
    req: &vike_tradehub_client::wire::OrderRequestWire,
    engine: &VenueBlock,
    commit: bool,
) -> SymbolPlan {
    // A close asks neither: both can sit on a gatewayhub round trip (a refresh and its commit, a price lane's facts) for
    // as long as the lease's `request_reply`, past the client's ack wait, and both exist for an order that opens.
    let closes = order_closes(req, std::slice::from_ref(engine));
    let admit = facts
        .filter(|_| commit && !closes)
        .and_then(|source| source.refresh(key, venue, &symbol))
        .filter(|a| a.route_key == key && a.symbol == symbol)
        .map(Box::new);
    let real = matches!(engine.tier, Some(AccountTier::Demo | AccountTier::Live));
    let asked = facts
        .filter(|_| commit && real && !closes)
        .map(|source| source.price(key, venue, &symbol, true));
    if real
        && !closes
        && symbol == engine.symbol
        && is_twin_shaped(venue, key, engine, facts)
        && let Some(why) = engine_own_price_refusal(key, &symbol, facts.is_some(), asked.as_ref())
    {
        return SymbolPlan::Refuse(why);
    }
    // A LIVE lane twin trades another symbol only at a leverage the exchange was SEEN holding for it.
    let mut admit = admit;
    if !closes && live_twin_extra_symbol(venue, key, engine, facts, &symbol) {
        let Some(source) = facts else {
            return SymbolPlan::Refuse(unconfirmed_leverage_refusal(key, &symbol));
        };
        if commit && source.confirmed_leverage(key, &symbol).is_none() {
            // The order asks the exchange AGAIN: the admission is restated so the door's commit runs, and the
            // door refuses the order itself if that commit confirms nothing ([`unconfirmed_leverage_refusal`]).
            admit = admit.or_else(|| {
                source
                    .admission(key, venue, &symbol)
                    .ok()
                    .filter(|a| a.route_key == key && a.symbol == symbol)
                    .map(Box::new)
            });
            if admit.is_none() {
                return SymbolPlan::Refuse(unconfirmed_leverage_refusal(key, &symbol));
            }
        }
    }
    let wait = (matches!(asked, Some(Ok(None))) && !closes)
        .then(|| PriceWait { route_key: key.to_string(), venue: venue.to_string() });
    SymbolPlan::Place { symbol, admit, wait }
}

/// **Whether `symbol` is an EXTRA symbol (not the mounted one) of a LIVE engine that has the lane twin's shape**:
/// the case where the leverage the exchange holds for the symbol must be the one the account stated (review r2,
/// finding 6). A symbol admitted into a twin is made to hold the account's figure by the gatewayhub's commit and
/// read back; when that is not confirmed, a DEMO twin trades it at the engine's own rate as it always did (the
/// exchange keeps whatever it held for the symbol, said in the gatewayhub's log), while a LIVE one must not: it
/// would trade real money at a leverage nobody chose.
pub(crate) fn live_twin_extra_symbol(
    venue: &str,
    key: &str,
    engine: &VenueBlock,
    facts: Option<&dyn SymbolAdmitter>,
    symbol: &str,
) -> bool {
    engine.tier == Some(AccountTier::Live)
        && symbol != engine.symbol
        && is_twin_shaped(venue, key, engine, facts)
}

/// The refusal for an opening order whose symbol would have to be admitted (and its leverage committed to the
/// exchange) while the engine's trading state is not `Active` ([`accept_command`]'s gate, review r2 item 9), or `None`
/// for an `Active` engine and one with no published block. A close goes without the admission: its caller's to ask.
pub(crate) fn admission_refusal_while_not_active(
    admission: &SymbolAdmission,
    blocks: &[VenueBlock],
) -> Option<HubRefusal> {
    let (route_key, symbol) = (&admission.route_key, &admission.symbol);
    let state = blocks
        .iter()
        .find(|b| b.route_key == *route_key)
        .map(|b| b.trading_state)
        .filter(|state| *state != vike_model::TradingState::Active)?;
    let state = match state {
        vike_model::TradingState::Halted => "this node is HALTED (the kill switch is on)",
        _ => "this node is REDUCING (only position-reducing orders are allowed)",
    };
    Some(
        SymbolRefusal::AdmissionWhileNotActive {
            state,
            symbol: symbol.clone(),
            route_key: route_key.clone(),
        }
        .into(),
    )
}

/// The refusal for an opening order on a LIVE lane twin's extra symbol whose leverage the exchange has not
/// confirmed ([`live_twin_extra_symbol`]).
pub(crate) fn unconfirmed_leverage_refusal(key: &str, symbol: &str) -> HubRefusal {
    SymbolRefusal::LeverageUnconfirmed { key: key.to_string(), symbol: symbol.to_string() }.into()
}

/// **An opening order with NO price of its own (a market or stop-market, whatever price the market order names:
/// `OrderRequestWire::limit_price`), or a bracket with a MARKET entry, on a
/// LIVE lane twin's own symbol**: the `(route key, symbol)` the door must size it for, or `None`. The door's notional ceiling cannot size a priceless
/// order on a linear contract (`ControlLimits::vet_notional`: `(None, Linear) => None`, "the core's to judge"),
/// and the core judges it at the engine's MARK, which a twin's own symbol may not have had yet (the mounted
/// symbol is priced in the twin only by the mark `DataClientSinkPriceTap` hands the core, and a cold or restored engine holds
/// none): notional zero, every cap passing. So on the one engine that reaches MAINNET through this path the
/// door sizes the order itself, at the price its own look-up holds ([`accept_command`]), and the ceiling binds
/// whatever the engine knows. What is this engine's alone is the refusal when that look-up holds nothing, and a
/// stop-market sized at the higher of its trigger and the price; every engine's market order is judged at the
/// price by `ControlLimits::market_reason` ([`market_order_to_size`]).
pub(crate) fn live_twin_unsized_open(
    cmd: &CommandWire,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> Option<(String, String)> {
    // A plain opening order with no price, or a BRACKET with a MARKET entry: the entry opens the position, and
    // the door's ceiling sizes a bracket by its priced legs alone (its stop-loss and take-profit, which a
    // client can set far under the entry's worth: `check_bracket` only asks the stop under the target).
    let (venue, symbol, account) = match cmd {
        CommandWire::Submit(req) if req.limit_price().is_none() && !order_closes(req, blocks) => {
            (&req.venue, &req.symbol, req.account.as_deref())
        }
        CommandWire::Bracket(b) if b.entry_price.is_none() => {
            (&b.venue, &b.symbol, b.account.as_deref())
        }
        // An ORDER LIST with an opening market leg (any of them), the bracket's market entry one verb over.
        CommandWire::OrderList(l) if !super::order_list::market_opening_legs(l).is_empty() => {
            (&l.venue, &l.symbol, l.account.as_deref())
        }
        _ => return None,
    };
    let label = match account {
        None => AccountLabel::Default,
        Some(text) => parse_wire_account(text).ok()?,
    };
    let key = route_key_of(venue, &label);
    let engine = blocks.iter().find(|b| b.route_key == key)?;
    // Both are USER names (decision 0290), one per kept row ignoring case: a match ignoring case is the engine's own
    // instrument on every venue, a case-kept hyperliquid coin included (`KPEPEUSDC.P`; audit HL-5).
    let own = symbol.trim().eq_ignore_ascii_case(&engine.symbol);
    (own && engine.tier == Some(AccountTier::Live) && is_twin_shaped(venue, &key, engine, facts))
        .then(|| (key, engine.symbol.clone()))
}

/// **The market order `cmd` is, for the door to judge at the price this node holds**: the `(route key, venue,
/// symbol)` of an OPENING market `Submit` or a bracket with a MARKET entry, on an engine the node published, or
/// `None` for anything else. A close is never sized ([`order_closes`]), as on the live twin. The word is read through
/// `OrderType::parse`, the door the order is lowered through. A blank symbol, or the engine's own in another case, is
/// the engine's spelling: the one its price is kept under.
///
/// Before the first publish no block answers, and a `Submit` is still judged: at a price the node holds, or at the
/// number it names (`ControlLimits::market_reason`), as it was before the door stopped reading that number as a
/// market order's price. An engine missing from a roster that IS published is the routing gates' refusal.
pub(crate) fn market_order_to_size<'a>(
    cmd: &'a CommandWire,
    blocks: &'a [VenueBlock],
) -> Option<(String, &'a str, &'a str)> {
    let (venue, symbol, account) = match cmd {
        CommandWire::Submit(req)
            if matches!(OrderType::parse(&req.order_type), Ok(OrderType::Market))
                && !order_closes(req, blocks) =>
        {
            (req.venue.as_str(), req.symbol.as_str(), req.account.as_deref())
        }
        CommandWire::Bracket(b) if b.entry_price.is_none() => {
            (b.venue.as_str(), b.symbol.as_str(), b.account.as_deref())
        }
        CommandWire::OrderList(l) if !super::order_list::market_opening_legs(l).is_empty() => {
            (l.venue.as_str(), l.symbol.as_str(), l.account.as_deref())
        }
        _ => return None,
    };
    let label = match account {
        None => AccountLabel::Default,
        Some(text) => parse_wire_account(text).ok()?,
    };
    let key = route_key_of(venue, &label);
    let symbol = symbol.trim();
    let Some(engine) = blocks.iter().find(|b| b.route_key == key) else {
        return (blocks.is_empty() && matches!(cmd, CommandWire::Submit(_)))
            .then_some((key, venue, symbol));
    };
    let own = symbol.is_empty() || symbol.eq_ignore_ascii_case(&engine.symbol);
    Some((key, venue, if own { engine.symbol.as_str() } else { symbol }))
}

/// `cmd`, the command [`live_twin_unsized_open`] or [`market_order_to_size`] found, with the price it lacks filled in at `px` so the door's
/// ceiling can size it: a plain order gets `px` as its price (a stop-market the higher of its trigger and `px`),
/// a bracket gets `px` as its entry price. `None` for any other command.
pub(crate) fn priced_for_sizing(cmd: &CommandWire, px: f64) -> Option<CommandWire> {
    match cmd {
        CommandWire::Submit(req) => {
            let mut sized = req.clone();
            sized.price = Some(sized.trigger_price.map_or(px, |trigger| trigger.max(px)));
            // This copy only carries the number to size at: left `market`, the ceiling would not read it.
            sized.order_type = OrderType::Limit.as_str().to_owned();
            Some(CommandWire::Submit(sized))
        }
        CommandWire::Bracket(b) => {
            let mut sized = b.clone();
            sized.entry_price = Some(px);
            Some(CommandWire::Bracket(sized))
        }
        // An order list's market opening leg gets `px` as its price, a plain order's rule.
        // An order list's market opening legs, EVERY one, get `px` as their price, a plain order's rule, so the
        // ceiling's per-leg arm sizes each (a straddle's second entry included).
        CommandWire::OrderList(l) => {
            let legs = super::order_list::market_opening_legs(l);
            if legs.is_empty() {
                return None;
            }
            let mut sized = l.clone();
            for i in legs {
                sized.legs[i].price = Some(px);
                sized.legs[i].order_type = OrderType::Limit.as_str().to_owned();
            }
            Some(CommandWire::OrderList(sized))
        }
        _ => None,
    }
}

/// **Why an opening order on a lane twin's OWN symbol cannot be judged**, or `None` (see [`already_trading`]):
/// the surface has no price source at all, or the source says no feed can be had. "None yet" is not here: the
/// order waits for the first price.
fn engine_own_price_refusal(
    key: &str,
    symbol: &str,
    has_source: bool,
    asked: Option<&Result<Option<f64>, String>>,
) -> Option<HubRefusal> {
    let (key, symbol) = (key.to_string(), symbol.to_string());
    if !has_source {
        return Some(SymbolRefusal::TwinNoPriceSource { key, symbol }.into());
    }
    match asked {
        Some(Err(why)) => {
            Some(SymbolRefusal::TwinNoPriceFeed { key, symbol, why: why.clone() }.into())
        }
        _ => None,
    }
}

/// Finish a placement of a NEW symbol: an OPENING order needs a price (the collar, the exposure caps
/// and the account margin are all judged at the mark, and an admitted symbol has none until its feed
/// delivers one). With `commit` the source subscribes the feed (once); a Preview only READS. A price
/// already here is a plain placement; none yet is a placement that must WAIT ([`PriceWait`], only
/// when committing); a feed that cannot be had is a refusal, `Err` with the source's words (the caller says which
/// engine refused, `SymbolRefusal::NewNoPriceFeed`). A reduce-only order is never held up or refused for want of a
/// price, but still subscribes the feed so the symbol is priced afterwards.
fn with_price(
    req: &vike_tradehub_client::wire::OrderRequestWire,
    facts: Option<&dyn SymbolAdmitter>,
    key: &str,
    venue: &str,
    symbol: String,
    mut admit: Option<Box<SymbolAdmission>>,
    commit: bool,
) -> Result<SymbolPlan, String> {
    let mut wait = None;
    if let Some(source) = facts {
        match source.price(key, venue, &symbol, commit) {
            // The price seen NOW rides the admission as the engine's first mark for the symbol, with
            // the time it was made.
            Ok(Some(px)) => {
                if let Some(a) = admit.as_mut() {
                    a.initial_mark = Some(px);
                    a.initial_mark_ms = source.price_made_ms(venue, &symbol, px);
                }
            }
            Ok(None) if !req.reduce_only && commit => {
                wait = Some(PriceWait { route_key: key.to_string(), venue: venue.to_string() });
            }
            Ok(None) => {}
            Err(why) if !req.reduce_only => return Err(why),
            Err(_) => {}
        }
    }
    Ok(SymbolPlan::Place { symbol, admit, wait })
}

/// The plain OPENING order a bracket's entry is (a limit at its price, a market without one), for
/// [`plan_submit_symbol`] only. It is never sent: the bracket lowers to its own `OrderIntent::Bracket`.
fn bracket_entry_order(
    b: &vike_tradehub_client::wire::OrderBracketWire,
) -> vike_tradehub_client::wire::OrderRequestWire {
    vike_tradehub_client::wire::OrderRequestWire {
        order_client_id: String::new(),
        venue: b.venue.clone(),
        symbol: b.symbol.clone(),
        side: b.side,
        qty: b.qty,
        order_type: if b.entry_price.is_some() { "limit" } else { "market" }.to_string(),
        price: b.entry_price,
        trigger_price: None,
        reduce_only: false,
        account: b.account.clone(),
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    }
}

/// Whether judging this command would cost the facts source a lookup: a Submit on an admitting venue
/// for a symbol the addressed engine does not already trade. The Preview takes a rate token for
/// exactly these (a lookup is a gatewayhub round trip a peer could otherwise flood for free).
pub(crate) fn needs_lookup(cmd: &CommandWire, blocks: &[VenueBlock]) -> bool {
    let CommandWire::Submit(req) = cmd else { return false };
    let Some(rule @ VenueSymbolRule::Admits(_)) = rule_of(&req.venue) else {
        return false;
    };
    submit_engine(cmd, blocks).is_some_and(|b| {
        (!b.symbol.is_empty() || account_wide(rule))
            && req.symbol != b.symbol
            && !b.trades(&req.symbol)
    })
}

/// The published block of the engine a Submit addresses (the same composer of the route key as
/// `account_refusal`), or `None` for any other command, a label that does not parse and an engine that
/// published no block.
fn submit_engine<'a>(cmd: &CommandWire, blocks: &'a [VenueBlock]) -> Option<&'a VenueBlock> {
    let CommandWire::Submit(req) = cmd else { return None };
    let label = match req.account.as_deref() {
        None => AccountLabel::Default,
        Some(text) => parse_wire_account(text).ok()?,
    };
    let key = route_key_of(&req.venue, &label);
    blocks.iter().find(|b| b.route_key == key)
}

/// **The contract value of the symbol a Submit names when its engine does not trade it yet**, from the
/// symbol's own facts, in the ACCOUNT's money (`effective_contract_value`). The notional ceiling at the
/// door sizes with it: the engine's published grid lists no row for an unadmitted symbol, so an INVERSE
/// one (an amount of US dollars, no price in its notional) would be sized as `qty · price`. It runs the
/// plan in its Preview form, which reads the facts (cached) and subscribes and refreshes nothing.
///
/// `Ok(None)` is "the grid has the row" (traded already, or not a lookup at all) and "the facts say the
/// default". **`Err(why)` is the plan REFUSING the symbol, in its own words, and the ceiling must not size
/// it**: the grid has no row and the facts gave no value, so the engine's default value would count the
/// pair's own QUOTE money (`qty · price`: 157,000 for 1,000 USDJPY, yen) against a ceiling that is stated
/// in the account's (`ControlLimits::notional_reason`'s default value). The caller waits for the symbol
/// door's verdict instead (`accept_command` refuses with it before the committing plan; the Previews end
/// in [`submit_symbol_refusal`], which is the same plan), so a refused symbol is refused for the reason
/// that is true and an unsized one is never let through.
///
/// **`Ok(Some(row))` also answers a symbol the engine ALREADY trades that was typed in another spelling**
/// (`usdjpy` for the mounted `USDJPY`: one user name in any case, decision 0290): the plan
/// places it on the engine's spelling, and the grid holds the value under THAT name. The ceiling used to look
/// the typed name up, found no row and sized the order at the default value, a figure that is wrong for any
/// engine whose row is not `Linear` 1.0 (an inverse contract, or a mount booked through a rate).
pub(crate) fn foreign_contract_value(
    cmd: &CommandWire,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> Result<Option<vike_model::ContractValue>, HubRefusal> {
    if !needs_lookup(cmd, blocks) {
        return Ok(None);
    }
    match plan_submit_symbol(cmd, blocks, facts, false) {
        SymbolPlan::Refuse(why) => Err(why),
        SymbolPlan::Place { symbol, admit, .. } => {
            if let Some(engine) = submit_engine(cmd, blocks)
                && engine.trades(&symbol)
            {
                return Ok(Some(engine.contract_value_of(&symbol)));
            }
            Ok(admit.as_deref().and_then(effective_contract_value))
        }
        SymbolPlan::Pass => Ok(None),
    }
}

/// The refusal the Preview surfaces print: [`plan_submit_symbol`]'s `Refuse`, then the door's post-only
/// crossing check on the symbol the plan would send the order on (`post_only_cross_refusal`: the same call
/// `accept_command` makes, so a preview and a command answer one frame alike while the tap holds the same book).
/// A PREVIEW: it reads (a facts lookup is cached and read-only) and never subscribes a price feed,
/// refreshes a grid or waits.
pub(crate) fn submit_symbol_refusal(
    cmd: &CommandWire,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> Option<HubRefusal> {
    match plan_submit_symbol(cmd, blocks, facts, false) {
        SymbolPlan::Refuse(why) => Some(why),
        SymbolPlan::Pass => {
            super::post_only_door::post_only_cross_refusal(cmd, None, blocks, facts)
                .or_else(|| held_grid_refusal(cmd, None, &[], blocks, facts))
        }
        SymbolPlan::Place { symbol, .. } => {
            super::post_only_door::post_only_cross_refusal(cmd, Some(&symbol), blocks, facts)
                .or_else(|| held_grid_refusal(cmd, Some(&symbol), &[], blocks, facts))
        }
    }
}

/// `cmd` as the send would carry it, for a Preview: on the symbol the plan places it on. The price the node holds is kept
/// under that spelling (`ethusdt` market, `ETHUSDT` priced), and the send sizes after its plan. A PREVIEW's plan: it
/// reads and subscribes nothing.
pub(crate) fn as_planned(
    cmd: &CommandWire,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> CommandWire {
    let mut planned = cmd.clone();
    if let (CommandWire::Submit(req), SymbolPlan::Place { symbol, .. }) =
        (&mut planned, plan_submit_symbol(cmd, blocks, facts, false))
    {
        req.symbol = symbol;
    }
    planned
}

/// **What a Preview cannot say about an order it accepted: the market order `cmd` is has no price held yet**, so the
/// ceiling did not size it. The send holds it for the first price and judges it at that, and may refuse what this
/// accepted. `None` for a `paper` engine, which holds no order for a price, and a symbol a strategy mount prices.
fn no_price_note(
    cmd: &CommandWire,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> Option<String> {
    let (key, venue, symbol) = market_order_to_size(cmd, blocks)?;
    let engine = blocks.iter().find(|b| b.route_key == key)?;
    let source = facts?;
    let held = matches!(engine.tier, Some(AccountTier::Demo | AccountTier::Live))
        && !source.priced_by_mount(venue, symbol);
    (held && matches!(source.price(&key, venue, symbol, false), Ok(None))).then(|| {
        format!(
            "no price is held for `{symbol}` yet, so this preview could not size the order: the send waits \
             for the first price, judges it at that price, and refuses it if none comes"
        )
    })
}

/// What a Preview says beside an order it accepted, on the symbol the send would carry ([`as_planned`]): what its
/// quantity goes out as ([`rounding_note`]) and that no price is held for it ([`no_price_note`]).
pub(crate) fn preview_notes(
    cmd: &CommandWire,
    orders: &[vike_execution::OrderView],
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> Vec<String> {
    let planned = as_planned(cmd, blocks, facts);
    rounding_note(&planned, None, orders, blocks, facts)
        .into_iter()
        .chain(no_price_note(&planned, blocks, facts))
        .collect()
}

/// How long the door holds an opening order for the first price of a symbol whose feed it just
/// subscribed. A pump delivers its first quote in well under a second; past this the order is
/// refused and the operator retries (the subscription stays), unless the price lane says its stream is
/// down ([`SymbolAdmitter::lane_verdict`]), which the refusal then carries instead of "retry".
///
/// ⚠ Built from `vike_config::config::DOOR_PRICE_WAIT_MS`, the number the CLIENT's default ack wait
/// (`config.node_ack_wait_ms`) is derived from, so the client outlasts this hold and reads its refusal.
/// Moving the hold means moving that one constant; `the_default_ack_wait_outlasts_the_doors_price_hold`
/// (`server/tests/admits_any_symbol.rs`) is the pin that the two cannot drift apart.
pub(crate) const PRICE_WAIT: std::time::Duration =
    std::time::Duration::from_millis(vike_config::config::DOOR_PRICE_WAIT_MS as u64);

/// Hold the order until `wait`'s symbol has a price, or say why not. Blocks the CONNECTION thread
/// (up to [`PRICE_WAIT`]), never the core, and never at all for a source that does not
/// [`SymbolAdmitter::waits`] (it gets one look and a prompt "retry").
pub(crate) fn await_price(
    facts: &dyn SymbolAdmitter,
    wait: &PriceWait,
    symbol: &str,
) -> Result<f64, HubRefusal> {
    let deadline = std::time::Instant::now() + PRICE_WAIT;
    loop {
        match facts.price(&wait.route_key, &wait.venue, symbol, true) {
            Ok(Some(px)) => return Ok(px),
            Ok(None) if facts.waits() && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) => {
                let held = facts.waits().then(|| PRICE_WAIT.as_secs());
                // The lane's own verdict beats "retry": a stream in a gap does not deliver by waiting.
                let refusal = match facts.lane_verdict(&wait.route_key, &wait.venue, symbol) {
                    Some(verdict) => SymbolRefusal::PriceStreamDown {
                        symbol: symbol.to_string(),
                        held: held.map_or_else(String::new, |s| format!(" after {s} s")),
                        verdict,
                    },
                    None => SymbolRefusal::NoPriceYet {
                        symbol: symbol.to_string(),
                        held: held.map_or_else(String::new, |s| format!(" in {s} s")),
                    },
                };
                return Err(refusal.into());
            }
            Err(why) => {
                return Err(SymbolRefusal::NoPriceFeed { symbol: symbol.to_string(), why }.into());
            }
        }
    }
}
