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
//! * **[`SymbolRule::Admits`]: bybit, binance, aster, okx, hyperliquid, deribit, alpaca, ctrader, oanda, ig, ibkr, dukascopy, fxcm.** The adapter signs each order
//!   on the ORDER's symbol, on that symbol's own grid (`crates/bridges/binance/src/per_symbol.rs`'s
//!   `PerSymbolRest`; okx's also on that instrument's own contract size; hyperliquid's through its loaded
//!   `Symbology`, which keys the asset index by the order's coin; deribit's through one client that
//!   resolves each instrument's row on first use), so the door admits the symbol into the engine first.
//!   **deribit admits PERPETUALS of the engine's own money model only** ([`Spelling::PerpetualName`],
//!   `money_model_refusal`): an engine counts money one way, so an inverse engine (an amount of US
//!   dollars) takes other inverse perpetuals and a linear one (an amount of the base coin) other linear
//!   ones; `docs/ops/deribit-any-symbol-plan.md` says why and what mixing would take. **alpaca admits
//!   equities and crypto pairs** ([`Spelling::AssetSymbol`]): every instrument is a USD-quoted share or coin
//!   with a multiplier of 1, so the engine's `qty x price` is the real USD exposure whatever the symbol;
//!   its facts are read with the account's own credentials (`docs/ops/any-symbol-remaining-venues-plan.md`).
//!   **ctrader admits FOREX PAIRS and the CFD classes whose unit of volume was measured** (metals, indices,
//!   oil and commodities, crypto: `vike_ctrader::catalog::MEASURED_CFD_CLASSES`; [`Spelling::BrokerName`], the
//!   broker's name with its case kept): the
//!   adapter resolves each order's symbol in the account's own symbol list and snaps it on that symbol's
//!   volume grid. An instrument quoted in the account's deposit currency is admitted as it is; one quoted in another
//!   currency is admitted for a `demo` account with an account-wide ceiling armed, with the conversion the
//!   facts carry (decision 0183: the gatewayhub reads the rate from a one-shot session of the account's own,
//!   `ctrader_symbol_facts_quoted`). What a share, a bond or a dated-futures CFD's unit means is unmeasured, so the gatewayhub's facts (read
//!   from the account's own demo session, `ctrader_symbol_facts`) refuse every other instrument by name. Its price handle is its MOUNT's
//!   `LiveFeeds::Ctrader` or, on a box that mounts no ctrader, the PRICE-ONLY feed
//!   (`crate::feeds::wire_price_only_feeds`), and its reconcile client reads every admitted symbol.
//!   **oanda admits instruments QUOTED IN THE ACCOUNT'S CURRENCY** ([`Spelling::OandaInstrument`]; the
//!   gatewayhub's plain facts refuse any other, because at par a USDJPY position on a USD account would be
//!   valued about 150 times too high; since decision 0183 an account on the `demo` tier with an account-wide
//!   exposure ceiling is the exception, `quote_conversion_refusal`: its node asks for OANDA's own
//!   conversion rate and the engine books the pair's money through it), whose unit step is 1 (the
//!   adapter rounds to whole units) and, for a currency pair, whatever price precision its own row states
//!   (the adapter writes exactly that many decimals: five for EURUSD, three for a JPY pair; any other class
//!   keeps the five decimals the adapter was proven on); its price handle is its MOUNT's
//!   `LiveFeeds::Oanda` or, on a box that mounts no OANDA market, the PRICE-ONLY feed
//!   (`crate::feeds::wire_price_only_feeds`; without it an unmounted account would lose every foreign
//!   pair), and its reconcile client reads every admitted instrument.
//!   **ig admits CURRENCIES epics quoted in a USD account's currency** ([`Spelling::Epic`]; and, for a
//!   `demo` account with an account-wide ceiling armed, in another currency too, converted at the rate
//!   IG's market details state and a price confirms: [`QUOTE_CONVERTING_VENUES`], decision 0183): the facts
//!   (`vike_ig::market_facts`, over the account's own login) refuse by name every other epic class, because
//!   what `size` means per class is unmeasured, and admit an FX epic only when its own payload agrees
//!   about the contract multiplier; its price handle is its MOUNT's `LiveFeeds::Ig` or, on a box that
//!   mounts no ig, the PRICE-ONLY feed (a quote lane per admitted epic, at most `IG_MAX_QUOTE_LANES`), and its reconcile client reads every
//!   admitted epic.
//!   **ibkr admits one US stock by NAME** ([`Spelling::StockName`], `TICKER.SMART.CCY`): IBKR publishes no
//!   instrument list, so the gatewayhub asks the account's gateway (a symbol search, contract details, market
//!   rules) and refuses an ambiguous ticker, another currency than the engine's, and every other kind of
//!   contract by name. Its price lane is a price-only feed (`feeds/ibkr.rs`) whose tap prices a two-sided quote of
//!   any LABELLED stream, a delayed or frozen one included (decision 0140's amendment of 2026-10-26: the label is
//!   recorded with the order, never a gate), and with no ibkr lane on the datahub the door answers `no price feed`
//!   to an opening order on a new stock. ⚠ The node's own door has not run end to end against a gateway
//!   (`docs/ops/ibkr-socket-gateway.md`, "Delayed data prices a new stock").
//!   A `demo` account with an account-wide ceiling armed, whose ledger the engine's assembly anchored to the
//!   base currency IBKR states (decision 0183, `unanchorable_mount`), is also admitted a stock of ANOTHER
//!   currency, with the conversion the facts carry (`stock_conversion_missing`); a forex pair is a CASH
//!   position and is refused in words (`canonical_ibkr_stock`).
//!   **dukascopy admits a currency PAIR** ([`Spelling::PairName`]; quoted in the engine's own money, or
//!   in another one for a converting `demo` account, below):
//!   its facts are the committed JForex SDK table's (static, offline: `crates/bridges/dukascopy/src/any_symbol.rs`'s
//!   `instrument_facts`), every CFD, both metals (their amount is in ounces, but a metal order through the
//!   sidecar's smallest size is a thousand ounces, so they stay refused until a demo order proves a metal
//!   path) and every name the table lacks is refused by name there, and the engine's
//!   own pair under another spelling (`EUR/USD`) is placed on the engine's spelling. Its price lane is a
//!   price-only feed (`feeds/dukascopy.rs`) over the quotes of the JForex sidecar the gatewayhub already runs: a
//!   pair is priced only while a sidecar whose jar announces `ticks` is up, from a real-time two-sided quote,
//!   and with none the door answers `cannot subscribe dukascopy's prices for ...: ...` (or `no price feed` when
//!   the datahub advertises no dukascopy lane). ⚠ Nothing of it has run against a JForex login.
//!   A pair quoted in ANOTHER money (`USDJPY`, `EURGBP`) is admitted for a `demo` account with an account-wide
//!   ceiling armed (decision 0183), only with the currency and the rate the facts state: the gatewayhub prices
//!   it from the sidecar's own ticks of the conversion pair, and the fence above is what that relaxes.
//!   **fxcm admits a regular forex PAIR** ([`Spelling::PairName`], as dukascopy: `EURUSD`, `EUR/USD` read as the
//!   same pair) under the SAME fence: its facts are the account's own running reconcile session's Offers table
//!   (`fxcm_rate` in the gatewayhub, no second login), which refuses every CFD, metal and index by name, and a
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
//!   Trades snapshots once per told pair: `RECON_READS_ADMITTED` names the venue).
//! * **[`SymbolRule::MountedOnly`]: no roster venue today.** An adapter that signs EVERY order on the
//!   symbol its engine was mounted on, whatever the order names, has this row: the door REFUSES an
//!   order on any other symbol, with the row's reason, until that adapter is finished. Deleting a row
//!   here is the whole change that opens a venue, once its adapter reads the order's symbol (deribit's
//!   row was the last one).
//! * **[`SymbolRule::PerOrder`]: polymarket.** Its adapter already reads the
//!   order's own symbol and the engine judges it well enough; the door says nothing about WHICH symbol.
//!   `per_order_refusal` keeps two fences for a symbol other than the engine's own, for a venue that
//!   would hold this row (oanda did, until it became `Admits`): a paper account is refused (one book,
//!   fills stamped with the mounted symbol), and an opening order with no limit price is refused (no
//!   price watcher, so the engine would judge it at notional 0). A reduce-only order is not touched, and
//!   polymarket (every token is judged) is exempt from both, so today they bind no venue.
//! * **[`SymbolRule::PerOrderUnsafe`]: no roster venue today** (fxcm's was the last, until its price lane).
//!   An adapter that reads the order's symbol but whose ENGINE cannot judge a foreign one (it rounds on the
//!   mounted symbol's grid, knows no multiplier or has no price) has this row: the door REFUSES a foreign
//!   symbol with the row's reason until admission supports the venue (an interim fence; deleting the row
//!   opens it).
//!
//! # What the door does for an `Admits` venue ([`plan_submit_symbol`])
//!
//! 1. The engine's OWN symbol, spelled as the engine spells it, passes UNTOUCHED: no extra command,
//!    byte-identical, no price asked and no wait. Since decision 0197 a COMMITTING surface also tells the source
//!    the engine's own symbol is being ordered in ([`keep_own_lane`], [`SymbolFacts::hold_lane`]): the source
//!    keeps that symbol's price lane open while the engine holds something in it, off the order's thread, and
//!    the plan is unchanged. A declared leg or an earlier admission (this run's, or a restored one) passes the
//!    spelling and admission steps but still asks the price lane (step 6) on a `demo`/`live` engine.
//! 2. Otherwise the symbol is CANONICALISED to the venue's spelling: upper case; binance and aster
//!    keep the `.P` lane suffix (`vike_catalog::split_perp`), bybit's exec plane is linear perpetuals
//!    only and its engine symbols are bare, so a `.P` is dropped; okx takes its own SWAP instId
//!    (`ETH-USDT-SWAP`) and nothing else ([`canonical_swap_instid`]: a spot, dated or option id is
//!    refused by name, `BTCUSDT.P` is not translated); hyperliquid's coin names keep their case
//!    (`kPEPE`) and have no lane (a spot pair, `@N` and a builder-dex `dex:COIN` are refused by name). A
//!    symbol with a character no instrument has is refused (it reaches a signed query string downstream).
//! 3. Only a `demo` or `live` engine takes a foreign symbol: a paper engine is one book.
//! 4. A binance or aster engine is ONE lane (spot or USDⓈ-M perp, by its mounted symbol): a symbol of
//!    the other lane is refused here, by name, instead of by a late `OrderRejected` (only the mounted
//!    lane has a user-data stream, so its fills would never arrive).
//! 5. The facts come from the [`SymbolFacts`] the surface was given, off the fold thread. Nothing is
//!    ever guessed: a symbol the venue does not list, a failed lookup, a grid without a tick or a lot,
//!    a surface with no source and a 17th extra symbol are each a refusal naming the cause. An OPENING
//!    order for a venue this node holds no price watcher for ([`SymbolFacts::price_source`]) is refused
//!    BEFORE the lookup (ibkr's lookup is a budgeted data connect), in step 6's words.
//! 6. An OPENING order on a symbol other than the engine's own needs a PRICE (the collar, the exposure
//!    caps and the account margin are judged at the mark). Asking [`SymbolFacts::price`] subscribes the
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

use vike_exec::{MAX_EXTRA_SYMBOLS, SymbolAdmission, VenueBlock};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::{
    AccountLabel, RESERVED_DEFAULT_LABEL, parse_wire_account, route_key_of,
};
use vike_tradehub_client::wire::{WireCommand, WireVenueLinkDown};

/// Where a surface gets the FACTS of a symbol its engine was not mounted on. Blocking (a lookup may
/// cross the network), so it is called on a connection thread, never on the core's fold; a
/// production source caches.
pub trait SymbolFacts: Send + Sync {
    /// Whether this node can price ANY symbol of `venue` at all (a price watcher exists for it), judged
    /// WITHOUT a network call. `Err` is the reason, in an operator's words. The door asks it BEFORE a facts
    /// lookup for an opening order on a new symbol: a lookup can cost a real connect (ibkr's data
    /// connection is budgeted per hour), and an order no price can ever back must not spend it. The default
    /// answers `Ok`: a source that does not know does not block ([`Self::price`] still refuses later).
    fn price_source(&self, _venue: &str) -> Result<(), String> {
        Ok(())
    }

    /// The admission that puts `symbol` (already canonical for `venue`) into the engine addressed by
    /// `route_key`: its `grid` from the venue's own instrument row and its contract value (`None` for
    /// the default). `Err` is the reason, in words an operator reads verbatim: an unknown symbol, an
    /// unreachable venue, an adapter that answers nothing.
    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, String>;

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

    /// The door sent `AdmitSymbol` for `symbol` (or the boot did): the source may now keep its price
    /// subscription for good and refresh its facts later ([`Self::refresh`]).
    fn admitted(&self, _route_key: &str, _venue: &str, _symbol: &str) {}

    /// [`Self::admitted`] for a caller that sends the order right after and so cannot state what the commit
    /// confirms ahead of it ([`FailFast`]'s, run off the Telegram poller's thread): a leverage the commit
    /// brings is stated by the symbol's next [`Self::refresh`] instead. The default is a source with no
    /// commit to defer.
    fn admitted_detached(&self, route_key: &str, venue: &str, symbol: &str) {
        self.admitted(route_key, venue, symbol);
    }

    /// The door gave up on `symbol` BEFORE admitting it (no price came): drop the price subscription
    /// [`Self::price`] opened for it. A no-op for a symbol that was admitted meanwhile.
    fn release(&self, _route_key: &str, _venue: &str, _symbol: &str) {}

    /// **An order names the engine's OWN symbol** (decision 0197): keep that symbol's price lane open while the
    /// engine holds a position or an order in it. The engine's own symbol is mounted, so the door asks no price
    /// for it ([`plan_submit_symbol`] passes it), and nothing else subscribes it unless a strategy is mounted
    /// there. A source that holds the lane opens it off the caller's thread: this call NEVER WAITS for a price,
    /// and never refuses (a limit order a closed market accepts must stay accepted, and a first market order is
    /// judged as it always was). Called only for a committing surface, a `demo`/`live` engine and an order that
    /// names the symbol; idempotent. The default is a source that holds no lanes.
    fn hold_lane(&self, _route_key: &str, _venue: &str, _symbol: &str) {}

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

    /// Whether the door may hold an order for a price ([`await_price`]). A surface that must not
    /// block on a slow lookup (the Telegram poller runs the whole acceptance inline, and a /panic
    /// queues behind it) answers `false` and gets a prompt "no price yet, retry" instead.
    fn waits(&self) -> bool {
        true
    }

    /// Whether the account `route_key` runs with an ACCOUNT-wide exposure ceiling armed
    /// (`policy.max_account_exposure`, or the account's own `max_exposure` row). `max_total_exposure`
    /// bounds ONE symbol, so a live account holding seventeen would carry seventeen times it; the door
    /// admits a symbol into a `live` account only when this is `true`. The default answers `true`: a
    /// source that does not know does not block (the production source answers from the settings).
    fn exposure_cap_armed(&self, _route_key: &str) -> bool {
        true
    }

    /// **Whether the account `route_key`, started in this running node, is still waiting for its venue
    /// balance** (decision 0169): `Some(how long since the start)` while its engine judges orders on the
    /// seed cash instead of what the venue holds, `None` once the balance landed and for every account that
    /// never waits (the boot's engines, a `paper` account, a venue with no reconcile leg). The door refuses
    /// an OPENING order for such an account ([`super::refusal::awaiting_balance_refusal`]). The default is
    /// `None`: a source that does not know holds nothing.
    fn awaiting_balance(&self, _route_key: &str) -> Option<std::time::Duration> {
        None
    }

    /// The account's ENGINE GENERATION: moves whenever the node starts or stops the account's engine.
    /// Work done on an engine's behalf later than the engine's own start (the boot's retry of the
    /// symbols it missed) captures it and ends when it moves. A source that never sees an engine
    /// change answers a constant. REQUIRED, no default: an implementor that forgot it would silently
    /// switch the retry's guard off.
    fn generation(&self, route_key: &str) -> u64;

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

    /// **Whether the engine under `route_key` is an operator's REAL account that spells a lane twin's label**
    /// (`vike_secrets::lane_twin`), as the account watcher last read the rows. The published block cannot say:
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

    /// **Whether the lane twin `route_key` is kept up only to be CLOSED**: its derived row is gone (the margin
    /// leverage is no longer stated) while it still holds a position or an order, so the watcher does not stop
    /// it ([`crate::account_follow`]'s `follow_lane_twins`). It takes no new position: the door refuses an
    /// opening order for it ([`super::refusal::lane_twin_closing_refusal`]) and lets every reducing one through.
    /// The default is `false`.
    fn lane_twin_closing_only(&self, _route_key: &str) -> bool {
        false
    }

    /// **The accounts whose venue balance this node HOLDS BACK from their engine** (the node-side balance gate,
    /// `remote_backend::balance_gate`), as `(route key, short reason)` in route-key order: the reason is the
    /// words an operator surface shows (`assumed USD`, `stated EUR not anchored`, `untold foreign stock`).
    /// The publisher stamps it on every frame ([`vike_tradehub_client::wire::WireSnapshot::balance_held`]). The
    /// default is none: a source with no reconcile gate holds nothing.
    fn balance_held(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// **The accounts whose VENUE PATH is down right now**, as the gatewayhub's status report last said it
    /// (`TradeStatusReport::venue_link`, polled by the lease's worker: `remote_backend::reteach`), in route-key
    /// order. The publisher stamps it on every frame
    /// ([`vike_tradehub_client::wire::WireSnapshot::venue_link_down`]) and the doctor's engine row reads it. The
    /// default is none: a source with no lease reports no path. ⚠ A wrapper must forward this verb like every
    /// other, or the production source answers the default.
    fn venue_link_down(&self) -> Vec<WireVenueLinkDown> {
        Vec::new()
    }
}

/// **The admission to send AFTER `admission` when the daemon's commit confirmed a leverage for its
/// symbol**: `admission` restated with `im_requirement = 1 / leverage` (the symbol's own margin rate,
/// which the engine accepts on a refresh and writes to its per-symbol row), or `None` when the source
/// confirmed nothing, the figure is not a leverage (below 1x or not finite: the engine would refuse the
/// whole admission with `bad-im-requirement`), or `admission` already says that rate. The caller sends
/// it ahead of the order it is judging, on the lane the first admission rode.
pub(crate) fn with_confirmed_leverage(
    source: &dyn SymbolFacts,
    admission: &SymbolAdmission,
) -> Option<SymbolAdmission> {
    let leverage = source.confirmed_leverage(&admission.route_key, &admission.symbol)?;
    if !(leverage.is_finite() && leverage >= 1.0) {
        return None;
    }
    let im = 1.0 / leverage;
    (admission.im_requirement != Some(im))
        .then(|| SymbolAdmission { im_requirement: Some(im), ..admission.clone() })
}

/// A [`SymbolFacts`] that never makes its caller wait for a price: everything else is the wrapped
/// source's. The Telegram channel's source.
pub struct FailFast(pub std::sync::Arc<dyn SymbolFacts>);

impl SymbolFacts for FailFast {
    fn price_source(&self, venue: &str) -> Result<(), String> {
        self.0.price_source(venue)
    }
    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, String> {
        self.0.admission(route_key, venue, symbol)
    }
    fn price(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
        subscribe: bool,
    ) -> Result<Option<f64>, String> {
        self.0.price(route_key, venue, symbol, subscribe)
    }
    /// **Never waits for the commit.** The Telegram poller runs the whole acceptance inline, so a chat order
    /// on a new symbol that waited for the daemon's commit (on a venue that posts a leverage: three or four
    /// signed reads, at most one write, a poll, worst case tens of seconds) would hold every other chat
    /// command behind it, a `/panic` included. The bookkeeping and the commit run on a thread of their own;
    /// the order is judged at the engine's own rate, and the figure the commit confirms is stated by the
    /// symbol's next refresh ([`SymbolFacts::admitted_detached`]). The number of such threads is bounded by
    /// the engine's own cap on admitted symbols (`MAX_EXTRA_SYMBOLS` per account), each ends within the
    /// lease's reply bound, and the gatewayhub answers a fifth settle in flight `FACTS_BUSY` at once.
    fn admitted(&self, route_key: &str, venue: &str, symbol: &str) {
        let inner = std::sync::Arc::clone(&self.0);
        let (route_key, venue, symbol) =
            (route_key.to_string(), venue.to_string(), symbol.to_string());
        let spawned = std::thread::Builder::new().name("symbol-commit".into()).spawn({
            let (route_key, venue, symbol) = (route_key.clone(), venue.clone(), symbol.clone());
            move || inner.admitted_detached(&route_key, &venue, &symbol)
        });
        if let Err(e) = spawned {
            tracing::warn!(
                %route_key, %venue, %symbol,
                "could not start the thread that tells the gatewayhub this symbol is admitted ({e}); \
                 its reconcile client will not read it until the next admission request succeeds"
            );
        }
    }
    fn admitted_detached(&self, route_key: &str, venue: &str, symbol: &str) {
        self.0.admitted_detached(route_key, venue, symbol);
    }
    fn release(&self, route_key: &str, venue: &str, symbol: &str) {
        self.0.release(route_key, venue, symbol);
    }
    /// Forwarded: the wrapped source's claim is a lock and its lane opens on a thread of its own, so the
    /// Telegram poller never waits for it.
    fn hold_lane(&self, route_key: &str, venue: &str, symbol: &str) {
        self.0.hold_lane(route_key, venue, symbol);
    }
    fn refresh(&self, route_key: &str, venue: &str, symbol: &str) -> Option<SymbolAdmission> {
        self.0.refresh(route_key, venue, symbol)
    }
    fn own_symbol_rate(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
        engine: &VenueBlock,
    ) -> Option<SymbolAdmission> {
        self.0.own_symbol_rate(route_key, venue, symbol, engine)
    }
    fn waits(&self) -> bool {
        false
    }
    fn exposure_cap_armed(&self, route_key: &str) -> bool {
        self.0.exposure_cap_armed(route_key)
    }
    fn awaiting_balance(&self, route_key: &str) -> Option<std::time::Duration> {
        self.0.awaiting_balance(route_key)
    }
    fn generation(&self, route_key: &str) -> u64 {
        self.0.generation(route_key)
    }
    fn tell_daemon(&self, route_key: &str, venue: &str, symbol: &str) -> Result<(), String> {
        self.0.tell_daemon(route_key, venue, symbol)
    }
    fn confirmed_leverage(&self, route_key: &str, symbol: &str) -> Option<f64> {
        self.0.confirmed_leverage(route_key, symbol)
    }
    fn real_account_under_twin_label(&self, route_key: &str) -> bool {
        self.0.real_account_under_twin_label(route_key)
    }
    fn derived_lane_twin(&self, route_key: &str) -> bool {
        self.0.derived_lane_twin(route_key)
    }
    fn lane_twin_closing_only(&self, route_key: &str) -> bool {
        self.0.lane_twin_closing_only(route_key)
    }
    fn balance_held(&self) -> Vec<(String, String)> {
        self.0.balance_held()
    }
    fn venue_link_down(&self) -> Vec<WireVenueLinkDown> {
        self.0.venue_link_down()
    }
}

/// How an `Admits` venue spells a symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Spelling {
    /// Upper case with the `.P` lane suffix kept (binance, aster): `ETHUSDT` spot, `ETHUSDT.P` perp.
    LaneSuffixed,
    /// Upper case, no suffix (bybit's linear-only exec plane): `ETHUSDT`.
    BareLinear,
    /// The venue's own SWAP instId, upper case, nothing translated (okx): `ETH-USDT-SWAP`. See
    /// [`canonical_swap_instid`].
    Hyphenated,
    /// The name as the venue lists it, case kept, no lane suffix (hyperliquid's core perps): `BTC`,
    /// `kPEPE`. `kPEPE` and `KPEPE` are different spellings, so nothing is upper-cased.
    CaseKept,
    /// The venue's own PERPETUAL name, upper case, nothing translated (deribit): `ETH-PERPETUAL` (the
    /// coin-margined book) or `ETH_USDC-PERPETUAL` (the linear one). See `canonical_perpetual_name`.
    PerpetualName,
    /// The venue's own asset symbol, upper case, nothing translated (alpaca): an equity ticker `AAPL` /
    /// `BRK.B`, or a crypto pair WITH its slash `BTC/USD`. See `canonical_alpaca_symbol`.
    AssetSymbol,
    /// The broker's own symbol name, CASE KEPT, nothing translated (ctrader): `EURUSD`, and the names a
    /// CFD list carries (`Wheat_H6`, `USCrude100`). A cTrader name is a case-sensitive string the account's
    /// symbol list defines, so nothing is upper-cased and the list refuses a wrong case by name. See
    /// `canonical_broker_name`.
    BrokerName,
    /// The engine's own spelling of an OANDA instrument, upper case: a six-letter FX pair WITHOUT its
    /// underscore (`GBPUSD`, as the mounts spell `EURUSD`) or a CFD name WITH it (`SPX500_USD`); `GBP_USD`
    /// is read as `GBPUSD`. The adapter and the gatewayhub's facts turn it into OANDA's `GBP_USD`
    /// (`vike_catalog::to_oanda_instrument`). See `canonical_oanda_instrument`.
    OandaInstrument,
    /// IG's own EPIC, upper case, nothing translated: `CS.D.EURUSD.MINI.IP`, dot-separated segments. See
    /// `canonical_epic`. Upper case is the engine's AND the reconcile client's spelling
    /// (`AdmittedSymbols` holds epics upper-case), so an epic IG itself spells otherwise is refused by the
    /// facts (`vike_ig::market_facts` compares the venue's spelling exactly), never admitted twice.
    Epic,
    /// An IBKR US stock by NAME (ibkr): `TICKER.SMART.CCY`, upper case; a bare ticker is read as the USD stock
    /// (`AAPL` is `AAPL.SMART.USD`). Forex, futures, options and an explicit exchange are refused by name
    /// ([`canonical_ibkr_stock`]).
    StockName,
    /// A currency or metal PAIR, upper case (dukascopy, fxcm): six letters, written `EURUSD` or `EUR/USD` (the
    /// slash is dropped: both venues' fills spell the pair without it). A name of any other shape that is still
    /// made of the characters an instrument name has (`COPPER.CMD/USD`) is passed through unchanged for the
    /// gatewayhub's table to refuse by name. See [`canonical_pair_name`].
    PairName,
}

/// What a venue's adapter does with the symbol an order names. See the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SymbolRule {
    /// Signs on the order's symbol: the door admits it.
    Admits(Spelling),
    /// Signs on the engine's mounted symbol: the door refuses any other, for this reason. ⚠ No roster
    /// venue has this row today (deribit's was the last): the variant is kept because
    /// `just new-venue`'s classification hint offers it to the next adapter.
    #[allow(dead_code, clippy::allow_attributes)]
    MountedOnly(&'static str),
    /// Reads the order's own symbol already; not this gate's.
    PerOrder,
    /// Reads the order's own symbol, but the ENGINE cannot judge a symbol it was not started on (its
    /// grid, multiplier or price are the mounted symbol's): the door refuses a foreign symbol, for this
    /// reason, until admission supports the venue. Deleting the row (to `PerOrder` or `Admits`) opens it.
    /// ⚠ No roster venue has this row today (fxcm's was the last, until its price lane): the variant is kept
    /// because `just new-venue`'s classification hint offers it. ⚠ NO TEST HOLDS WHAT IT DOES any more (the test
    /// that did went with the last row, and a rule cannot be injected into `SYMBOL_RULES` from outside): whoever
    /// gives a venue this row again writes one first (`per_order_refusal`'s `PerOrderUnsafe` arm).
    #[allow(dead_code, clippy::allow_attributes)]
    PerOrderUnsafe(&'static str),
}

/// The rule of every roster venue. `every_roster_venue_has_a_symbol_rule` fails on a venue in none,
/// so a new venue is classified by whoever reads its order builder.
// vike:new-venue:note do NOT add a row here by hand-copying one: read the new venue's ORDER BUILDER (does it sign on the order's symbol or on the one its client was bound to?), then add `("{venue}", SymbolRule::PerOrder)` if it reads the order's symbol AND the engine judges a foreign symbol soundly (grid, multiplier, a price source), `SymbolRule::PerOrderUnsafe("<why>")` if it reads the symbol but the engine cannot, or `SymbolRule::MountedOnly("<why>")` if it does not. `Admits` is only for a venue whose adapter has a per-symbol router AND whose `TradeLane::symbol_facts` answers (the gatewayhub), and it also needs a `Spelling`.
pub(super) const SYMBOL_RULES: &[(&str, SymbolRule)] = &[
    ("binance", SymbolRule::Admits(Spelling::LaneSuffixed)),
    ("aster", SymbolRule::Admits(Spelling::LaneSuffixed)),
    ("bybit", SymbolRule::Admits(Spelling::BareLinear)),
    ("okx", SymbolRule::Admits(Spelling::Hyphenated)),
    ("deribit", SymbolRule::Admits(Spelling::PerpetualName)),
    ("hyperliquid", SymbolRule::Admits(Spelling::CaseKept)),
    ("alpaca", SymbolRule::Admits(Spelling::AssetSymbol)),
    ("ctrader", SymbolRule::Admits(Spelling::BrokerName)),
    ("ig", SymbolRule::Admits(Spelling::Epic)),
    ("oanda", SymbolRule::Admits(Spelling::OandaInstrument)),
    ("fxcm", SymbolRule::Admits(Spelling::PairName)),
    ("ibkr", SymbolRule::Admits(Spelling::StockName)),
    ("polymarket", SymbolRule::PerOrder),
    ("dukascopy", SymbolRule::Admits(Spelling::PairName)),
];

/// The venues the door admits symbols for (the `Admits` rows): the root hands them to the mount as
/// `AdapterPolicy::fail_closed_symbol_venues`, so exactly these engines deny a symbol they were never
/// told about.
pub fn admitting_venues() -> Vec<String> {
    SYMBOL_RULES
        .iter()
        .filter(|(_, rule)| matches!(rule, SymbolRule::Admits(_)))
        .map(|(venue, _)| (*venue).to_string())
        .collect()
}

/// **The venues whose gatewayhub states a conversion rate for a symbol quoted in another currency than the
/// account's** (decision 0183, phase 1): oanda (`homeConversions`), ctrader (the mid of the conversion pair's
/// own quote), ibkr (the mid of a live forex quote, for a STOCK; forex pairs are cash positions and stay
/// refused), dukascopy (the mid of the conversion pair on the JForex sidecar's own ticks, in the account's
/// currency the sidecar's `ready` line states), ig (the market details' `currencies[]`, checked against a
/// price) and fxcm (the mid of the conversion pair in the account's own running session, the account's
/// currency from its `BASE_CRNCY` property or, when the API cannot say, the owner's assumed and flagged USD).
/// Every other venue is fenced to instruments quoted in its account's own currency, by its own facts. The
/// root hands them to the engine's assembly as `AdapterPolicy::quote_converting_venues`, and
/// [`quote_conversion_refusal`] is the door's rule. fxcm's door is `Admits` since its price lane
/// (`feeds/fxcm.rs`), so a foreign-quoted pair is admitted at its rate on a capped `demo` account and nowhere else.
const QUOTE_CONVERTING_VENUES: &[&str] = &["oanda", "ctrader", "ibkr", "dukascopy", "ig", "fxcm"];

/// [`QUOTE_CONVERTING_VENUES`] as the root hands them to the engine's assembly.
pub fn quote_converting_venues() -> Vec<String> {
    QUOTE_CONVERTING_VENUES.iter().map(|v| (*v).to_string()).collect()
}

/// **The venues whose gatewayhub lane rates the engine's OWN MOUNTED symbol too** (decision 0183, the
/// mounted-symbol amendment): a subset of [`QUOTE_CONVERTING_VENUES`]. On such a venue a ledger that is not in the
/// mount's money (a EUR account mounted on `EURUSD`) is anchored to the account's currency and the mount is
/// DECLARED foreign-quoted in its own money, rated by the node (`LeaseFacts::rate_foreign_mounts`: the start's
/// first admission, a refresh about every [`MOUNT_RATE_EVERY`] and the door's re-rate of the own symbol); on every
/// other venue the mount fence stands as it was (`vike_tradehub_core::unanchorable_mount`).
///
/// oanda (`homeConversions`), ctrader (the mid of the conversion pair) and fxcm (the mid of the conversion pair in
/// the running session's Offers table) state the rate of any listed symbol, the mounted one included. NOT in it:
/// dukascopy (its lane's `Ledger::Elsewhere` refuses every symbol of a ledger the mount is not in, in words, and
/// would leave the mount unrated for ever: it needs its own lane edit and demo proof), ig (`ig_account_refusal`:
/// a non-USD ig account does not run once the ceiling is armed) and ibkr (foreign cash is a per-asset ledger, a
/// decision 0183 reopener). Adding a venue is one row here and one demo round trip. The root hands the list to the
/// engine's assembly as `AdapterPolicy::mount_converting_venues`, so the door and the assembly decide from ONE
/// table.
const MOUNT_CONVERTING_VENUES: &[&str] = &["oanda", "ctrader", "fxcm"];

/// [`MOUNT_CONVERTING_VENUES`] as the root hands them to the engine's assembly.
pub fn mount_converting_venues() -> Vec<String> {
    MOUNT_CONVERTING_VENUES.iter().map(|v| (*v).to_string()).collect()
}

/// **The money the engine mounted on `mounted` is quoted in, when `venue` books that mount through a rate for a
/// ledger in `ccy`, or `None`** (the door's side of `vike_tradehub_core::foreign_mount_money`, which the engine's
/// assembly asks with [`MOUNT_CONVERTING_VENUES`] too): the venue must be in the list, and the strict fence
/// ([`unanchorable_mount`]) must decline the ledger only because the mount's NAME states another money.
pub(crate) fn foreign_mount_money(venue: &str, mounted: &str, ccy: &str) -> Option<String> {
    MOUNT_CONVERTING_VENUES
        .contains(&venue)
        .then(|| vike_tradehub_core::foreign_mount_money(venue, mounted, ccy))
        .flatten()
}

/// **Why this account may NOT hold a symbol quoted in a currency other than its own, or `None`** (decision
/// 0183, phase 1): the venue must have a rate source ([`QUOTE_CONVERTING_VENUES`]) and the account must be on
/// its practice tier with an account-wide exposure ceiling armed
/// (`vike_tradehub_core::quote_conversion_tier_refusal`, the half the engine's assembly reads too). The tier
/// gate IS the switch: there is no setting and no environment variable beside it.
pub(crate) fn quote_conversion_refusal(
    venue: &str,
    tier: Option<AccountTier>,
    account_ceiling_armed: bool,
) -> Option<String> {
    if !QUOTE_CONVERTING_VENUES.contains(&venue) {
        return Some(format!(
            "this node converts a quote currency for the practice accounts of {} only, and {venue} \
             has no rate source: only instruments quoted in the account's own currency are admitted",
            QUOTE_CONVERTING_VENUES.join(", ")
        ));
    }
    vike_tradehub_core::quote_conversion_tier_refusal(tier, account_ceiling_armed)
}

/// **Whether a lease of `venue` at `tier` asks the gatewayhub to convert** (`TradeSession::convert_quotes`):
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
/// budget.
pub(crate) fn quote_lease_converts(
    venue: &str,
    tier: AccountTier,
    anchored: bool,
    ceiling: bool,
) -> bool {
    quote_conversion_refusal(venue, Some(tier), true).is_none()
        && (!reads_currency_on_ask(venue) || (anchored && ceiling))
}

/// Whether `venue`'s statement of the account's currency is paid for out of a venue budget, so the node asks for it
/// only for an account that will convert (an exposure ceiling armed): ibkr (the data lane's four dials an hour)
/// and ig (a login and `GET /accounts` out of the account's non-trading allowance, which a rejected login may
/// also count against). oanda's account summary is free and always read.
fn reads_currency_on_ask(venue: &str) -> bool {
    matches!(venue, "ibkr" | "ig")
}

/// **Whether the start of a lease of `venue` at `tier` asks the gatewayhub to READ the account's currency**
/// (`AdapterStart::read_account_ccy`): only for a venue whose statement of the currency costs the venue a budget
/// (ibkr: the data lane's four dials an hour; ig: a login and a read of the account allowance), and only for an
/// account the node will convert for (the rule's venue and tier, with an exposure ceiling armed). An ibkr or ig
/// account without a ceiling reads nothing at its start, as before the conversion existed.
pub(crate) fn quote_lease_reads_currency(venue: &str, tier: AccountTier, ceiling: bool) -> bool {
    reads_currency_on_ask(venue) && account_converts(venue, tier, ceiling)
}

/// **Whether the account converts a quote currency TODAY**: the venue and tier the rule names, with an exposure
/// ceiling armed (the opt-in). The one question behind both the budgeted read of the currency
/// ([`quote_lease_reads_currency`]) and the journal's fallback anchor ([`anchor_with_journal`]).
pub(crate) fn account_converts(venue: &str, tier: AccountTier, ceiling: bool) -> bool {
    ceiling && quote_conversion_refusal(venue, Some(tier), true).is_none()
}

/// **Whether a RATE of `venue` MUST say which ledger currency it is expressed in** (`WireSymbolFacts::account_ccy`):
/// ibkr's gatewayhub reads the base currency apart from the start and stamps every rate with the one it found, so a
/// rate without that stamp is refused (fail closed: the engine may be anchored to a journaled currency the rate was
/// never checked against). oanda's, ctrader's and fxcm's gatewayhub lanes stamp theirs too (the currency of the
/// listing or session the rate was priced against), and the door refuses any stamp that is not the engine's anchor
/// (`LeaseFacts`), but an UNSTAMPED answer of theirs (an older gatewayhub) is still admitted, as before, and an
/// anchor taken from the journal stays unconfirmed for them until a stamp confirms it.
pub(crate) fn rate_must_state_its_ledger(venue: &str) -> bool {
    venue == "ibkr"
}

/// **Whether the reconcile BALANCE of `venue` is the account's cash with no currency beside it**: oanda's account
/// summary, ig's account row and fxcm's Accounts table state none, so the core adopts the figure into a ledger
/// labelled with whatever the engine was bound with, and the node holds it back when that label is not what the
/// venue stated (`remote_backend::balance_gate`). ctrader's `parse_balance` answers only a USD deposit it NAMES,
/// dukascopy has no reconcile balance and ibkr's client withholds itself, so those pass through.
pub(crate) fn balance_carries_no_currency(venue: &str) -> bool {
    matches!(venue, "oanda" | "ig" | "fxcm")
}

/// **The currency a lease is anchored to when the gatewayhub could not state one at its start**, or the one it
/// stated. `stated` is the gatewayhub's word; `journaled` is the currency the previous run's ledger was anchored
/// to for this account (the fx table of the engine snapshot the journal restores: present exactly when the engine
/// held a symbol quoted in another currency); `converts` is [`account_converts`]. A stated currency always wins.
///
/// THE RULE for the journal's (every converting venue: ibkr, ctrader, dukascopy, ig, oanda, fxcm). It is used only
/// when (1) the account converts today, so a re-pointed account that no longer does is never anchored from the
/// past; (2) the venue stated none; (3) the venue's own rule on the account's currency allows it, exactly as it
/// judges a stated one (ig books every order in USD, so a journaled other money is not an ig anchor: `ig_account_refusal`);
/// and the assembly's mount fence then judges it like a stated one too (`unanchorable_mount` of the engine and of the door).
/// WHY it is used at all: the restored engine's rates are expressed in it, and the core restores a default
/// account's journaled engine only when its ledger label equals the journaled one (a snapshot with no route key is
/// matched on `quote_asset`), so an engine assembled unanchored after one missed read at a restart would be skipped,
/// flat while the venue holds the position.
/// WHAT bounds a stale one (the owner re-points the account to another currency): a venue whose rates state the
/// ledger they are expressed in (ibkr's, `WireSymbolFacts::account_ccy`) has every rate checked against the anchor
/// by the door; for a venue whose rates do not, the anchor is UNCONFIRMED (`LeaseFacts::anchor_unconfirmed`) and the
/// door admits no symbol quoted in another currency on it, until the account is started again and states its own.
pub(crate) fn anchor_with_journal(
    venue: &str,
    converts: bool,
    stated: Option<String>,
    journaled: Option<&str>,
) -> Option<String> {
    if stated.is_some() || !converts {
        return stated;
    }
    let ccy = journaled.map(str::trim).filter(|ccy| !ccy.is_empty())?.to_ascii_uppercase();
    ig_account_refusal(venue, Some(&ccy)).is_none().then_some(ccy)
}

/// The currency the owner's ruling of 2026-10-19 assumes for an account whose venue states none: USD.
pub(crate) const ASSUMED_ACCOUNT_CCY: &str = "USD";

/// **The currency a lease is anchored to, and whether it is only ASSUMED**: the VENUE's word, else the JOURNAL's
/// ([`anchor_with_journal`], under its fences), else [`ASSUMED_ACCOUNT_CCY`] (owner ruling 2026-10-19, repeated:
/// "the account currency is what the venue API states; if the API does not state it, or the read fails: USD. Never
/// refuse because a currency is unknown"). `stated_assumed` is the lane's own flag on `stated` (the gatewayhub
/// fills in USD itself when a venue is silent, fxcm's and now every converting venue's, and says so): that is the
/// venue's SILENCE, so it ranks below the journal and travels flagged when nothing outranks it. The default is
/// applied only to an account that converts today (`converts`, [`account_converts`]): a `live` or `paper` account,
/// an account with no exposure ceiling and a venue with no rate source are left as they were, so nothing opens
/// there. A blank statement is no statement. The assembly's mount fence then judges a defaulted currency like a
/// stated one (`unanchorable_mount`). ⚠ A journal that NAMES a currency the venue's own rule refuses (ig's money
/// other than USD) is evidence the account is not in USD, so it is not a "no journal": nothing is defaulted
/// there, and the engine keeps its label as before. A journal that names the DEFAULT while the venue says nothing
/// only records the previous run's assumption, so it is returned ASSUMED too (it trades like the first run).
pub(crate) fn anchor_with_journal_or_usd(
    venue: &str,
    converts: bool,
    stated: Option<String>,
    stated_assumed: bool,
    journaled: Option<&str>,
) -> Option<(String, bool)> {
    let stated = stated.filter(|ccy| !ccy.trim().is_empty());
    if !converts {
        return stated.map(|ccy| (ccy, stated_assumed));
    }
    // The venue's OWN word. A lane's assumption (`stated_assumed`) is the venue's SILENCE that the gatewayhub filled
    // in: it ranks below the journal (venue > journal > USD assumed, whoever assumed it).
    let venue_word = stated.clone().filter(|_| !stated_assumed);
    let venue_silent = venue_word.is_none();
    let journal_names_one = journaled.is_some_and(|ccy| !ccy.trim().is_empty());
    match anchor_with_journal(venue, true, venue_word, journaled) {
        // A journal that names the DEFAULT while the venue says nothing only records the previous run's
        // assumption (the fx table carries the ledger currency exactly when that run held a foreign-quoted
        // symbol): it is the same assumed USD with the same rights and the same warning, not an "unconfirmed"
        // journal anchor that would lock the default out of every run after the first.
        Some(ccy) => {
            let recorded_assumption = venue_silent && ccy.eq_ignore_ascii_case(ASSUMED_ACCOUNT_CCY);
            Some((ccy, recorded_assumption))
        }
        None if !journal_names_one => {
            Some((stated.unwrap_or_else(|| ASSUMED_ACCOUNT_CCY.to_string()), true))
        }
        None => None,
    }
}

/// **Whether the anchor of a lease is UNCONFIRMED**: nothing the venue said stands behind it, and no rate of the
/// venue states its ledger to check it against, so the door admits no symbol quoted in another currency on it. That
/// is the JOURNAL's anchor (the previous run's ledger, used because the read missed at this start: the owner may
/// have re-pointed the account since) for a venue whose rates do not state their ledger
/// ([`rate_must_state_its_ledger`]). The venue's own word is confirmed; so is an ASSUMED currency (the owner's
/// ruling of 2026-10-19, repeated: the USD default must TRADE, inside the conversion's own gates, as FXCM's
/// assumed USD does), whose assumption is flagged and warned instead. `venue_stated`: the venue itself named the
/// currency (a lane's own assumption does not count); `assumed`: the bind flags it assumed.
pub(crate) fn anchor_is_unconfirmed(
    venue: &str,
    anchored: bool,
    assumed: bool,
    venue_stated: bool,
) -> bool {
    anchored && !venue_stated && !assumed && !rate_must_state_its_ledger(venue)
}

/// The currencies IBKR may quote in their MINOR unit (London in pence, Tel Aviv in agorot, Johannesburg in
/// cents), by the names a contract carries them under (`GBP` for a pence-priced LSE stock, `GBX`/`GBp`, `ILA`,
/// `ZAC`/`ZAc`), upper-cased. The gatewayhub refuses a RATE for the three major units (`vike_ibkr::fx_rate`'s
/// `minor_unit_refusal`; this node links no bridge, so the list is restated); here they refuse an ANCHOR.
const MINOR_UNIT_MONEY: &[&str] = &["GBP", "GBX", "ILS", "ILA", "ZAR", "ZAC"];

/// **Why the engine mounted on `mounted` cannot have its ledger anchored to `ccy`, the currency the venue
/// states for the account, or `None`.** Only ibkr has a rule: its engine counts money in the currency of its
/// MOUNTED contract (the cash row its balance seed reads, `ibkr_engine_money`), so a base currency that is
/// another one would label that cash, and value the mounted stock, in a currency it is not in; and a mounted
/// forex pair is a CASH position, whose profit this margin engine does not book (`FOREX_REFUSAL` of the bridge);
/// and a mount whose money is a currency IBKR may quote in its minor unit ([`MINOR_UNIT_MONEY`]) is not anchored
/// either. Every other venue is judged by the ENGINE's own fence (`vike_tradehub_core::unanchorable_mount`: a mount
/// quoted in another money than `ccy`, or whose name says none, is not a `ccy` ledger; a venue with no rate
/// source is not judged): ONE decision, so the door never anchors a lease, nor asks a budgeted venue for a rate,
/// on a currency the engine's assembly then declines to be told.
pub(crate) fn unanchorable_mount(venue: &str, mounted: &str, ccy: &str) -> Option<String> {
    if venue != "ibkr" {
        return vike_tradehub_core::unanchorable_mount(venue, mounted, ccy);
    }
    if ibkr_forex_name(mounted) {
        return Some(format!(
            "the engine is mounted on the forex pair `{mounted}`, a CASH position whose profit this \
             margin engine does not book, so its ledger is not anchored to the account's currency"
        ));
    }
    match ibkr_engine_money(mounted) {
        // Whether IBKR prices this mount in pounds or in pence is not measured: anchoring the ledger to the
        // currency would book a pence price as pounds, 100 times too high.
        Some(money) if MINOR_UNIT_MONEY.contains(&money.as_str()) => Some(format!(
            "the engine is mounted on `{mounted}`, which counts its money in {money}, a currency IBKR may \
             quote in its MINOR unit (pence, agorot, cents), and which listings it does so for is not \
             measured: its prices could be booked 100 times too high, so the ledger is not anchored to {ccy}"
        )),
        Some(money) if money.eq_ignore_ascii_case(ccy) => None,
        Some(money) => Some(format!(
            "the engine is mounted on `{mounted}`, which counts its money in {money}, and the account's \
             base currency is {ccy}: the balance the engine reads is the {money} cash, so the ledger is \
             not anchored to {ccy}. Mount a {ccy} contract to trade other currencies"
        )),
        None => Some(format!(
            "the engine is mounted on `{mounted}`, which states no currency, so its ledger is not \
             anchored to the account's currency ({ccy})"
        )),
    }
}

/// **The pair a venue's engine is mounted on INSTEAD of `symbol`, so that its ledger can be anchored to `ccy`**
/// (the currency the VENUE states for the account), or `None` to keep `symbol`. This crate is the registry that names
/// venues (`vike-tradehub-core` names none), so the table lives here beside [`QUOTE_CONVERTING_VENUES`].
///
/// The mount fence ([`unanchorable_mount`]) declines a ledger the mounted symbol's money is not, which left FXCM's
/// demo account (GBP, measured 2026-10-20) with an unanchored ledger on the wired `EURUSD`: its balance held back and
/// every foreign-quoted pair refused. An engine is mounted where its venue session was STARTED, and the currency is
/// read by that start, so `RemoteBackend::start_adapter` starts the session again on the pair this returns and reports
/// it in `AdapterBind::mount_symbol` (`moved_mount` is its one caller).
///
/// The rule is deliberately narrow, so that every start it does not name is byte-identical to the one before it:
/// `symbol` must be the venue's WIRED mount (a mount somebody chose is their word, and a labelled account's mount is
/// its strategy's), the wired mount must NOT already carry the ledger (a USD account stays on `EURUSD`), and
/// [`CURRENCY_MOUNTS`] must hold a row for `ccy` whose pair DOES. An unstated currency never reaches it (the node
/// assumes USD, which the wired mount carries), and any money without a row keeps today's behaviour: unanchored
/// ledger, balance held, foreign-quoted pairs refused.
pub(crate) fn currency_mount(venue: &str, symbol: &str, ccy: &str) -> Option<&'static str> {
    if vike_venues::venue_wiring::wired_symbol(venue) != Some(symbol) {
        return None;
    }
    unanchorable_mount(venue, symbol, ccy)?;
    CURRENCY_MOUNTS
        .iter()
        .find(|(v, c, _)| *v == venue && c.eq_ignore_ascii_case(ccy))
        .map(|(_, _, pair)| *pair)
        .filter(|pair| unanchorable_mount(venue, pair, ccy).is_none())
}

/// `(venue, currency, pair)`: the pair a venue's engine is mounted on for an account whose money is the currency.
/// fxcm's GBP: `EURGBP` is a regular pair quoted in pounds, so its ledger books the mount at par in the account's own
/// money, and the dollar pairs convert at GBPUSD. One row, because one currency was MEASURED; another is a row (and a
/// test row) once an account in it exists to measure.
pub(crate) const CURRENCY_MOUNTS: &[(&str, &str, &str)] = &[("fxcm", "GBP", "EURGBP")];

/// The currency an IG ledger is read in: `vike_ig::WIRE_CURRENCY`, the currency of the mounted epic (EUR/USD) and the
/// one an order is written in whenever its epic lists it (this node links no bridge, so it is restated).
const IG_WIRE_CURRENCY: &str = "USD";

/// **Why an ig account whose venue-stated currency is `stated` must NOT RUN, or `None`** (decision 0183, review
/// finding): the mounted epic is quoted in [`IG_WIRE_CURRENCY`], the balance seed is the account's cash in
/// ITS currency, and the engine labels its ledger one currency. For a pound account the seed (10,000 GBP) is
/// read as dollars and a USD-quoted mount's profit is added to it unconverted (~35% off), and the ceiling and
/// the margin run on those numbers; anchoring the ledger to "GBP" would book the mount's dollars under a pound
/// label, and no rate for the mount exists (facts are asked of epics the engine is NOT mounted on). So an ig
/// account whose currency the venue stated and is not the wire's does not run: only a USD account does.
/// `stated` is `Some` only when this start asked for the currency (`quote_lease_reads_currency`: an exposure
/// ceiling armed), so an ig account with no ceiling is not judged here and runs exactly as it always did. An
/// unread currency (`None`) is not a verdict either.
pub(crate) fn ig_account_refusal(venue: &str, stated: Option<&str>) -> Option<String> {
    let ccy = stated.map(str::trim).filter(|c| !c.is_empty())?;
    (venue == "ig" && !ccy.eq_ignore_ascii_case(IG_WIRE_CURRENCY)).then(|| {
        format!(
            "this ig account's currency is {ccy} and the mounted epic is quoted in {IG_WIRE_CURRENCY}: \
             the ledger would hold {ccy} cash and {IG_WIRE_CURRENCY} profit as one number, and the \
             exposure ceiling and the margin would run on it. Only a {IG_WIRE_CURRENCY} ig account is \
             supported"
        )
    })
}

fn rule_of(venue: &str) -> Option<SymbolRule> {
    SYMBOL_RULES.iter().find(|(v, _)| *v == venue).map(|(_, rule)| *rule)
}

/// The door's verdict on a plain Submit's symbol.
#[derive(Debug, PartialEq)]
pub(crate) enum SymbolPlan {
    /// Not this gate's, or already placeable exactly as written: send the order untouched.
    Pass,
    /// Refused, with the words the caller gets.
    Refuse(String),
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

/// The engine's trade list as the refusal prints it.
fn trades_text(engine: &VenueBlock) -> String {
    std::iter::once(&engine.symbol)
        .chain(engine.extra_symbols.iter())
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The canonical spelling of `raw` for `spelling`, or why it is not a symbol.
pub(super) fn canonical_symbol(spelling: Spelling, raw: &str) -> Result<String, String> {
    if spelling == Spelling::Hyphenated {
        return canonical_swap_instid(raw);
    }
    if spelling == Spelling::PerpetualName {
        return canonical_perpetual_name(raw);
    }
    if spelling == Spelling::AssetSymbol {
        return canonical_asset_symbol(raw);
    }
    if spelling == Spelling::BrokerName {
        return canonical_broker_name(raw);
    }
    if spelling == Spelling::OandaInstrument {
        return canonical_oanda_instrument(raw);
    }
    if spelling == Spelling::Epic {
        return canonical_epic(raw);
    }
    if spelling == Spelling::StockName {
        return canonical_ibkr_stock(raw);
    }
    if spelling == Spelling::PairName {
        return canonical_pair_name(raw);
    }
    let upper = raw.trim().to_ascii_uppercase();
    // A case-kept venue has no lane suffix either: `BTC.P` stays a refused name, never `BTC`.
    let (base, perp) = if spelling == Spelling::CaseKept {
        (raw.trim(), false)
    } else {
        vike_catalog::split_perp(&upper)
    };
    if base.is_empty() || base.len() > 32 || !base.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(format!(
            "`{raw}` is not an exchange symbol (letters and digits, e.g. `ETHUSDT`)"
        ));
    }
    Ok(match spelling {
        Spelling::LaneSuffixed if perp => format!("{base}{}", vike_catalog::PERP_SUFFIX),
        Spelling::LaneSuffixed
        | Spelling::BareLinear
        | Spelling::Hyphenated
        | Spelling::PerpetualName
        | Spelling::AssetSymbol
        | Spelling::BrokerName
        | Spelling::OandaInstrument
        | Spelling::Epic
        | Spelling::StockName
        | Spelling::PairName
        | Spelling::CaseKept => base.to_string(),
    })
}

/// **ctrader's symbol is the broker's own name**, case kept and nothing translated: the name the account's
/// symbol list gives (`EURUSD`; a CFD list also carries `Wheat_H6`, `USCrude100`), looked up by that exact
/// string. Letters, digits and `_`, at most 32, nothing else: the name reaches a log line and a protobuf
/// field downstream. A wrong case (`eurusd`) is another, unlisted name and the list refuses it by name.
fn canonical_broker_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty()
        || name.len() > 32
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(format!(
            "`{raw}` is not a ctrader symbol (the broker's name as the account's symbol list spells it, \
             case kept, letters, digits and `_`, e.g. `EURUSD`)"
        ));
    }
    Ok(name.to_string())
}

/// **IG's symbol is its EPIC**, upper-cased, nothing translated: three or more non-empty dot-separated
/// segments of letters, digits, `_` and `-` (`CS.D.EURUSD.MINI.IP`). A market NAME (`EURUSD`, `EUR/USD`),
/// which the adapter itself refuses (`non_epic_refusal`: IG answers it with an HTTP 500), is refused here
/// by name before any lookup. The symbol reaches a URL path downstream, so nothing else passes.
fn canonical_epic(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let segment = |s: &str| {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    };
    if upper.len() > 64 || upper.split('.').count() < 3 || !upper.split('.').all(segment) {
        return Err(format!(
            "`{raw}` is not an IG epic (a dot-separated market id such as `CS.D.EURUSD.MINI.IP`, \
             not a pair name like `EURUSD`)"
        ));
    }
    Ok(upper)
}

/// Whether two ig spellings name ONE epic: they differ in case only. The door spells an epic upper case
/// (the reconcile client holds it so), so an engine mounted on `cs.d.eurusd.mini.ip` must not be admitted
/// `CS.D.EURUSD.MINI.IP` as a second symbol, or one venue position would be booked twice.
fn same_ig_epic(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// **A Dukascopy or FXCM instrument is a six-letter currency or metal pair**, upper-cased: `EURUSD`, written with
/// or without its slash (`EUR/USD` becomes `EURUSD`, the form both venues' fills and position lines carry). What
/// else the venue names (`AAPL.US/USD`, `COPPER.CMD/USD`) is passed through UNCHANGED, upper-cased, when it is
/// made of letters, digits, `.` and `/` only, so the gatewayhub's table refuses it BY NAME (a CFD placeholder,
/// a CFD whose fill spelling differs, an instrument the table does not carry); the door itself knows no table.
/// Anything with another character is refused here, before any lookup: the symbol reaches the sidecar's JSON
/// line (dukascopy) or the Offers lookup (fxcm) downstream.
fn canonical_pair_name(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let bare = bare_pair(&upper);
    if is_pair(&bare) {
        return Ok(bare);
    }
    let name_made = !upper.is_empty()
        && upper.len() <= 32
        && upper.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'/');
    if name_made {
        Ok(upper)
    } else {
        Err(format!(
            "`{raw}` is not an instrument name (a currency or metal pair is six letters, \
             `EURUSD` or `EUR/USD`)"
        ))
    }
}

/// `symbol` upper-cased with a `AAA/BBB` slash dropped: the one spelling of a pair the engine and the fills share.
fn bare_pair(symbol: &str) -> String {
    let upper = symbol.trim().to_ascii_uppercase();
    match upper.split_once('/') {
        Some((a, b)) if a.len() == 3 && b.len() == 3 => format!("{a}{b}"),
        _ => upper,
    }
}

/// Whether `bare` is six ASCII letters, the shape of a currency or metal pair.
fn is_pair(bare: &str) -> bool {
    bare.len() == 6 && bare.bytes().all(|b| b.is_ascii_alphabetic())
}

/// The currency a pair engine (dukascopy, fxcm) counts its money in: the QUOTE of its own pair (`EURUSD` is USD),
/// the last three letters, or `None` when the engine's symbol is not a pair.
fn pair_money(symbol: &str) -> Option<String> {
    let bare = bare_pair(symbol);
    is_pair(&bare).then(|| bare[3..].to_string())
}

/// **oanda's symbol, in the engine's spelling**: upper case, a six-letter FX pair without an underscore
/// (`GBP_USD` and `gbpusd` are `GBPUSD`: the mounts spell a pair that way, and one instrument must be ONE
/// symbol in an engine), any other name as OANDA writes it, with its single underscore (`SPX500_USD`).
/// Letters and digits and at most that one underscore, nothing else: the symbol reaches a URL path and a
/// JSON body downstream. Nothing is guessed: a bare `BTCUSD`-shaped name is read as the FX pair it looks
/// like and the account's own list refuses it by name.
fn canonical_oanda_instrument(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let hint = "an OANDA instrument is a six-letter FX pair such as `GBPUSD` (or `GBP_USD`), or a CFD \
                name with its underscore such as `SPX500_USD`";
    let word =
        |p: &str| !p.is_empty() && p.len() <= 12 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    let ok = match upper.split_once('_') {
        None => upper.len() == 6 && upper.bytes().all(|b| b.is_ascii_alphabetic()),
        Some((base, quote)) => word(base) && word(quote),
    };
    if !ok {
        return Err(format!("`{raw}` is not an OANDA instrument ({hint})"));
    }
    Ok(match upper.split_once('_') {
        Some((base, quote))
            if base.len() == 3
                && quote.len() == 3
                && base.bytes().chain(quote.bytes()).all(|b| b.is_ascii_alphabetic()) =>
        {
            format!("{base}{quote}")
        }
        _ => upper,
    })
}

/// Whether two oanda spellings name ONE instrument (`GBPUSD` and `GBP_USD` do): an engine mounted on one
/// must not be admitted the other, or one venue position would be booked under two symbols.
fn same_oanda_instrument(a: &str, b: &str) -> bool {
    vike_catalog::to_oanda_instrument(a) == vike_catalog::to_oanda_instrument(b)
}

/// **alpaca's symbol is the venue's own asset symbol**, upper-cased, and nothing is translated: an equity
/// ticker (`AAPL`, with a share-class dot, `BRK.B`) or a crypto pair WITH its slash (`BTC/USD`). A bare
/// `BTCUSD` is NOT turned into `BTC/USD` here (the adapter does that on the wire, but guessing a pair
/// from letters at the door could sign a different instrument than the operator meant): it is read as
/// the ticker it looks like and the venue's list refuses it by name. Letters and digits, one inner `.` or
/// one inner `/`, nothing else: the symbol reaches a query string and a JSON body downstream.
fn canonical_asset_symbol(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let word =
        |p: &str| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    let hint = "an alpaca symbol is an equity ticker such as `AAPL` (or `BRK.B`), or a crypto pair \
                written with its slash such as `BTC/USD`";
    let ok = match upper.split_once(['.', '/']) {
        None => word(&upper),
        Some((a, b)) => word(a) && word(b),
    };
    if ok { Ok(upper) } else { Err(format!("`{raw}` is not an alpaca symbol ({hint})")) }
}

/// **An IBKR stock is admitted by NAME**: `TICKER.SMART.CCY`, upper-cased, where the ticker is letters and
/// digits (a dot cannot be written in the engine grammar, `crates/bridges/vike-ibkr/src/contract.rs`) and the
/// currency is three letters; a bare ticker is read as the USD stock. What else a trader may type is refused
/// here BY NAME, before any lookup: forex (`EUR.IDEALPRO.USD`), a future or an option (a fourth field), a stock
/// on a named exchange (`AAPL.NASDAQ.USD`: it is another way to name the stock the engine may already hold).
/// The gatewayhub's lookup checks the same form again and resolves the name to exactly one stock.
fn canonical_ibkr_stock(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let hint = "an IBKR stock is `TICKER.SMART.CCY`, e.g. `AAPL.SMART.USD` (or just the ticker `AAPL`, read \
                as the USD stock); forex, futures, options and an explicit exchange are not admitted by \
                name";
    let word =
        |p: &str| !p.is_empty() && p.len() <= 12 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    let parts: Vec<&str> = upper.split('.').collect();
    match parts.as_slice() {
        [ticker] if word(ticker) => Ok(format!("{ticker}.SMART.USD")),
        [ticker, "SMART", ccy]
            if word(ticker) && ccy.len() == 3 && ccy.bytes().all(|b| b.is_ascii_alphabetic()) =>
        {
            Ok(upper)
        }
        _ if ibkr_forex_name(&upper) => Err(format!(
            "`{raw}` is not an IBKR stock name ({hint}). An IBKR forex pair is a CASH position: buying it \
             changes the account's cash balances in two currencies and books no margin profit, which \
             this engine's margin-account model does not hold, so it is refused and not converted"
        )),
        _ => Err(format!("`{raw}` is not an IBKR stock name ({hint})")),
    }
}

/// The currency an IBKR engine counts its money in: the currency of its own (mounted) contract, which is the
/// cash row its balance seed reads (`crates/bridges/vike-ibkr/src/recon/socket.rs`'s `parse_socket_balance`).
/// `SYMBOL.EXCHANGE.CCY[...]`, except forex's legacy `EUR.USD.IDEALPRO` where the currency is the middle field.
fn ibkr_engine_money(engine_symbol: &str) -> Option<String> {
    let parts: Vec<&str> = engine_symbol.split('.').collect();
    let ccy = match parts.as_slice() {
        [_, ccy, route, ..] if route.eq_ignore_ascii_case("IDEALPRO") => ccy,
        [_, _, ccy, ..] => ccy,
        _ => return None,
    };
    Some(ccy.to_ascii_uppercase())
}

/// Whether an IBKR canonical names a forex pair: the forex exchange in the route or currency field
/// (`EUR.IDEALPRO.USD`, `EUR.USD.IDEALPRO`). A CASH position, not a stock (decision 0183).
fn ibkr_forex_name(canonical: &str) -> bool {
    canonical.split('.').skip(1).any(|p| p.eq_ignore_ascii_case("IDEALPRO"))
}

/// Whether the engine symbol `own` is the same STOCK as the admitted name `symbol` under another spelling
/// (`AAPL.NASDAQ.USD` beside `AAPL.SMART.USD`): two symbols in one engine would book one venue position twice.
/// A forex pair, a future or an option sharing the ticker is another instrument.
fn same_ibkr_stock(own: &str, symbol: &str) -> bool {
    let (a, b): (Vec<&str>, Vec<&str>) = (own.split('.').collect(), symbol.split('.').collect());
    let is_stock = a.len() == 3 && !a.iter().any(|p| p.eq_ignore_ascii_case("IDEALPRO"))
        || a.len() == 4 && a[3].eq_ignore_ascii_case("STK");
    is_stock && a.first().zip(b.first()).is_some_and(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// Whether two alpaca spellings name ONE instrument: `BTCUSD` and `BTC/USD` do (the adapter slashes a bare
/// crypto pair on the wire), so an engine trading one must not be admitted the other: two symbols in the
/// engine would book one venue position twice.
fn same_alpaca_instrument(a: &str, b: &str) -> bool {
    let bare = |s: &str| s.replace('/', "").to_ascii_uppercase();
    bare(a) == bare(b)
}

/// **deribit's symbol is the venue's own PERPETUAL name**, upper-cased, and nothing is translated:
/// `BASE-PERPETUAL` (`ETH-PERPETUAL`, the coin-margined book) or `BASE_QUOTE-PERPETUAL`
/// (`ETH_USDC-PERPETUAL`, the linear one). What else a trader may type is refused here BY NAME, before
/// any lookup: a dated future (`BTC-27MAR26`), an option (`BTC-27MAR26-60000-C`), a spot pair
/// (`BTC_USDC`) and anything else. Letters, digits, `-` and `_` only: the symbol reaches a JSON-RPC
/// frame downstream.
fn canonical_perpetual_name(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let hint = "a deribit perpetual is `BASE-PERPETUAL` or `BASE_QUOTE-PERPETUAL`, e.g. \
                `ETH-PERPETUAL`";
    let word =
        |p: &str| !p.is_empty() && p.len() <= 16 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    if upper.len() > 40
        || !upper.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(format!("`{raw}` is not a deribit instrument name ({hint})"));
    }
    let parts: Vec<&str> = upper.split('-').collect();
    match parts.as_slice() {
        [base, "PERPETUAL"] if base.split('_').count() <= 2 && base.split('_').all(word) => {
            Ok(upper.clone())
        }
        [_, _, _, "C" | "P"] => {
            Err(format!("`{raw}` is an option and this adapter trades perpetuals only ({hint})"))
        }
        [_, tail] if tail.bytes().next().is_some_and(|b| b.is_ascii_digit()) => Err(format!(
            "`{raw}` is a dated future and this adapter trades perpetuals only ({hint})"
        )),
        [_] => Err(format!(
            "`{raw}` is a spot pair or a bare symbol and this adapter trades perpetuals only ({hint})"
        )),
        _ => Err(format!("`{raw}` is not a deribit perpetual ({hint})")),
    }
}

/// Why a deribit engine cannot hold the instrument `admission` describes, or `None`. An engine counts
/// money ONE way (decision 0170 and `docs/ops/deribit-any-symbol-plan.md`): a perpetual whose amount is
/// US dollars (inverse) and one whose amount is the base coin (linear) are valued, margined and
/// settled differently, and the engine's ledger is one currency on one balance mode. So the
/// instruments an engine may add are the ones of ITS kind, and an engine mounted on anything but a
/// perpetual (a dated future, an option) adds none. The adapter checks the same rule again, and a
/// linear instrument's settlement currency as well, which the door cannot see.
fn money_model_refusal(engine: &VenueBlock, admission: &SymbolAdmission) -> Option<String> {
    if !engine.symbol.ends_with("-PERPETUAL") {
        return Some(format!(
            "this engine is mounted on `{}`, which is not a perpetual, and only an engine mounted on \
             a perpetual adds another instrument. Use an account mounted on a perpetual",
            engine.symbol
        ));
    }
    use vike_model::NotionalKind::{Inverse, Linear};
    let mounted = engine.contract_value_of(&engine.symbol).kind;
    let theirs = admission.contract_value.unwrap_or_default().kind;
    let word = |k| match k {
        Inverse => "INVERSE (its amount counts US dollars)",
        Linear => "LINEAR (its amount counts the base coin)",
    };
    (mounted != theirs).then(|| {
        format!(
            "`{}` is {} but this engine is mounted on `{}`, which is {}, and one account trades one \
             money model. Use an account mounted on a {} perpetual",
            admission.symbol,
            word(theirs),
            engine.symbol,
            word(mounted),
            if theirs == Inverse { "inverse" } else { "linear" },
        )
    })
}

/// Whether a hyperliquid symbol's TEXT is a core perpetual's name: not a spot pair (`BASE/QUOTE`, `@N`)
/// and not a builder-dex coin (`dex:COIN`). The gatewayhub's facts source reads the same three marks.
fn is_core_perp_name(symbol: &str) -> bool {
    !(symbol.contains('/') || symbol.starts_with('@') || symbol.contains(':'))
}

/// **okx's symbol is its own SWAP instId, `BASE-QUOTE-SWAP`, upper-cased, and nothing is translated**
/// (`BTCUSDT` and `BTCUSDT.P` are binance's names, not okx's: guessing a pair from them would sign a
/// different instrument than the operator meant). This adapter trades the SWAP lane only, so what
/// else a trader may type is refused here BY NAME, before any lookup: a spot id (`BTC-USDT`), a dated
/// future (`BTC-USDT-250926`), an option (`BTC-USD-250926-60000-C`), anything else. Letters and digits
/// between hyphens only: the symbol reaches a signed query string downstream.
fn canonical_swap_instid(raw: &str) -> Result<String, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let parts: Vec<&str> = upper.split('-').collect();
    let hint = "an okx SWAP instrument id is `BASE-QUOTE-SWAP`, e.g. `ETH-USDT-SWAP`";
    let word =
        |p: &&str| !p.is_empty() && p.len() <= 16 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    if upper.len() > 48 || !parts.iter().all(word) {
        return Err(format!(
            "`{raw}` is not an okx SWAP instrument id ({hint}; okx's names are its own, so \
             `BTCUSDT` and `BTCUSDT.P` are not translated)"
        ));
    }
    match parts.as_slice() {
        [_, _, "SWAP"] => Ok(upper),
        [_, _] => Err(format!(
            "`{raw}` is a spot instrument id and this adapter trades SWAP perpetuals only ({hint})"
        )),
        [_, _, tail] if tail.len() == 6 && tail.bytes().all(|b| b.is_ascii_digit()) => {
            Err(format!(
                "`{raw}` is a dated future and this adapter trades SWAP perpetuals only ({hint})"
            ))
        }
        [_, _, _, _, "C" | "P"] => Err(format!(
            "`{raw}` is an option and this adapter trades SWAP perpetuals only ({hint})"
        )),
        _ => Err(format!("`{raw}` is not an okx SWAP instrument id ({hint})")),
    }
}

/// Whether `engine`, published under `key`, is `venue`'s LANE TWIN: the second engine over the default
/// account's row, for the exec lane its default engine does not trade (`vike_secrets::lane_twin`).
/// Judged by the table's label, its mount symbol AND the tiers it is derived at (`LaneTwin::tiers`: demo and,
/// since the owner opened it on 2026-10-19, live): an account an operator labelled the same and mounted
/// elsewhere is not mistaken for it, and a block that does not say its tier is not judged to be one. And
/// `facts` may know what the block cannot: that the account under the key is an operator's REAL one that
/// spells the label ([`SymbolFacts::real_account_under_twin_label`]; the node's rows derive no twin for it),
/// which a default account's perpetual order must never be rewritten onto.
///
/// ⚠ **At `live` "no real account was reported" is not enough** (review r2, finding 7): the engine must also be
/// POSITIVELY known as the derived twin ([`SymbolFacts::derived_lane_twin`], the watcher's word), because an
/// older real `live` account spelling the label and mounted on the perpetual symbol has the twin's shape to the
/// letter and this answer decides whose keys a default account's order is rewritten onto. The demo tier keeps
/// the absence rule. A restriction that only ADDS a check (the twin's price rule, [`is_twin_shaped`]) does not
/// need the evidence: it fails closed on the shape alone.
fn is_lane_twin_engine(
    venue: &str,
    key: &str,
    engine: &VenueBlock,
    facts: Option<&dyn SymbolFacts>,
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
    facts: Option<&dyn SymbolFacts>,
) -> bool {
    vike_secrets::lane_twin(venue).is_some_and(|twin| {
        engine.symbol == twin.symbol
            && engine.tier.is_some_and(|tier| twin.tiers.contains(&tier))
            && key.strip_prefix(venue).and_then(|rest| rest.strip_prefix('#')) == Some(twin.label)
            && !facts.is_some_and(|source| source.real_account_under_twin_label(key))
    })
}

/// What the lane refusal tells the operator to do about the lane the engine does not trade.
///
/// A venue with a lane twin (`vike_secrets::lane_twin`) answers it with the account's OWN other engine:
/// a default (spot) engine is told how to get its perpetual lane (state a margin leverage, which derives
/// the twin; no restart), and the twin's engine is told its spot symbols go to the account's own spot
/// engine. Any other venue keeps the sentence this refusal always carried.
fn other_lane_hint(
    venue: &str,
    key: &str,
    engine: &VenueBlock,
    want_perp: bool,
    facts: Option<&dyn SymbolFacts>,
) -> String {
    let Some(twin) = vike_secrets::lane_twin(venue) else {
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
        // The tiers are the TABLE's (`LaneTwin::tiers`), so opening or closing one never leaves this
        // sentence naming the wrong ones.
        let tiers: Vec<&str> = twin.tiers.iter().map(|tier| tier.as_str()).collect();
        return format!(
            "or give this account its perpetual lane: state a margin leverage on its row \
             (`vike-cli secrets account set-leverage --id N --leverage X`, {} accounts only) \
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
/// runs TWO engines (a lane twin is up, `vike_secrets::lane_twin`) is put on the engine of its lane. A perpetual
/// (`.P`) goes to the twin, every other symbol to the default engine, and the address is written out in
/// both cases (`DEFAULT` for the default account).
///
/// WRITTEN OUT, because the core refuses an order that names no account on a venue it runs two engines of
/// (it will not pick one), and a client that has always left the default account unnamed would otherwise
/// have every binance order refused after its `Ack` the moment a twin starts. Only an order for the
/// DEFAULT account (absent, or `DEFAULT`) is touched: another account's order is that account's. A twin
/// that is not up leaves the order exactly as it came, and the plan's refusal names what to do.
///
/// ONE function for every surface that judges a Submit (the command, both Previews, Telegram), applied
/// before any gate reads the address, so a Preview and a command cannot disagree.
///
/// A `MountStrategy` that names no account is the second thing it addresses (the match arm says why):
/// without it, stating a margin leverage would refuse every strategy mounted at run time on the venue.
pub(crate) fn lane_addressed(
    mut cmd: WireCommand,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolFacts>,
) -> WireCommand {
    match &mut cmd {
        WireCommand::Submit(req) => {
            if let Some(account) =
                lane_account(&req.venue, &req.symbol, req.account.as_deref(), blocks, facts)
            {
                req.account = Some(account);
            }
        }
        // A BRACKET is a plain order's twin here, and for the same reason: the core refuses an account-less
        // bracket on a venue it runs two engines of, and a `.P` bracket of the default account belongs on
        // the twin (the only engine of that venue that can hold its stop-loss).
        WireCommand::Bracket(b) => {
            if let Some(account) =
                lane_account(&b.venue, &b.symbol, b.account.as_deref(), blocks, facts)
            {
                b.account = Some(account);
            }
        }
        // A strategy mount that names no account means the venue's DEFAULT account (every shipped mount
        // does), and the core refuses an account-less mount on a venue it runs two engines of. `DEFAULT`
        // names the unlabelled account and routes to it at any engine count: the engine the mount reached
        // before the twin was up.
        WireCommand::MountStrategy { venue, account, .. }
            if account.is_none() && lane_twin_up(venue, blocks, facts) =>
        {
            *account = Some(RESERVED_DEFAULT_LABEL.to_string());
        }
        _ => {}
    }
    cmd
}

/// Whether `venue` runs both its default engine and its lane twin, as the published blocks say.
fn lane_twin_up(venue: &str, blocks: &[VenueBlock], facts: Option<&dyn SymbolFacts>) -> bool {
    let Some(twin) = vike_secrets::lane_twin(venue) else { return false };
    let twin_key = format!("{venue}#{}", twin.label);
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
    facts: Option<&dyn SymbolFacts>,
) -> Option<String> {
    let twin = vike_secrets::lane_twin(venue)?;
    if !matches!(account, None | Some(RESERVED_DEFAULT_LABEL)) {
        return None;
    }
    let published = |key: &str| blocks.iter().find(|b| b.route_key == key);
    let twin_key = format!("{venue}#{}", twin.label);
    published(&twin_key).filter(|b| is_lane_twin_engine(venue, &twin_key, b, facts))?;
    if vike_catalog::split_perp(&symbol.trim().to_ascii_uppercase()).1 {
        Some(twin.label.to_string())
    } else {
        published(venue).map(|_| RESERVED_DEFAULT_LABEL.to_string())
    }
}

/// **The plan for a plain Submit's symbol**, judged from the engine's PUBLISHED block (the same
/// composer of the route key as `account_refusal`). Anything that is not a Submit, a venue this gate
/// does not own, an engine that did not say what it trades and an address nobody can parse are
/// somebody else's: `Pass`.
pub(crate) fn plan_submit_symbol(
    cmd: &WireCommand,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolFacts>,
    commit: bool,
) -> SymbolPlan {
    // A BRACKET is planned as the opening order its ENTRY is: the same price wait for the lane twin's own
    // symbol (mounted on no strategy, so no feed marks it and an entry judged with no mark vacates the
    // exposure caps) and the same refresh of an admitted symbol's facts. Its exits are never planned: they
    // are held by the core and reduce-only, and a covered reduce is never held up for a price.
    let entry;
    let req = match cmd {
        WireCommand::Submit(req) => req,
        WireCommand::Bracket(b) => {
            entry = bracket_entry_order(b);
            &entry
        }
        _ => return SymbolPlan::Pass,
    };
    let Some(rule) = rule_of(&req.venue) else { return SymbolPlan::Pass };
    let per_order = matches!(rule, SymbolRule::PerOrder | SymbolRule::PerOrderUnsafe(_));
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
    // A lane twin's OWN symbol is not passed untouched: it is mounted on no strategy, so no boot feed
    // prices it, and it goes down the same road as an admitted symbol that is already trading (the door
    // asks its price lane, once, and holds an opening order for the first price).
    let twin_own = is_twin_shaped(&req.venue, &key, engine, facts);
    if engine.symbol.is_empty() || (req.symbol == engine.symbol && !twin_own) {
        keep_own_lane(facts, commit, &key, &req.venue, engine);
        // A mount booked through a rate whose rate has aged is renewed ahead of the order (decision 0183, the
        // mounted-symbol amendment): the only way this plan is not the untouched pass.
        return match rerate_own_symbol(facts, commit, &key, &req.venue, engine) {
            Some(admit) => {
                SymbolPlan::Place { symbol: req.symbol.clone(), admit: Some(admit), wait: None }
            }
            None => SymbolPlan::Pass,
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
    let refuse = |why: String| {
        SymbolPlan::Refuse(format!(
            "submit: this node's `{key}` engine trades {} only, not `{}`: {why}. The order was \
             REFUSED",
            trades_text(engine),
            req.symbol
        ))
    };
    if per_order {
        return per_order_refusal(rule, req, engine).map_or(SymbolPlan::Pass, refuse);
    }
    let spelling = match rule {
        SymbolRule::MountedOnly(reason) => {
            // Compared with the engine's OWN symbol and nothing else: a combo leg in `extra_symbols`
            // is a symbol this adapter would still sign on the mounted instrument.
            return refuse(format!(
                "{venue}'s adapter places every order on the symbol the engine was mounted on \
                 ({reason}), so this order would have been placed on another instrument or priced \
                 on the wrong grid. Send it on a symbol that engine trades"
            ));
        }
        SymbolRule::Admits(spelling) => spelling,
        SymbolRule::PerOrder | SymbolRule::PerOrderUnsafe(_) => return SymbolPlan::Pass,
    };
    // An `Admits` venue's engine already trading a symbol (a declared leg, or an earlier admission)
    // takes the order as written; an admission of ours that has grown stale is refreshed first.
    if engine.trades(&req.symbol) {
        return already_trading(facts, &key, venue, req.symbol.clone(), req, engine, commit);
    }
    // ibkr: the engine's own symbols in another case (this venue answered them before it admitted) are the
    // engine's own spelling, whatever the account's tier.
    if spelling == Spelling::StockName
        && let Some(own) = std::iter::once(&engine.symbol)
            .chain(engine.extra_symbols.iter())
            .find(|own| own.eq_ignore_ascii_case(&req.symbol))
    {
        return if *own == engine.symbol {
            keep_own_lane(facts, commit, &key, venue, engine);
            SymbolPlan::Place { symbol: own.clone(), admit: None, wait: None }
        } else {
            already_trading(facts, &key, venue, own.clone(), req, engine, commit)
        };
    }
    // dukascopy: the engine's own pair under another spelling (`EUR/USD` for `EURUSD`, any case) is the same
    // instrument this venue took before it admitted symbols (the sidecar reads both as one), so it is the
    // engine's own symbol on every tier: placed on the engine's spelling, no lookup, no second symbol.
    if spelling == Spelling::PairName
        && let Some(own) = std::iter::once(&engine.symbol)
            .chain(engine.extra_symbols.iter())
            .find(|own| bare_pair(own) == bare_pair(&req.symbol))
    {
        return if *own == engine.symbol {
            keep_own_lane(facts, commit, &key, venue, engine);
            let admit = rerate_own_symbol(facts, commit, &key, venue, engine);
            SymbolPlan::Place { symbol: own.clone(), admit, wait: None }
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
                return refuse(
                    "this is a LIVE account with no account-wide exposure ceiling armed, and the \
                     per-symbol ceiling (`max_total_exposure`) would then apply to each new symbol on \
                     its own. Arm one first: `vike-cli config set policy.max_account_exposure <amount \
                     in the quote currency>` (applied at the next restart), or this account's own \
                     `vike-cli secrets account set-exposure`"
                        .into(),
                );
            }
        }
        Some(AccountTier::Paper) => {
            return refuse("a paper account trades only the symbol it was started on".into());
        }
        None => {
            return refuse(
                "this node does not say what kind of account this is, so it trades only the \
                 symbol it was started on"
                    .into(),
            );
        }
    }
    let symbol = match canonical_symbol(spelling, &req.symbol) {
        Ok(symbol) => symbol,
        Err(why) => return refuse(why),
    };
    if symbol == engine.symbol && !twin_own {
        keep_own_lane(facts, commit, &key, venue, engine);
        let admit = rerate_own_symbol(facts, commit, &key, venue, engine);
        return SymbolPlan::Place { symbol, admit, wait: None };
    }
    if engine.trades(&symbol) {
        return already_trading(facts, &key, venue, symbol, req, engine, commit);
    }
    // One instrument written two ways (alpaca's `BTCUSD`/`BTC/USD`, oanda's `GBPUSD`/`GBP_USD`, an ig
    // epic in another case) is ONE symbol in an engine, or one venue position would be booked twice.
    let same_instrument: Option<fn(&str, &str) -> bool> = match spelling {
        Spelling::AssetSymbol => Some(same_alpaca_instrument),
        Spelling::OandaInstrument => Some(same_oanda_instrument),
        Spelling::Epic => Some(same_ig_epic),
        _ => None,
    };
    if let Some(same) = same_instrument
        && let Some(own) = std::iter::once(&engine.symbol)
            .chain(engine.extra_symbols.iter())
            .find(|own| same(own, &symbol))
    {
        return refuse(format!(
            "`{symbol}` is the instrument this engine already trades as `{own}` ({venue} writes this \
             instrument in more than one spelling), and one instrument is one symbol in an engine. \
             Send the order on `{own}`"
        ));
    }
    if spelling == Spelling::StockName {
        if let Some(own) = std::iter::once(&engine.symbol)
            .chain(engine.extra_symbols.iter())
            .find(|own| same_ibkr_stock(own, &symbol))
        {
            return refuse(format!(
                "`{symbol}` is the stock this engine already trades as `{own}` (one stock can be \
                 spelled with a named exchange), and one stock is one symbol in an engine. Send the \
                 order on `{own}`"
            ));
        }
        // The engine counts money in the currency of its own contract, and nothing converts, EXCEPT for an
        // account that converts a quote currency (decision 0183): then a stock of another currency is looked
        // up, and admitted only with the conversion the facts carry (`quote_refusal` and
        // `stock_conversion_missing` below).
        let money = ibkr_engine_money(&engine.symbol);
        let stock_money = ibkr_engine_money(&symbol);
        if money.is_none() || money != stock_money {
            // The account's REAL ceiling bit: a demo account with a ceiling armed, mounted on a forex pair or a
            // contract that states no currency, is not told it "needs a ceiling" (it has one; its mount is why).
            let ceiling = facts.is_some_and(|source| source.exposure_cap_armed(&key));
            let converting = money.is_some()
                && stock_money.is_some()
                && !ibkr_forex_name(&engine.symbol)
                && quote_conversion_refusal(venue, engine.tier, ceiling).is_none();
            if !converting {
                let hint = quote_conversion_refusal(venue, engine.tier, ceiling)
                    .map(|why| format!(" (a stock of another currency needs a conversion: {why})"))
                    .unwrap_or_default();
                return refuse(format!(
                    "`{symbol}` is a {} stock and this engine, mounted on `{}`, counts its money in {} (the \
                     ledger is one currency and nothing converts). Use an account mounted on a {} \
                     contract{hint}",
                    stock_money.as_deref().unwrap_or("?"),
                    engine.symbol,
                    money.as_deref().unwrap_or("an unknown currency"),
                    stock_money.as_deref().unwrap_or("matching")
                ));
            }
        }
    }
    // dukascopy: the engine counts money in the quote of its own pair and nothing converts (decision 0170),
    // so a pair is admitted only when it is quoted in the same money. A name that is not a pair is the
    // gatewayhub table's to refuse by name. EXCEPT for an account that converts (decision 0183: a `demo` account
    // with an account-wide ceiling armed, [`quote_conversion_refusal`]): a pair quoted in another money is then
    // the facts' to judge, and it is admitted only with the currency and the rate they state (checked below, so
    // a gatewayhub that states neither never lets a foreign pair through at par).
    let mut foreign_money: Option<String> = None;
    if spelling == Spelling::PairName && is_pair(&symbol) {
        // The money the LEDGER counts in. An engine that was told a non-dollar currency (`VenueBlock::ledger_ccy`,
        // empty for the dollar group and for an engine nobody told) counts in THAT whatever its mount is quoted in:
        // before decision 0183's mounted-symbol amendment the two were the same by the mount fence (a ledger was
        // told only the money its mount is quoted in), and now a EUR ledger over a USD-quoted mount converts the
        // mount like any pair. Every other engine counts in its mount's money, as it always did.
        let money = if engine.ledger_ccy.is_empty() {
            pair_money(&engine.symbol)
        } else {
            Some(engine.ledger_ccy.to_string())
        };
        let quote = pair_money(&symbol);
        let converts = facts.is_some_and(|source| {
            quote_conversion_refusal(venue, engine.tier, source.exposure_cap_armed(&key)).is_none()
        });
        if money.is_some() && money != quote && converts {
            foreign_money = quote.clone();
        } else if money.is_none() || money != quote {
            return refuse(format!(
                "`{symbol}` is quoted in {} and this engine, mounted on `{}`, counts its money in {} \
                 (the ledger is one currency and nothing converts). Use an account mounted on a pair \
                 quoted in {}",
                quote.as_deref().unwrap_or("?"),
                engine.symbol,
                money.as_deref().unwrap_or("an unknown currency"),
                quote.as_deref().unwrap_or("the same money")
            ));
        }
    }
    if spelling == Spelling::LaneSuffixed {
        let engine_perp = vike_catalog::split_perp(&engine.symbol).1;
        let want_perp = vike_catalog::split_perp(&symbol).1;
        if engine_perp != want_perp {
            let (this, other) = if engine_perp { ("perp", "spot") } else { ("spot", "perp") };
            return refuse(format!(
                "`{symbol}` is a {other} symbol and this engine runs {venue}'s {this} lane (an \
                 engine has one lane and one user-data stream, so a fill on the other lane would \
                 never arrive). Use a {this} symbol, {}",
                other_lane_hint(venue, &key, engine, want_perp, facts)
            ));
        }
    }
    // A hyperliquid engine mounted on a spot pair (`HYPE/USDC`, `@N`) or a builder-dex coin runs that
    // product: its balance read, fills channel and asset ids are not a core perp's, so a core perp
    // is not admitted into it (the lane rule above, for the one venue whose lanes are not suffixes).
    if spelling == Spelling::CaseKept && !is_core_perp_name(&engine.symbol) {
        return refuse(format!(
            "this engine runs {venue}'s spot or builder-dex market (`{}`), not its core perpetuals, \
             and an engine has one product (balance, fills and asset ids differ). Use an account \
             mounted on a core perpetual",
            engine.symbol
        ));
    }
    if engine.extra_symbols.len() >= MAX_EXTRA_SYMBOLS {
        return refuse(format!(
            "an engine admits at most {MAX_EXTRA_SYMBOLS} symbols beyond the one it was started on"
        ));
    }
    let Some(source) = facts else {
        return refuse(
            "this surface cannot look up a symbol's facts (its grid and contract), so it cannot \
             admit a new symbol; place the order from the Trade window or the CLI"
                .into(),
        );
    };
    // An opening order needs a price at the end ([`with_price`]); a venue this node cannot price at all
    // refuses it now, BEFORE the lookup spends a venue connection on an order that can never succeed.
    if !req.reduce_only
        && let Err(why) = source.price_source(venue)
    {
        return refuse(format!("no price feed for `{symbol}`: {why}"));
    }
    let admission = match source.admission(&key, venue, &symbol) {
        Ok(admission) => admission,
        Err(why) => return refuse(format!("cannot admit `{symbol}`: {why}")),
    };
    if admission.route_key != key || admission.symbol != symbol {
        return refuse(format!(
            "the facts source answered for `{}` on `{}` when `{symbol}` on `{key}` was asked",
            admission.symbol, admission.route_key
        ));
    }
    if admission.grid.tick_size.is_none() || admission.grid.lot_size.is_none() {
        let missing = match (admission.grid.tick_size, admission.grid.lot_size) {
            (None, None) => "tick size and lot size",
            (None, Some(_)) => "tick size",
            _ => "lot size",
        };
        return refuse(format!(
            "{venue} publishes no {missing} for `{symbol}`, so its price and size cannot be \
             rounded on its own grid"
        ));
    }
    // deribit counts money in two ways, and an engine in one (the module doc).
    if spelling == Spelling::PerpetualName
        && let Some(why) = money_model_refusal(engine, &admission)
    {
        return refuse(why);
    }
    // A symbol quoted in a currency other than the account's needs a rate and an account that converts.
    if let Some(why) = quote_refusal(venue, engine, &admission, source, &key) {
        return refuse(why);
    }
    // ibkr: a stock of a currency other than the engine's money was let past the lookup only because the
    // account converts, and is admitted only with the conversion in the facts. The facts of a gatewayhub that
    // does not convert (an older one, or a lease that asked the old way) carry none, and would put the stock
    // in the ledger at par.
    if spelling == Spelling::StockName
        && let Some(why) = stock_conversion_missing(engine, &admission)
    {
        return refuse(why);
    }
    // A pair the door read as quoted in another money than the engine's (dukascopy, above) is admitted only
    // when the facts state THAT currency: an answer with none is a gatewayhub that did not convert, and the
    // pair would be booked at par.
    if let Some(expected) = &foreign_money
        && admission.quote_ccy.as_deref() != Some(expected.as_str())
    {
        return refuse(format!(
            "`{symbol}` is quoted in {expected} and this account's money is another currency, but the \
             facts for it state {} (a gatewayhub that does not convert answers none). The symbol is NOT \
             admitted (no rate, no admission: it is never valued at par)",
            admission.quote_ccy.as_deref().unwrap_or("no currency")
        ));
    }
    // A pair in the MOUNT's own money for which the facts nonetheless state a currency (the review's F-1). An
    // anchored engine has mount money == account money, and the facts state a currency only for a pair quoted in
    // something other than the account's, so a stated currency here means this engine is NOT anchored (the mount's
    // money is not the account's): the engine would refuse the admission (`account-ccy-unset`) and deny the order,
    // but only AFTER the node had committed the symbol (`LeaseFacts::admitted` makes its price pump permanent and
    // a place of the venue's lane is held until a restart). Refused here, before any of that.
    if spelling == Spelling::PairName
        && is_pair(&symbol)
        && foreign_money.is_none()
        && let Some(ccy) = admission.quote_ccy.as_deref()
    {
        return refuse(format!(
            "`{symbol}` is quoted in {ccy}, the money this engine's mounted pair `{}` counts in, but the facts \
             state {ccy} against an account currency that is another one: the engine's ledger is not anchored \
             to the account's currency, so it would refuse the admission (`account-ccy-unset`) and a price \
             place would be held for a symbol it never trades. The symbol is NOT admitted. Use an account \
             mounted on a pair quoted in the account's currency",
            engine.symbol
        ));
    }
    // The slot is spent only by an order worth admitting for. An admission is permanent for the life of
    // the process (the engine has no un-admit and caps a venue account at `MAX_EXTRA_SYMBOLS`), so a
    // reduce-only order (it can only fail: the engine holds nothing in a symbol it was never
    // told about) and an order the venue's own grid would reject must not be able to burn one.
    if req.reduce_only {
        return refuse(
            "a reduce-only order can only close a position this account holds in the symbol, and it \
             holds none; send an opening order to start trading it"
                .into(),
        );
    }
    if let Some(why) = grid_refusal(req, &admission.grid) {
        return refuse(format!("{why} (no symbol slot was spent)"));
    }
    match with_price(req, facts, &key, venue, symbol, Some(Box::new(admission)), commit) {
        SymbolPlan::Refuse(why) => refuse(why),
        other => other,
    }
}

/// **Why a symbol quoted in a currency other than the account's cannot be admitted, or `None`** (decision
/// 0183, phase 1). An admission that names no currency is quoted in the account's own (the daemon's fence
/// guarantees it) and must carry no rate either. One that names a currency is admitted only with a usable
/// rate (finite, positive, stamped: the engine refuses anything else, and so does the door, first, in
/// words), and only for an account that converts at all: OANDA's practice account with an account-wide
/// exposure ceiling armed ([`quote_conversion_refusal`], whose tier half the engine's assembly reads too).
/// Freshness is the facts source's to judge against its clock (`LeaseFacts`); the engine judges it again
/// against its own.
fn quote_refusal(
    venue: &str,
    engine: &VenueBlock,
    admission: &SymbolAdmission,
    source: &dyn SymbolFacts,
    key: &str,
) -> Option<String> {
    let symbol = &admission.symbol;
    let Some(ccy) = admission.quote_ccy.as_deref() else {
        return (admission.quote_rate.is_some() || admission.rate_as_of_ms.is_some()).then(|| {
            format!(
                "{venue}: the facts for `{symbol}` carry a conversion rate and no currency to apply \
                 it to, so the symbol is NOT admitted"
            )
        });
    };
    let usable = admission.quote_rate.is_some_and(|rate| rate.is_finite() && rate > 0.0)
        && admission.rate_as_of_ms.is_some();
    if !usable {
        return Some(format!(
            "`{symbol}` is quoted in {ccy} and this account's money is another currency, so it is \
             admitted only with a usable conversion rate, and the facts carry none. The symbol is NOT \
             admitted (no rate, no admission: it is never valued at par)"
        ));
    }
    quote_conversion_refusal(venue, engine.tier, source.exposure_cap_armed(key))
        .map(|why| format!("`{symbol}` is quoted in {ccy}: {why}"))
}

/// Why an ibkr stock of another currency than the engine's money is not admitted because the facts carry no
/// conversion into it, or `None`. A stock in the engine's own currency needs none. An absent currency is never
/// read as the engine's: the facts must NAME the stock's currency as the one they convert.
fn stock_conversion_missing(engine: &VenueBlock, admission: &SymbolAdmission) -> Option<String> {
    let money = ibkr_engine_money(&engine.symbol)?;
    let stock = ibkr_engine_money(&admission.symbol)?;
    if money.eq_ignore_ascii_case(&stock) {
        return None;
    }
    let converts = admission.quote_ccy.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(&stock))
        && admission.quote_rate.is_some();
    (!converts).then(|| {
        format!(
            "`{}` is a {stock} stock and this engine counts its money in {money}, and the facts carry \
             no conversion of {stock} into it (the gatewayhub converts a quote currency only for a \
             `demo` account whose ledger is anchored to the account's currency), so it is NOT admitted \
             (it is never valued at par)",
            admission.symbol
        )
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
fn grid_refusal(
    req: &vike_tradehub_client::wire::WireOrderRequest,
    grid: &vike_model::SymbolGrid,
) -> Option<String> {
    let qty = req.qty.abs();
    if let Some(min) = grid.min_qty
        && qty < min
    {
        return Some(format!("the quantity {qty} is below the symbol's minimum {min}"));
    }
    if let Some(lot) = grid.lot_size
        && qty < lot
    {
        return Some(format!("the quantity {qty} is below one lot ({lot})"));
    }
    let price = req.price.or(req.trigger_price)?;
    let min = grid.min_notional?;
    (qty * price.abs() < min).then(|| {
        format!(
            "the order's value {} is below the symbol's minimum notional {min}",
            qty * price.abs()
        )
    })
}

/// Why a per-order venue's engine cannot take `req`, a symbol it was not started on, or `None`: its
/// adapter reads the order's symbol, but the engine is still ONE book on the mounted symbol's grid.
fn per_order_refusal(
    rule: SymbolRule,
    req: &vike_tradehub_client::wire::WireOrderRequest,
    engine: &VenueBlock,
) -> Option<String> {
    // Polymarket judges every token its engine is given (its mounts hold one, or none); live accounts
    // run on this row, so it keeps today's behaviour exactly.
    if req.venue == "polymarket" {
        return None;
    }
    // The paper client stamps its own symbol on every fill and runs one single-symbol book.
    if engine.tier == Some(AccountTier::Paper) {
        return Some("a paper account trades only the symbol it was started on".into());
    }
    if let SymbolRule::PerOrderUnsafe(reason) = rule {
        return Some(format!(
            "{}'s adapter reads the order's symbol, but the engine cannot judge another one \
             ({reason}). Send it on a symbol that engine trades",
            req.venue
        ));
    }
    // No per-venue price watcher exists for these venues: a priceless opening order is judged at
    // notional 0 and passes every ceiling, margin check and collar. (Reduce-only closes what exists.)
    (!req.reduce_only && req.price.is_none()).then(|| {
        format!(
            "{} has no price source for a symbol the engine was not started on, so an opening \
             order without a limit price would be judged at notional 0 (every ceiling, the \
             margin check and the collar would pass). Send a limit price, or a reduce-only order",
            req.venue
        )
    })
}

/// **The order names the engine's OWN symbol: keep its price lane** (decision 0197; [`SymbolFacts::hold_lane`]).
/// The engine's own symbol passes the door untouched (no price asked, no admission, no wait), and a `demo`/`live`
/// engine that goes on to hold a position or an order in it would be priced by nobody unless a strategy is mounted
/// there. So a COMMITTING surface (never a Preview) asks the source to hold the lane, and the plan is exactly what
/// it was: no wait for a quote, no refusal, a limit order a closed market accepts stays accepted. A `paper`
/// engine, an engine with no symbol and a surface with no source ask nothing.
fn keep_own_lane(
    facts: Option<&dyn SymbolFacts>,
    commit: bool,
    key: &str,
    venue: &str,
    engine: &VenueBlock,
) {
    if commit
        && !engine.symbol.is_empty()
        && matches!(engine.tier, Some(AccountTier::Demo | AccountTier::Live))
        && let Some(source) = facts
    {
        source.hold_lane(key, venue, &engine.symbol);
    }
}

/// **The order names the engine's OWN symbol and the engine books it through a rate that has aged: the fresh
/// admission to send ahead of the order** (decision 0183, the mounted-symbol amendment; [`SymbolFacts::own_symbol_rate`]).
/// Only a COMMITTING surface (a Preview judges the same verdict without the lookup) and only a `demo` engine (a
/// conversion is demo-only: `quote_conversion_tier_refusal`) ask; every engine that books its own symbol at par
/// (all but a non-USD account on a USD-quoted mount) gets `None` from the source and the plan is the untouched pass.
fn rerate_own_symbol(
    facts: Option<&dyn SymbolFacts>,
    commit: bool,
    key: &str,
    venue: &str,
    engine: &VenueBlock,
) -> Option<Box<SymbolAdmission>> {
    if commit
        && !engine.symbol.is_empty()
        && engine.tier == Some(AccountTier::Demo)
        && let Some(source) = facts
    {
        source.own_symbol_rate(key, venue, &engine.symbol, engine).map(Box::new)
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
/// denied `no-reference-price` for ever). A real account's order therefore ASKS the price lane here too
/// (idempotent: a subscribed symbol is left alone), and an opening order with no price yet is held for the
/// first one ([`PriceWait`]) exactly as for a new symbol. A failed ask changes nothing: the engine's own
/// `no-reference-price` stays the fail-closed answer.
///
/// ⚠ **EXCEPT for a lane twin's own symbol, where it is no answer at all.** `no-reference-price` asks only of an
/// `on_demand` row and the twin's own symbol is its MOUNTED one, so an engine with no mark there judges a market
/// order at notional zero (every cap passes) and a limit order against itself. An opening order on it is therefore
/// refused when the price cannot be asked for, in words ([`engine_own_price_refusal`]), and the price it waits for
/// is the tap's `door_price`, which is "none yet" until the core was handed the symbol's mark (`TapSink`).
fn already_trading(
    facts: Option<&dyn SymbolFacts>,
    key: &str,
    venue: &str,
    symbol: String,
    req: &vike_tradehub_client::wire::WireOrderRequest,
    engine: &VenueBlock,
    commit: bool,
) -> SymbolPlan {
    let admit = facts
        .filter(|_| commit)
        .and_then(|source| source.refresh(key, venue, &symbol))
        .filter(|a| a.route_key == key && a.symbol == symbol)
        .map(Box::new);
    let real = matches!(engine.tier, Some(AccountTier::Demo | AccountTier::Live));
    let asked =
        facts.filter(|_| commit && real).map(|source| source.price(key, venue, &symbol, true));
    if real
        && !req.reduce_only
        && symbol == engine.symbol
        && is_twin_shaped(venue, key, engine, facts)
        && let Some(why) = engine_own_price_refusal(key, &symbol, facts.is_some(), asked.as_ref())
    {
        return SymbolPlan::Refuse(why);
    }
    // A LIVE lane twin trades another symbol only at a leverage the exchange was SEEN holding for it.
    let mut admit = admit;
    if !req.reduce_only && live_twin_extra_symbol(venue, key, engine, facts, &symbol) {
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
    let wait = (matches!(asked, Some(Ok(None))) && !req.reduce_only)
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
    facts: Option<&dyn SymbolFacts>,
    symbol: &str,
) -> bool {
    engine.tier == Some(AccountTier::Live)
        && symbol != engine.symbol
        && is_twin_shaped(venue, key, engine, facts)
}

/// The refusal for an opening order whose symbol would have to be admitted (and its leverage committed to the
/// exchange) while the engine's trading state is not `Active` ([`accept_command`]'s gate, review r2 item 9).
pub(crate) fn admission_refusal_while_not_active(
    route_key: &str,
    symbol: &str,
    state: vike_model::TradingState,
) -> String {
    let why = match state {
        vike_model::TradingState::Halted => "this node is HALTED (the kill switch is on)",
        _ => "this node is REDUCING (only position-reducing orders are allowed)",
    };
    format!(
        "submit: {why}, and admitting `{symbol}` into `{route_key}` commits its leverage to the exchange, \
         which can change the account's leverage for that symbol there; a node that is not trading changes \
         nothing on the exchange. The order was REFUSED and nothing was sent: resume trading first \
         (`vike-cli trade resume`), or place the order on a symbol the account already trades"
    )
}

/// The refusal for an opening order on a LIVE lane twin's extra symbol whose leverage the exchange has not
/// confirmed ([`live_twin_extra_symbol`]).
pub(crate) fn unconfirmed_leverage_refusal(key: &str, symbol: &str) -> String {
    format!(
        "submit: the perp lane `{key}` is a LIVE account and the exchange has not confirmed that it holds the \
         account's stated leverage for `{symbol}`: the figure is made real and read back when the symbol is \
         admitted, and a symbol that trades at whatever leverage the exchange already holds for it would be \
         leverage nobody chose on real money. The order was REFUSED; the next attempt asks the exchange \
         again (the gatewayhub's log says why it was not confirmed)"
    )
}

/// **An opening order with NO price of its own (a market or stop-market), or a bracket with a MARKET entry, on a
/// LIVE lane twin's own symbol**: the `(route key, symbol)` the door must size it for, or `None`. The door's notional ceiling cannot size a priceless
/// order on a linear contract (`ControlLimits::vet_notional`: `(None, Linear) => None`, "the core's to judge"),
/// and the core judges it at the engine's MARK, which a twin's own symbol may not have had yet (the mounted
/// symbol is priced in the twin only by the mark `TapSink` hands the core, and a cold or restored engine holds
/// none): notional zero, every cap passing. So on the one engine that reaches MAINNET through this path the
/// door sizes the order itself, at the price its own look-up holds ([`accept_command`]), and the ceiling binds
/// whatever the engine knows. Demo keeps what it had.
pub(crate) fn live_twin_unsized_open(
    cmd: &WireCommand,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolFacts>,
) -> Option<(String, String)> {
    // A plain opening order with no price, or a BRACKET with a MARKET entry: the entry opens the position, and
    // the door's ceiling sizes a bracket by its priced legs alone (its stop-loss and take-profit, which a
    // client can set far under the entry's worth: `check_bracket` only asks the stop under the target).
    let (venue, symbol, account) = match cmd {
        WireCommand::Submit(req) if !req.reduce_only && req.price.is_none() => {
            (&req.venue, &req.symbol, req.account.as_deref())
        }
        WireCommand::Bracket(b) if b.entry_price.is_none() => {
            (&b.venue, &b.symbol, b.account.as_deref())
        }
        _ => return None,
    };
    let label = match account {
        None => AccountLabel::Default,
        Some(text) => parse_wire_account(text).ok()?,
    };
    let key = route_key_of(venue, &label);
    let engine = blocks.iter().find(|b| b.route_key == key)?;
    let own = symbol.trim().eq_ignore_ascii_case(&engine.symbol);
    (own && engine.tier == Some(AccountTier::Live) && is_twin_shaped(venue, &key, engine, facts))
        .then(|| (key, engine.symbol.clone()))
}

/// `cmd`, the command [`live_twin_unsized_open`] found, with the price it lacks filled in at `px` so the door's
/// ceiling can size it: a plain order gets `px` as its price (a stop-market the higher of its trigger and `px`),
/// a bracket gets `px` as its entry price. `None` for any other command.
pub(crate) fn priced_for_sizing(cmd: &WireCommand, px: f64) -> Option<WireCommand> {
    match cmd {
        WireCommand::Submit(req) => {
            let mut sized = req.clone();
            sized.price = Some(sized.trigger_price.map_or(px, |trigger| trigger.max(px)));
            Some(WireCommand::Submit(sized))
        }
        WireCommand::Bracket(b) => {
            let mut sized = b.clone();
            sized.entry_price = Some(px);
            Some(WireCommand::Bracket(sized))
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
) -> Option<String> {
    if !has_source {
        return Some(format!(
            "submit: the perp lane `{key}` has no price for `{symbol}` in its engine, and this surface cannot \
             ask the node's price feed for one, so the order's size could not be judged against any ceiling. \
             The order was REFUSED: place it from the Trade window or the CLI"
        ));
    }
    match asked {
        Some(Err(why)) => Some(format!(
            "submit: the perp lane `{key}` has no price for `{symbol}` in its engine yet, and no price feed can \
             be had for it: {why}. The order was REFUSED"
        )),
        _ => None,
    }
}

/// Finish a placement of a NEW symbol: an OPENING order needs a price (the collar, the exposure caps
/// and the account margin are all judged at the mark, and an admitted symbol has none until its feed
/// delivers one). With `commit` the source subscribes the feed (once); a Preview only READS. A price
/// already here is a plain placement; none yet is a placement that must WAIT ([`PriceWait`], only
/// when committing); a feed that cannot be had is a refusal. A reduce-only order is never held up or
/// refused for want of a price, but still subscribes the feed so the symbol is priced afterwards.
fn with_price(
    req: &vike_tradehub_client::wire::WireOrderRequest,
    facts: Option<&dyn SymbolFacts>,
    key: &str,
    venue: &str,
    symbol: String,
    mut admit: Option<Box<SymbolAdmission>>,
    commit: bool,
) -> SymbolPlan {
    let mut wait = None;
    if let Some(source) = facts {
        match source.price(key, venue, &symbol, commit) {
            // The price seen NOW rides the admission as the engine's first mark for the symbol.
            Ok(Some(px)) => {
                if let Some(a) = admit.as_mut() {
                    a.initial_mark = Some(px);
                }
            }
            Ok(None) if !req.reduce_only && commit => {
                wait = Some(PriceWait { route_key: key.to_string(), venue: venue.to_string() });
            }
            Ok(None) => {}
            Err(why) if !req.reduce_only => {
                return SymbolPlan::Refuse(format!("no price feed for `{symbol}`: {why}"));
            }
            Err(_) => {}
        }
    }
    SymbolPlan::Place { symbol, admit, wait }
}

/// The plain OPENING order a bracket's entry is (a limit at its price, a market without one), for
/// [`plan_submit_symbol`] only. It is never sent: the bracket lowers to its own `OrderIntent::Bracket`.
fn bracket_entry_order(
    b: &vike_tradehub_client::wire::WireBracketSpec,
) -> vike_tradehub_client::wire::WireOrderRequest {
    vike_tradehub_client::wire::WireOrderRequest {
        client_order_id: String::new(),
        venue: b.venue.clone(),
        symbol: b.symbol.clone(),
        side: b.side,
        qty: b.qty,
        order_type: if b.entry_price.is_some() { "limit" } else { "market" }.to_string(),
        price: b.entry_price,
        trigger_price: None,
        reduce_only: false,
        account: b.account.clone(),
    }
}

/// Whether judging this command would cost the facts source a lookup: a Submit on an admitting venue
/// for a symbol the addressed engine does not already trade. The Preview takes a rate token for
/// exactly these (a lookup is a gatewayhub round trip a peer could otherwise flood for free).
pub(crate) fn needs_lookup(cmd: &WireCommand, blocks: &[VenueBlock]) -> bool {
    let WireCommand::Submit(req) = cmd else { return false };
    if !matches!(rule_of(&req.venue), Some(SymbolRule::Admits(_))) {
        return false;
    }
    let label = match req.account.as_deref() {
        None => AccountLabel::Default,
        Some(text) => match parse_wire_account(text) {
            Ok(label) => label,
            Err(_) => return false,
        },
    };
    let key = route_key_of(&req.venue, &label);
    blocks
        .iter()
        .find(|b| b.route_key == key)
        .is_some_and(|b| !b.symbol.is_empty() && req.symbol != b.symbol && !b.trades(&req.symbol))
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
pub(crate) fn foreign_contract_value(
    cmd: &WireCommand,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolFacts>,
) -> Result<Option<vike_model::ContractValue>, String> {
    if !needs_lookup(cmd, blocks) {
        return Ok(None);
    }
    match plan_submit_symbol(cmd, blocks, facts, false) {
        SymbolPlan::Place { admit: Some(admission), .. } => {
            Ok(effective_contract_value(&admission))
        }
        SymbolPlan::Refuse(why) => Err(why),
        SymbolPlan::Place { admit: None, .. } | SymbolPlan::Pass => Ok(None),
    }
}

/// The refusal the Preview surfaces print: [`plan_submit_symbol`]'s `Refuse`, nothing else. A
/// PREVIEW: it reads (a facts lookup is cached and read-only) and never subscribes a price feed,
/// refreshes a grid or waits.
pub(crate) fn submit_symbol_refusal(
    cmd: &WireCommand,
    blocks: &[VenueBlock],
    facts: Option<&dyn SymbolFacts>,
) -> Option<String> {
    match plan_submit_symbol(cmd, blocks, facts, false) {
        SymbolPlan::Refuse(why) => Some(why),
        SymbolPlan::Pass | SymbolPlan::Place { .. } => None,
    }
}

/// How long the door holds an opening order for the first price of a symbol whose feed it just
/// subscribed. A pump delivers its first quote in well under a second; past this the order is
/// refused and the operator retries (the subscription stays).
///
/// ⚠ Built from `vike_config::config::DOOR_PRICE_WAIT_MS`, the number the CLIENT's default ack wait
/// (`config.node_ack_wait_ms`) is derived from, so the client outlasts this hold and reads its refusal.
/// Moving the hold means moving that one constant; `the_default_ack_wait_outlasts_the_doors_price_hold`
/// (`server/tests/admits_any_symbol.rs`) is the pin that the two cannot drift apart.
pub(crate) const PRICE_WAIT: std::time::Duration =
    std::time::Duration::from_millis(vike_config::config::DOOR_PRICE_WAIT_MS as u64);

/// Hold the order until `wait`'s symbol has a price, or say why not. Blocks the CONNECTION thread
/// (up to [`PRICE_WAIT`]), never the core, and never at all for a source that does not
/// [`SymbolFacts::waits`] (it gets one look and a prompt "retry").
pub(crate) fn await_price(
    facts: &dyn SymbolFacts,
    wait: &PriceWait,
    symbol: &str,
) -> Result<f64, String> {
    let deadline = std::time::Instant::now() + PRICE_WAIT;
    loop {
        match facts.price(&wait.route_key, &wait.venue, symbol, true) {
            Ok(Some(px)) => return Ok(px),
            Ok(None) if facts.waits() && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) => {
                return Err(format!(
                    "submit: `{symbol}` has no price on this node yet (its price feed was just \
                     subscribed and nothing has arrived{}). The order was REFUSED: retry in a moment",
                    if facts.waits() {
                        format!(" in {} s", PRICE_WAIT.as_secs())
                    } else {
                        String::new()
                    }
                ));
            }
            Err(why) => return Err(format!("submit: no price feed for `{symbol}`: {why}")),
        }
    }
}
