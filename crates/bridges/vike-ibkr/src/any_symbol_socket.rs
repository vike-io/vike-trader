//! The SOCKET backend of the any-symbol lookup (`crate::any_symbol`): the three requests over one ibapi
//! connection of the logged-in IB Gateway, and the [`IbkrFactsSource`] the gatewayhub asks.
//!
//! # ONE data connection per gateway session, shared with the price stream, through the DATA lane of the gate
//!
//! A lookup takes a lease on the session's connection under the account's `data_client_id`
//! (`crate::data_session`), asks the three requests on it, and gives the lease back:
//!
//! * a connection **the price stream (`IbkrFeeds`) or another lookup already holds is JOINED**, with no dial at
//!   all: IB refuses a second client under an id in use (error 326), so a lookup that dialled its own beside a
//!   live stream would answer "unavailable" for every name. The stream and this module meet in ONE process-wide
//!   session table (`crate::data_session`'s `SESSIONS`, keyed by gateway address and `data_client_id`), and both
//!   run inside the gatewayhub, so the join is real;
//! * otherwise the lease **dials once** through `market_feed::dial_data_client`, i.e. the gate's data turn
//!   (`GatewayGate::dial_data`), which **can never delay an order-lane dial** (the exec connect, the grid
//!   pre-fetch, the reconcile connect): refused at once, nothing dialled, while any dial holds the turn or an
//!   order-lane dial waits, while the rebuild schedule is armed, and while the data lane's backoff runs; it
//!   gives way the moment an order-lane dial arrives;
//! * that dial **spends the data lane's budget**: the gate allows `MAX_ATTEMPTS_PER_HOUR` (6) data connects an
//!   hour per gateway, and this lane (`DataLane::Lookup`) may spend at most four of them (`LANE_RESERVE` keeps two
//!   for the stream's re-dial). The lane above keeps an answer for ten minutes and a refusal for thirty seconds,
//!   so a name asked twice costs one dial, and none while the stream is up. That floor is the gate's, not this
//!   module's, and it is the price of never touching the order lane;
//! * **one ask is one [`FactsScope`]**: an ask that reads several things (a stock's facts, the account's base
//!   currency, a forex rate) takes the connection ONCE and every read of the ask rides it, so the ask costs at
//!   most one dial, and a new foreign stock costs two in all (the ask's and the stream's first). A read that
//!   needs no connection (a stock in the base currency already known, a rate read less than [`RATE_KEEP`] ago)
//!   takes none and spends nothing;
//! * the connection is **closed when the last lease is dropped**: a lookup alone closes it right after its
//!   answer (so it never overlaps the grid pre-fetch that shares the `data_client_id`, which also joins a live
//!   one), a lookup beside a stream leaves it to the stream. The requests run on a bounded thread (they
//!   have no timeout of their own), so a gateway that stops answering is "unavailable" within the bound of the
//!   read ([`LOOKUP_DEADLINE`], [`RATE_DEADLINE`]).
//!   `HistoricalFetcher` still dials its own connection under the id (error 326 beside a live stream).
//!
//! The matching request is paced by the gateway (the research notes say 1 s apart); one lookup makes one,
//! and the gate spaces the dials by `CONNECT_GAP`. A cpapi account is REFUSED by name: the Client Portal
//! has `secdef/search` and `info-and-rules`, neither of which this module reads, and a cpapi box opens no
//! ibapi socket (decision 0140).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ibapi::client::blocking::Client;
use ibapi::contracts::{
    Contract, ContractDescription, ContractDetails, Currency, Exchange, MarketRule, SecurityType,
    Symbol,
};

use crate::any_symbol::{
    ContractLookup, DetailsRow, FactsError, MatchRow, PriceBand, ROUTE, StockFacts, checked_name,
    parse_stock_name, stock_facts, stock_facts_any_currency,
};
use crate::config::IbkrConfig;
use crate::data_session::{Hold, session_for};
use crate::fx_rate::{QuoteRate, check_against_previous, minor_unit_refusal, quote_rate};
use crate::fx_socket::{SocketFx, base_currency};
use crate::market_feed::{dial_data_client_by, refuse_cpapi};
use crate::transport::gateway_gate::{AskDeadline, DataLane};
use crate::transport::socket::{OPEN_DEADLINE, bounded};

/// How long one lookup may take end to end, connect included. Above the gate's usual 15 s open bound: it is
/// four requests, not one.
pub const LOOKUP_DEADLINE: Duration = Duration::from_secs(25);

/// The lookup over one connected ibapi client.
struct SocketLookup<'a> {
    client: &'a Client,
}

impl ContractLookup for SocketLookup<'_> {
    fn matching_symbols(&self, pattern: &str) -> Result<Vec<MatchRow>, String> {
        let rows = self.client.matching_symbols(pattern).map_err(|e| e.to_string())?;
        Ok(rows.iter().map(match_row).collect())
    }

    fn contract_details(&self, contract: &MatchRow) -> Result<Vec<DetailsRow>, String> {
        let asked = Contract {
            contract_id: contract.conid,
            symbol: Symbol::from(contract.symbol.as_str()),
            security_type: SecurityType::Stock,
            exchange: Exchange::from(ROUTE),
            currency: Currency::from(contract.currency.as_str()),
            ..Contract::default()
        };
        let rows = self.client.contract_details(&asked).map_err(|e| e.to_string())?;
        Ok(rows.iter().map(details_row).collect())
    }

    fn market_rule(&self, id: i32) -> Result<Vec<PriceBand>, String> {
        let rule = self.client.market_rule(id).map_err(|e| e.to_string())?;
        Ok(price_bands(&rule))
    }
}

/// One matching-symbols row in the seam's words.
pub(crate) fn match_row(row: &ContractDescription) -> MatchRow {
    MatchRow {
        conid: row.contract.contract_id,
        symbol: row.contract.symbol.0.clone(),
        sec_type: row.contract.security_type.to_string(),
        primary_exchange: row.contract.primary_exchange.0.clone(),
        currency: row.contract.currency.0.clone(),
    }
}

/// One contract-details row in the seam's words.
pub(crate) fn details_row(row: &ContractDetails) -> DetailsRow {
    DetailsRow {
        conid: row.contract.contract_id,
        symbol: row.contract.symbol.0.clone(),
        sec_type: row.contract.security_type.to_string(),
        currency: row.contract.currency.0.clone(),
        multiplier: row.contract.multiplier.clone(),
        price_magnifier: row.price_magnifier,
        min_tick: row.min_tick,
        size_increment: row.size_increment,
        min_size: row.min_size,
        market_rule_ids: row.market_rule_ids.clone(),
    }
}

/// A market rule's bands in the seam's words.
pub(crate) fn price_bands(rule: &MarketRule) -> Vec<PriceBand> {
    rule.price_increments
        .iter()
        .map(|b| PriceBand { low_edge: b.low_edge, increment: b.increment })
        .collect()
}

/// How long a read base currency is kept before it is read again (on a connection the ask holds anyway). An
/// account's base currency is a setting of the account, not a market: asking again at every rate would spend a
/// request for the same word. The value read is REMEMBERED for the life of the source whatever its age
/// ([`IbkrFactsSource::remembered_base`]): a stock quoted in it needs no connection, and a read that comes back
/// different is refused (the engine's ledger was anchored to the first).
pub const BASE_KEEP: Duration = Duration::from_secs(600);

/// How long a read rate is reused for the same currency, by every ask and every stock quoted in it: one forex
/// read per currency, not per symbol. Shorter than the node's once-a-minute refresh of a held position, so each
/// refresh reads afresh, and well inside the engine's 120 s window.
pub const RATE_KEEP: Duration = Duration::from_secs(30);

/// How long one rate read may take end to end, connect included: the base currency (when not kept) and at
/// most two forex pairs, each bounded apart ([`crate::fx_socket::SUMMARY_DEADLINE`], [`crate::fx_socket::BOOK_DEADLINE`]).
pub const RATE_DEADLINE: Duration = Duration::from_secs(35);

/// How long a rate read that FAILED is remembered, per quote currency: the next ask for the same currency inside it
/// gets the same answer at once. A pair that says nothing costs a read [`crate::fx_socket::BOOK_DEADLINE`] (6 s), and
/// the six held stocks of one currency would otherwise cost the node's rate tick 36 s of them; the node itself waits
/// 10 s before it retries a failed rate (`RATE_RETRY_AFTER`), so a longer keep here would only hide a recovery.
pub const RATE_FAIL_KEEP: Duration = Duration::from_secs(10);

/// The rate reads that failed, by quote currency: when, and the words ([`RATE_FAIL_KEEP`]).
type KeptFailures = Mutex<HashMap<String, (Instant, String)>>;

/// The base currency read, and when ([`BASE_KEEP`]).
type KeptBase = Mutex<Option<(Instant, String)>>;

/// The rates read, by quote currency: when, the base currency each is expressed in, and the rate ([`RATE_KEEP`]).
type KeptRates = Mutex<HashMap<String, (Instant, String, QuoteRate)>>;

/// **One ask's use of the session's data connection** ([`IbkrFactsSource::scope`]): the connection is joined or
/// dialled by the ask's first read that needs one and every later read of the ask rides it, so the ask costs at
/// most one dial of the data lane's hourly budget however many things it reads. Dropping the scope gives the
/// connection back (the last hold closes it). Not shared between threads: one ask, one thread.
pub struct FactsScope<'a> {
    cfg: &'a IbkrConfig,
    hold: RefCell<Hold<Client>>,
}

impl<'a> FactsScope<'a> {
    fn new(cfg: &'a IbkrConfig) -> Self {
        Self { cfg, hold: RefCell::new(Hold::new(session_for(cfg))) }
    }

    /// Run `work` on the ask's connection, bounded by `within` END TO END: the wait for another asker's dial of
    /// the session, the gate's waits for the settling threads and the connect gap, the dial itself (when this read
    /// makes it) and the request all draw on the one bound ([`AskDeadline`]), so the caller's bound is the most the
    /// call takes, and a dial that would leave the read less than a quarter of it is not made (it would spend one of
    /// the lane's hourly dials for nothing). The connection is joined without a dial when the price stream (or an
    /// earlier read of this ask) holds it, else dialled once inside the gate's data turn. A cpapi account is
    /// REFUSED by name before any dial. A read that outlives its bound is abandoned and the connection is given
    /// back at once, so the abandoned request cannot keep it open under the next one.
    fn run<T: Send + 'static>(
        &self,
        what: &'static str,
        within: Duration,
        work: impl FnOnce(&Client) -> Result<T, FactsError> + Send + 'static,
    ) -> Result<T, FactsError> {
        refuse_cpapi(self.cfg, &format!("the {what}"))
            .map_err(|why| FactsError::Refused(why.to_string()))?;
        let ask = AskDeadline::within(within);
        let cfg = self.cfg;
        let client = self
            .hold
            .borrow_mut()
            .client(Some(ask.by()), || {
                dial_data_client_by(
                    cfg,
                    DataLane::Lookup,
                    what,
                    ask.left().min(OPEN_DEADLINE),
                    Some(ask),
                )
            })
            .map_err(|why| FactsError::Unavailable(why.to_string()))?;
        // The requests have no timeout of their own: they run on a thread this waits on for what is left of
        // `within`.
        let answer = bounded(what, ask.left(), None, move || Ok(work(&client)));
        match answer {
            Ok(done) => done,
            Err(why) => {
                self.hold.borrow_mut().release();
                Err(FactsError::Unavailable(why.to_string()))
            }
        }
    }
}

/// What a rate read found: the account's base currency and, for a stock quoted in another currency, the rate
/// that books it in the base ([`crate::fx_rate`]). `rate` is `None` exactly when the stock is quoted in the base.
#[derive(Debug, Clone, PartialEq)]
pub struct RateAnswer {
    /// The account's base currency (upper case), as its `NetLiquidation` summary row states it.
    pub base: String,
    /// The conversion, read just now.
    pub rate: Option<QuoteRate>,
}

/// What the gatewayhub asks for the facts of one stock of ONE account's gateway session.
pub struct IbkrFactsSource {
    cfg: IbkrConfig,
    /// The account's base currency, when read, and when ([`BASE_KEEP`]). Never cleared: the value is the
    /// source's memory of what the engine's ledger was anchored to ([`Self::remembered_base`]).
    base: Arc<KeptBase>,
    /// The rates read, by quote currency ([`RATE_KEEP`]).
    rates: Arc<KeptRates>,
    /// The rate reads that failed, by quote currency ([`RATE_FAIL_KEEP`]).
    failures: KeptFailures,
}

impl std::fmt::Debug for IbkrFactsSource {
    /// Names the gateway only: the config carries the IB account number, which a derived `Debug` would print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IbkrFactsSource(gateway={}:{})", self.cfg.host, self.cfg.port)
    }
}

impl IbkrFactsSource {
    /// A source over `cfg` (the account's resolved connection: gateway, ids, account).
    #[must_use]
    pub fn new(cfg: IbkrConfig) -> Self {
        Self { cfg, base: Arc::default(), rates: Arc::default(), failures: Mutex::default() }
    }

    /// **A new ask**: the reads made through the returned scope ([`Self::facts_any_currency_in`],
    /// [`Self::rate_for_in`]) share one connection and cost at most one dial. A scope takes nothing until a read
    /// needs the connection, so an ask answered from what is kept costs nothing.
    #[must_use]
    pub fn scope(&self) -> FactsScope<'_> {
        FactsScope::new(&self.cfg)
    }

    /// The account's base currency as this source last read it, whatever its age: what the engine's ledger was
    /// anchored to. `None` until a read succeeds. The gatewayhub falls back to it when a start cannot read the
    /// currency live, so a restart's engine is anchored to the currency it ran under.
    #[must_use]
    pub fn remembered_base(&self) -> Option<String> {
        known_base(&self.base)
    }

    /// Whether this source was built for `cfg`'s gateway, client ids and account, so the caller keeps it
    /// across asks and builds a new one when the account's rows change.
    #[must_use]
    pub fn serves(&self, cfg: &IbkrConfig) -> bool {
        let mine = &self.cfg;
        mine.backend == cfg.backend
            && mine.host == cfg.host
            && mine.port == cfg.port
            && mine.account == cfg.account
            && mine.client_id == cfg.client_id
            && mine.data_client_id == cfg.data_client_id
    }

    /// The facts of the stock `name` (`TICKER.SMART.CCY`) in an account whose money is `account_currency`.
    ///
    /// # Errors
    /// [`FactsError::Refused`] for a name or backend this lookup does not admit (decided before any dial),
    /// and every verdict of [`stock_facts`]; [`FactsError::Unavailable`] when the data lane of the gate
    /// refused the dial or the gateway did not answer within [`LOOKUP_DEADLINE`].
    pub fn facts(&self, name: &str, account_currency: &str) -> Result<StockFacts, FactsError> {
        self.facts_within(name, account_currency, LOOKUP_DEADLINE)
    }

    /// [`Self::facts`] under a `within` bound (a test shortens it).
    pub(crate) fn facts_within(
        &self,
        name: &str,
        account_currency: &str,
        within: Duration,
    ) -> Result<StockFacts, FactsError> {
        checked_name(name, account_currency)?;
        let (name, currency) = (name.to_string(), account_currency.to_string());
        self.scope().run("any-symbol lookup", within, move |client| {
            stock_facts(&SocketLookup { client }, &name, &currency)
        })
    }

    /// The facts of the stock `name` in ANY currency, for an account that converts a quote currency (decision
    /// 0183): [`Self::facts`] without the currency fence. Whether the name's currency is the ledger's own or needs
    /// a rate is [`Self::rate_for`]'s answer, read apart and fresh. A scope of its own; an ask that also reads the
    /// rate uses [`Self::facts_any_currency_in`] so both ride one connection.
    ///
    /// # Errors
    /// As [`Self::facts`], minus the currency fence.
    pub fn facts_any_currency(&self, name: &str) -> Result<StockFacts, FactsError> {
        self.facts_any_currency_in(&self.scope(), name)
    }

    /// [`Self::facts_any_currency`] on the connection of `scope`'s ask.
    ///
    /// # Errors
    /// As [`Self::facts_any_currency`].
    pub fn facts_any_currency_in(
        &self,
        scope: &FactsScope<'_>,
        name: &str,
    ) -> Result<StockFacts, FactsError> {
        self.facts_any_currency_within(scope, name, LOOKUP_DEADLINE)
    }

    /// [`Self::facts_any_currency_in`] under a `within` bound (a test shortens it).
    pub(crate) fn facts_any_currency_within(
        &self,
        scope: &FactsScope<'_>,
        name: &str,
        within: Duration,
    ) -> Result<StockFacts, FactsError> {
        parse_stock_name(name).map_err(FactsError::Refused)?;
        let name = name.to_string();
        scope.run("any-symbol lookup", within, move |client| {
            stock_facts_any_currency(&SocketLookup { client }, &name)
        })
    }

    /// **The account's base currency, and the rate that books a stock quoted in `quote_ccy` in it** ([`RateAnswer`]).
    /// The base is the currency of the account summary's `NetLiquidation` row (kept [`BASE_KEEP`], remembered for
    /// the life of the source); the rate is the mid of a live forex quote ([`crate::fx_rate`]) read on the
    /// session's data connection, reused for [`RATE_KEEP`] by every ask for the same currency. A stock quoted in
    /// the base currency already known, and a rate read less than [`RATE_KEEP`] ago, need no connection at all.
    /// A scope of its own; an ask that also reads the stock's facts uses [`Self::rate_for_in`].
    ///
    /// # Errors
    /// [`FactsError::Refused`] for a currency this adapter refuses by name (a minor-unit currency, decided before
    /// any dial), a currency with no forex pair against the base, a base the summary does not state singly and a
    /// base that differs from the one the source read before; [`FactsError::Unavailable`] when the data lane
    /// refused the dial, the gateway did not answer within [`RATE_DEADLINE`], or the quote is not a rate
    /// (delayed, one-sided, crossed, wide).
    pub fn rate_for(&self, quote_ccy: &str) -> Result<RateAnswer, FactsError> {
        self.rate_for_in(&self.scope(), quote_ccy)
    }

    /// [`Self::rate_for`] on the connection of `scope`'s ask.
    ///
    /// # Errors
    /// As [`Self::rate_for`].
    pub fn rate_for_in(
        &self,
        scope: &FactsScope<'_>,
        quote_ccy: &str,
    ) -> Result<RateAnswer, FactsError> {
        self.rate_for_within(scope, quote_ccy, RATE_DEADLINE)
    }

    /// [`Self::rate_for_in`] under a `within` bound (a test shortens it).
    pub(crate) fn rate_for_within(
        &self,
        scope: &FactsScope<'_>,
        quote_ccy: &str,
        within: Duration,
    ) -> Result<RateAnswer, FactsError> {
        let quote = quote_ccy.trim().to_ascii_uppercase();
        // Before any dial, and whether or not the quote is the base's own: a stock priced in a minor unit is
        // not admitted at all (`fx_rate`'s module doc).
        if let Some(why) = minor_unit_refusal(&quote) {
            return Err(FactsError::Refused(format!("ibkr: {why}")));
        }
        // What needs no connection: a stock quoted in the base currency (the account's own money, nothing to
        // convert) and a rate another ask read a moment ago.
        if let Some(base) = known_base(&self.base) {
            if base.eq_ignore_ascii_case(&quote) {
                return Ok(RateAnswer { base, rate: None });
            }
            if let Some(rate) = fresh_rate(&self.rates, &base, &quote, RATE_KEEP) {
                return Ok(RateAnswer { base, rate: Some(rate) });
            }
        }
        // A read of this currency failed a moment ago: the same answer, with no dial and no wait.
        if let Some(why) = recent_failure(&self.failures, &quote, RATE_FAIL_KEEP) {
            return Err(FactsError::Unavailable(why));
        }
        let (account, kept, rates, asked) = (
            self.cfg.account.clone(),
            Arc::clone(&self.base),
            Arc::clone(&self.rates),
            quote.clone(),
        );
        let got = scope.run("fx rate read", within, move |client| {
            let base = base_of(client, &account, &kept)?;
            if base.eq_ignore_ascii_case(&asked) {
                return Ok(RateAnswer { base, rate: None });
            }
            let rate = quote_rate(&SocketFx { client }, &base, &asked, vike_model::now_ms())?;
            // The sanity gate (vendor-doc V1): a read that moved by more than the engine's step since the one
            // this source accepted a moment ago is not a rate. The refused one is NOT kept as the reference.
            if let Some((at, was_base, previous)) = previous_rate(&rates, &asked)
                && was_base == base
            {
                check_against_previous(&previous, at.elapsed(), &rate)?;
            }
            keep_rate(&rates, &base, &rate);
            Ok(RateAnswer { base, rate: Some(rate) })
        });
        note_rate_outcome(&self.failures, &quote, &got);
        got
    }

    /// **The account's base currency**, read from the gateway unless a read less than [`BASE_KEEP`] old is kept:
    /// what the gatewayhub hands the node at an adapter's start, so the engine's ledger is anchored to a currency
    /// the VENUE stated and not to a default. A read at a start REPLACES a different value the source held (the
    /// new engine is anchored to what the venue says now, said at `warn`).
    ///
    /// # Errors
    /// As [`Self::rate_for`], for the base alone.
    pub fn account_base_currency(&self) -> Result<String, FactsError> {
        self.account_base_currency_in(&self.scope())
    }

    /// [`Self::account_base_currency`] on the connection of `scope`'s ask.
    ///
    /// # Errors
    /// As [`Self::account_base_currency`].
    pub fn account_base_currency_in(&self, scope: &FactsScope<'_>) -> Result<String, FactsError> {
        self.read_base(scope, RATE_DEADLINE)
    }

    /// [`Self::account_base_currency`] under a `within` bound, the dial and the read together (an adapter's
    /// start bounds it tightly: the start holds the account's adapter lock while it waits).
    ///
    /// # Errors
    /// As [`Self::account_base_currency`].
    pub fn account_base_currency_within(&self, within: Duration) -> Result<String, FactsError> {
        self.read_base(&self.scope(), within)
    }

    fn read_base(&self, scope: &FactsScope<'_>, within: Duration) -> Result<String, FactsError> {
        if let Some(base) = fresh_base(&self.base, BASE_KEEP) {
            return Ok(base);
        }
        let (account, kept) = (self.cfg.account.clone(), Arc::clone(&self.base));
        scope.run("account summary read", within, move |client| {
            keep_base(&kept, base_currency(client, &account)?, true)
        })
    }
}

/// The base currency in `kept` when it was read less than `keep` ago.
fn fresh_base(kept: &KeptBase, keep: Duration) -> Option<String> {
    let kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
    kept.as_ref().filter(|(at, _)| at.elapsed() < keep).map(|(_, base)| base.clone())
}

/// The base currency kept in `kept`, whatever its age.
fn known_base(kept: &KeptBase) -> Option<String> {
    kept.lock().unwrap_or_else(PoisonError::into_inner).as_ref().map(|(_, base)| base.clone())
}

/// Keep `read` as the account's base currency. A value that differs from the one already kept means the account's
/// currency changed under an engine whose ledger was anchored to the first: refused (every rate would be
/// expressed in a currency the engine does not count in), unless `restart` says this read IS a new anchor (an
/// adapter's start), which replaces the old value and says so.
fn keep_base(kept: &KeptBase, read: String, restart: bool) -> Result<String, FactsError> {
    // The summary's own rows are upper case already (`base_currency_of`); kept in that one spelling.
    let read = read.trim().to_ascii_uppercase();
    let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
    let was =
        kept.as_ref().map(|(_, was)| was.clone()).filter(|was| !was.eq_ignore_ascii_case(&read));
    if let Some(was) = was {
        if !restart {
            return Err(FactsError::Refused(format!(
                "ibkr: the account's base currency reads {read} where this gatewayhub read {was} before, and \
                 the engine's ledger is anchored to {was}: no rate is stated until the account is started \
                 again (set its `account.active` off and on)"
            )));
        }
        tracing::warn!(
            "ibkr: the account's base currency is {read} now, it was {was}: the account's new engine is \
             anchored to {read}"
        );
    }
    *kept = Some((Instant::now(), read.clone()));
    Ok(read)
}

/// The base currency kept in `kept` while fresh, else read off `client`'s account summary and kept (a read that
/// disagrees with the kept value is refused: [`keep_base`]).
fn base_of(client: &Client, account: &str, kept: &KeptBase) -> Result<String, FactsError> {
    if let Some(base) = fresh_base(kept, BASE_KEEP) {
        return Ok(base);
    }
    keep_base(kept, base_currency(client, account)?, false)
}

/// The rate for `quote` expressed in `base` that was read less than `keep` ago.
fn fresh_rate(kept: &KeptRates, base: &str, quote: &str, keep: Duration) -> Option<QuoteRate> {
    let kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
    kept.get(quote)
        .filter(|(at, was_base, _)| at.elapsed() < keep && was_base == base)
        .map(|(_, _, rate)| rate.clone())
}

/// The rate this source last accepted for `quote`, whatever its age: when, the base it is expressed in, and the
/// rate (what the sanity gate compares a fresh read with, [`check_against_previous`]).
fn previous_rate(kept: &KeptRates, quote: &str) -> Option<(Instant, String, QuoteRate)> {
    kept.lock().unwrap_or_else(PoisonError::into_inner).get(quote).cloned()
}

/// The words of a failed read of `quote` less than `keep` ago, for the ask that would repeat it.
fn recent_failure(kept: &KeptFailures, quote: &str, keep: Duration) -> Option<String> {
    let kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
    kept.get(quote).filter(|(at, _)| at.elapsed() < keep).map(|(at, why)| {
        format!(
            "{why} (the same read failed {} s ago and is not asked again for {} s)",
            at.elapsed().as_secs(),
            keep.as_secs()
        )
    })
}

/// Remember a read of `quote` that was UNAVAILABLE (a refusal is a verdict, cheap, and not repeated by anyone),
/// and forget the failure of one that answered.
fn note_rate_outcome(kept: &KeptFailures, quote: &str, got: &Result<RateAnswer, FactsError>) {
    let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
    match got {
        Ok(_) => {
            kept.remove(quote);
        }
        Err(FactsError::Unavailable(why)) => {
            kept.insert(quote.to_string(), (Instant::now(), why.clone()));
        }
        Err(FactsError::Refused(_)) => {}
    }
}

/// Keep `rate` (expressed in `base`) for the next ask for its currency.
fn keep_rate(kept: &KeptRates, base: &str, rate: &QuoteRate) {
    kept.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(rate.quote_ccy.clone(), (Instant::now(), base.to_string(), rate.clone()));
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ibapi::contracts::PriceIncrement;
    use vike_venues::IbkrBackend;

    use super::*;
    use crate::transport::socket::socket_tests::{QUICK, TEST_BOUND, silent_gateway};

    fn description(conid: i32, symbol: &str, kind: SecurityType, ccy: &str) -> ContractDescription {
        ContractDescription {
            contract: Contract {
                contract_id: conid,
                symbol: Symbol::from(symbol),
                security_type: kind,
                primary_exchange: Exchange::from("NASDAQ"),
                currency: Currency::from(ccy),
                ..Contract::default()
            },
            derivative_security_types: vec!["OPT".into()],
        }
    }

    /// The ibapi rows reach the seam in IB's own words: the secType as its wire code, the exchange and
    /// currency as text, the optional size rules kept optional, the rule ids kept as sent.
    #[test]
    fn ibapi_rows_map_into_the_seams_words() {
        let row = match_row(&description(265_598, "AAPL", SecurityType::Stock, "USD"));
        assert_eq!(
            row,
            MatchRow {
                conid: 265_598,
                symbol: "AAPL".into(),
                sec_type: "STK".into(),
                primary_exchange: "NASDAQ".into(),
                currency: "USD".into(),
            }
        );
        assert_eq!(match_row(&description(1, "ES", SecurityType::Future, "USD")).sec_type, "FUT");

        let details = ContractDetails {
            contract: Contract {
                contract_id: 265_598,
                symbol: Symbol::from("AAPL"),
                security_type: SecurityType::Stock,
                currency: Currency::from("USD"),
                multiplier: String::new(),
                ..Contract::default()
            },
            price_magnifier: 100,
            min_tick: 0.01,
            min_size: None,
            size_increment: Some(1.0),
            market_rule_ids: vec!["26".into(), "26".into()],
            ..ContractDetails::default()
        };
        let row = details_row(&details);
        assert_eq!(
            (row.conid, row.sec_type.as_str(), row.currency.as_str()),
            (265_598, "STK", "USD")
        );
        assert_eq!((row.min_tick, row.min_size, row.size_increment), (0.01, None, Some(1.0)));
        assert_eq!(row.price_magnifier, 100, "the gateway's own word reaches the check");
        assert_eq!(row.market_rule_ids, ["26", "26"]);

        let rule = MarketRule {
            market_rule_id: 26,
            price_increments: vec![
                PriceIncrement { low_edge: 0.0, increment: 0.0001 },
                PriceIncrement { low_edge: 1.0, increment: 0.01 },
            ],
        };
        assert_eq!(
            price_bands(&rule),
            [
                PriceBand { low_edge: 0.0, increment: 0.0001 },
                PriceBand { low_edge: 1.0, increment: 0.01 }
            ]
        );
    }

    /// A name the grammar or the currency fence refuses never dials: the gateway here is a closed port.
    #[test]
    fn a_refused_name_dials_nothing() {
        let (_gateway, cfg) = silent_gateway();
        let source = IbkrFactsSource::new(cfg);
        let began = Instant::now();
        for name in ["AAPL", "EUR.IDEALPRO.USD", "VOD.SMART.GBP"] {
            let got = source.facts_within(name, "USD", TEST_BOUND);
            assert!(matches!(got, Err(FactsError::Refused(_))), "{name}: {got:?}");
        }
        assert!(began.elapsed() < QUICK, "refused without dialling: {:?}", began.elapsed());
    }

    /// A cpapi account is refused by name and nothing is dialled (a cpapi box opens no ibapi socket).
    #[test]
    fn a_cpapi_account_is_refused_by_name() {
        let (_gateway, mut cfg) = silent_gateway();
        cfg.backend = IbkrBackend::Cpapi;
        let got = IbkrFactsSource::new(cfg).facts_within("AAPL.SMART.USD", "USD", TEST_BOUND);
        let Err(FactsError::Refused(why)) = got else { panic!("expected a refusal, got {got:?}") };
        assert!(why.contains("cpapi"), "{why}");
    }

    /// A gateway that takes the connection and never answers ends in "unavailable" within the bound.
    #[test]
    fn a_silent_gateway_is_unavailable_within_the_bound() {
        let (_gateway, cfg) = silent_gateway();
        let began = Instant::now();
        let got = IbkrFactsSource::new(cfg).facts_within("AAPL.SMART.USD", "USD", TEST_BOUND);
        assert!(matches!(got, Err(FactsError::Unavailable(_))), "{got:?}");
        assert!(began.elapsed() < QUICK, "waited {:?}", began.elapsed());
    }

    /// A printed source names the gateway and not the IB account number its config carries.
    #[test]
    fn a_printed_source_does_not_name_the_account() {
        let (_gateway, mut cfg) = silent_gateway();
        cfg.account = "DU1234567".into();
        let (host, port) = (cfg.host.clone(), cfg.port);
        let printed =
            format!("{:?} {:#?}", IbkrFactsSource::new(cfg.clone()), IbkrFactsSource::new(cfg));
        assert!(!printed.contains("DU1234567"), "{printed}");
        assert!(
            printed.contains(&format!("{host}:{port}")),
            "it still names the gateway: {printed}"
        );
    }

    /// A minor-unit currency (GBP, ILS, ZAR) is refused by name before any dial, whether or not it is the base's
    /// own; a bad name is refused before any dial too. The gateway here is a closed port.
    #[test]
    fn a_rate_read_for_a_minor_unit_currency_or_a_bad_name_dials_nothing() {
        let (_gateway, cfg) = silent_gateway();
        let source = IbkrFactsSource::new(cfg);
        let began = Instant::now();
        for ccy in ["GBP", " gbp ", "ILS", "ZAR"] {
            let got = source.rate_for_within(&source.scope(), ccy, TEST_BOUND);
            let Err(FactsError::Refused(why)) = got else { panic!("{ccy}: {got:?}") };
            assert!(why.contains("100 times too high"), "{ccy}: {why}");
        }
        let got = source.facts_any_currency_within(&source.scope(), "EUR.IDEALPRO.USD", TEST_BOUND);
        let Err(FactsError::Refused(why)) = got else { panic!("{got:?}") };
        assert!(why.contains("CASH position"), "{why}");
        assert!(began.elapsed() < QUICK, "refused without dialling: {:?}", began.elapsed());
    }

    /// A cpapi account is refused by name on every new read, and nothing is dialled.
    #[test]
    fn a_cpapi_account_is_refused_by_name_on_the_rate_reads_too() {
        let (_gateway, mut cfg) = silent_gateway();
        cfg.backend = IbkrBackend::Cpapi;
        let source = IbkrFactsSource::new(cfg);
        for got in [
            source.rate_for_within(&source.scope(), "EUR", TEST_BOUND).map(|_| ()),
            source.account_base_currency_within(TEST_BOUND).map(|_| ()),
            source
                .facts_any_currency_within(&source.scope(), "SAP.SMART.EUR", TEST_BOUND)
                .map(|_| ()),
        ] {
            let Err(FactsError::Refused(why)) = got else {
                panic!("expected a refusal, got {got:?}")
            };
            assert!(why.contains("cpapi"), "{why}");
        }
    }

    /// A gateway that takes the connection and never answers ends every new read in "unavailable" within the
    /// bound: never a base currency, never a rate.
    #[test]
    fn a_silent_gateway_gives_no_base_currency_and_no_rate() {
        let (_gateway, cfg) = silent_gateway();
        let source = IbkrFactsSource::new(cfg);
        let began = Instant::now();
        let got = source.rate_for_within(&source.scope(), "EUR", TEST_BOUND);
        assert!(matches!(got, Err(FactsError::Unavailable(_))), "{got:?}");
        let got = source.account_base_currency_within(TEST_BOUND);
        assert!(matches!(got, Err(FactsError::Unavailable(_))), "{got:?}");
        assert!(source.base.lock().unwrap().is_none(), "nothing was kept");
        assert_eq!(source.remembered_base(), None, "nothing is remembered either");
        assert!(began.elapsed() < QUICK * 2, "waited {:?}", began.elapsed());
    }

    /// The kept base currency answers while it is fresh and not once its keep has run out; nothing kept is
    /// nothing answered. What the source REMEMBERS has no age: a stock quoted in it needs no connection.
    #[test]
    fn a_kept_base_currency_is_used_while_fresh_and_a_stale_one_is_not() {
        let kept = Mutex::new(None);
        assert_eq!(fresh_base(&kept, BASE_KEEP), None);
        assert_eq!(known_base(&kept), None);
        *kept.lock().unwrap() = Some((Instant::now(), "EUR".to_string()));
        assert_eq!(fresh_base(&kept, BASE_KEEP).as_deref(), Some("EUR"));
        assert_eq!(fresh_base(&kept, Duration::ZERO), None, "a keep of no time keeps nothing");
        assert_eq!(known_base(&kept).as_deref(), Some("EUR"), "but it is still the one remembered");
    }

    /// A read that disagrees with the base currency the source kept is REFUSED (the engine's ledger is anchored
    /// to the first and every rate would be in a currency it does not count in); a start's read is the new anchor
    /// and replaces it; the same value just refreshes the keep.
    #[test]
    fn a_base_currency_that_changed_is_refused_unless_it_is_a_start() {
        let kept = Mutex::new(None);
        assert_eq!(
            keep_base(&kept, "USD".into(), false).as_deref(),
            Ok("USD"),
            "the first read stands"
        );
        assert_eq!(keep_base(&kept, " usd ".into(), false).as_deref(), Ok("USD"), "the same word");
        let Err(FactsError::Refused(why)) = keep_base(&kept, "EUR".into(), false) else {
            panic!("a different base is refused")
        };
        assert!(
            why.contains("EUR") && why.contains("USD") && why.contains("started again"),
            "{why}"
        );
        assert_eq!(known_base(&kept).as_deref(), Some("USD"), "the refused read kept nothing");
        assert_eq!(
            keep_base(&kept, "EUR".into(), true).as_deref(),
            Ok("EUR"),
            "a start re-anchors"
        );
        assert_eq!(known_base(&kept).as_deref(), Some("EUR"));
    }

    /// The data dials a gateway's gate has admitted: what an ask spent of the lane's hourly budget.
    fn dials_spent(cfg: &IbkrConfig) -> usize {
        crate::transport::gateway_gate::gate_for(&format!("{}:{}", cfg.host, cfg.port))
            .data_dials_spent()
    }

    fn eur_rate() -> QuoteRate {
        QuoteRate {
            quote_ccy: "EUR".into(),
            account_per_quote: crate::fx_rate::AccountPerQuote::new(1.10).unwrap(),
            as_of_ms: 1_700_000_000_000,
            pair: "EUR.USD".into(),
        }
    }

    /// **A stock quoted in the base currency the source already knows costs NO dial** (it needs no rate, the
    /// answer is "your own money"): before this a USD stock on a USD account dialled the gateway to be told so.
    /// The gateway here is a silent port, so a dial would be counted (and would time out).
    #[test]
    fn a_stock_in_the_known_base_currency_spends_no_dial() {
        let (_gateway, cfg) = silent_gateway();
        let source = IbkrFactsSource::new(cfg.clone());
        *source.base.lock().unwrap() = Some((Instant::now(), "USD".to_string()));
        let began = Instant::now();
        let got = source.rate_for_within(&source.scope(), " usd ", TEST_BOUND);
        assert_eq!(got, Ok(RateAnswer { base: "USD".into(), rate: None }));
        assert_eq!(dials_spent(&cfg), 0);
        assert!(began.elapsed() < QUICK, "answered from what is kept: {:?}", began.elapsed());
    }

    /// **A rate read a moment ago is served to the next ask for the same currency, with no dial**: one forex
    /// read per currency, not per symbol. An older read, or one expressed in another base, is not served.
    #[test]
    fn a_rate_read_inside_its_keep_is_reused_for_the_currency_and_costs_no_dial() {
        let (_gateway, cfg) = silent_gateway();
        let source = IbkrFactsSource::new(cfg.clone());
        *source.base.lock().unwrap() = Some((Instant::now(), "USD".to_string()));
        keep_rate(&source.rates, "USD", &eur_rate());
        let got = source.rate_for_within(&source.scope(), "EUR", TEST_BOUND).expect("served");
        assert_eq!(got, RateAnswer { base: "USD".into(), rate: Some(eur_rate()) });
        assert_eq!(dials_spent(&cfg), 0);

        assert_eq!(fresh_rate(&source.rates, "USD", "EUR", RATE_KEEP), Some(eur_rate()));
        assert_eq!(fresh_rate(&source.rates, "USD", "EUR", Duration::ZERO), None, "aged out");
        assert_eq!(fresh_rate(&source.rates, "GBP", "EUR", RATE_KEEP), None, "another base's rate");
        assert_eq!(fresh_rate(&source.rates, "USD", "JPY", RATE_KEEP), None, "another currency");
    }

    /// **An account that never converts costs what it cost before**: the plain lookup (the path of an account with
    /// no exposure ceiling) makes one dial, and nothing here reads the account's currency. The several reads of a
    /// converting ask spend ONE dial between them: once a silent gateway has taken it (and backed the lane off),
    /// the rest of the ask spends nothing more.
    #[test]
    fn the_plain_lookup_costs_one_dial_and_a_converting_ask_costs_one_between_its_reads() {
        let (_gateway, cfg) = silent_gateway();
        let plain = IbkrFactsSource::new(cfg.clone());
        let _ = plain.facts_within("AAPL.SMART.USD", "USD", TEST_BOUND);
        assert_eq!(dials_spent(&cfg), 1, "the old path: one dial");
        assert_eq!(plain.remembered_base(), None, "and the account's currency was never read");

        let (_gateway, cfg) = silent_gateway();
        let source = IbkrFactsSource::new(cfg.clone());
        let ask = source.scope();
        for _ in 0..2 {
            let _ = source.facts_any_currency_within(&ask, "SAP.SMART.EUR", TEST_BOUND);
            let _ = source.rate_for_within(&ask, "EUR", TEST_BOUND);
        }
        assert_eq!(dials_spent(&cfg), 1, "four reads, one dial");
    }

    /// **A rate read that failed is not repeated for a few seconds** (finding 2 of the second IBKR currency review):
    /// per currency, only an unavailable read (not a refusal), forgotten when the currency answers, aged out
    /// after [`RATE_FAIL_KEEP`].
    #[test]
    fn a_failed_rate_read_is_remembered_per_currency_for_a_few_seconds() {
        let kept = KeptFailures::default();
        assert_eq!(recent_failure(&kept, "EUR", RATE_FAIL_KEEP), None);
        let unavailable = Err(FactsError::Unavailable("no two-sided quote arrived".into()));
        note_rate_outcome(&kept, "EUR", &unavailable);
        let why = recent_failure(&kept, "EUR", RATE_FAIL_KEEP).expect("remembered");
        assert!(
            why.contains("no two-sided quote arrived") && why.contains("not asked again"),
            "{why}"
        );
        assert_eq!(recent_failure(&kept, "JPY", RATE_FAIL_KEEP), None, "another currency asks");
        assert_eq!(recent_failure(&kept, "EUR", Duration::ZERO), None, "aged out");

        note_rate_outcome(&kept, "JPY", &Err(FactsError::Refused("no pair".into())));
        assert_eq!(recent_failure(&kept, "JPY", RATE_FAIL_KEEP), None, "a refusal is not kept");
        note_rate_outcome(&kept, "EUR", &Ok(RateAnswer { base: "USD".into(), rate: None }));
        assert_eq!(recent_failure(&kept, "EUR", RATE_FAIL_KEEP), None, "an answer clears it");
    }

    /// The second ask for a currency whose read just failed answers at once, dials nothing and waits for nothing:
    /// the gateway here is silent, so any dial would be counted and would take the bound.
    #[test]
    fn a_currency_whose_read_just_failed_is_answered_without_a_dial() {
        let (_gateway, cfg) = silent_gateway();
        let source = IbkrFactsSource::new(cfg.clone());
        *source.base.lock().unwrap() = Some((Instant::now(), "USD".to_string()));
        note_rate_outcome(
            &source.failures,
            "EUR",
            &Err(FactsError::Unavailable("EUR.USD: no two-sided quote".into())),
        );
        let began = Instant::now();
        let got = source.rate_for_within(&source.scope(), "EUR", TEST_BOUND);
        let Err(FactsError::Unavailable(why)) = got else { panic!("{got:?}") };
        assert!(
            why.contains("EUR.USD: no two-sided quote") && why.contains("not asked again"),
            "{why}"
        );
        assert_eq!(dials_spent(&cfg), 0);
        assert!(began.elapsed() < Duration::from_millis(100), "{:?}", began.elapsed());
    }

    /// **An ask's bound is TOTAL** (finding 4 of the second IBKR currency review): while another ask's dial holds
    /// the session (a silent gateway takes the dial's whole bound), a second ask waits for it only as long as ITS
    /// OWN bound allows, then answers "unavailable" with nothing dialled and none of the lane's hourly dials
    /// spent. Before, it queued on the session's lock for as long as that dial took and only then began its own
    /// bound, which an adapter's start (it bounds the base-currency read at 8 s with its lock held) paid for.
    #[test]
    fn an_ask_does_not_queue_behind_another_asks_dial_past_its_own_bound() {
        let (_gateway, cfg) = silent_gateway();
        let first = Arc::new(IbkrFactsSource::new(cfg.clone()));
        let holder = {
            let first = Arc::clone(&first);
            std::thread::spawn(move || {
                first.account_base_currency_within(Duration::from_millis(1_500))
            })
        };
        let began = Instant::now();
        while dials_spent(&cfg) < 1 {
            assert!(began.elapsed() < Duration::from_secs(10), "the first ask never dialled");
            std::thread::sleep(Duration::from_millis(5));
        }
        let second = IbkrFactsSource::new(cfg.clone());
        let asked = Instant::now();
        let got = second.account_base_currency_within(TEST_BOUND);
        assert!(matches!(got, Err(FactsError::Unavailable(_))), "{got:?}");
        assert!(
            asked.elapsed() < QUICK,
            "the second ask ended at its own bound, not behind the first's dial: {:?}",
            asked.elapsed()
        );
        assert_eq!(dials_spent(&cfg), 1, "the second ask dialled nothing");
        let _ = holder.join();
    }

    /// The source is kept while the account's connection is the same and replaced when it changes.
    #[test]
    fn a_source_serves_its_own_connection_only() {
        let (_gateway, cfg) = silent_gateway();
        let source = IbkrFactsSource::new(cfg.clone());
        assert!(source.serves(&cfg));
        for change in [
            |c: &mut IbkrConfig| c.port += 1,
            |c: &mut IbkrConfig| c.host = "<host>".into(),
            |c: &mut IbkrConfig| c.account = "DU1".into(),
            |c: &mut IbkrConfig| c.data_client_id += 1,
            |c: &mut IbkrConfig| c.client_id += 5,
        ] {
            let mut other = cfg.clone();
            change(&mut other);
            assert!(!source.serves(&other));
        }
    }
}
