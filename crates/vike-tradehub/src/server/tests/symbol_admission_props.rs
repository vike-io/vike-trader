//! **The control door's admission planner under random input**: the property twin of
//! `admits_any_symbol.rs`'s hand-picked rows. The subject is
//! `crates/vike-tradehub/src/server/symbol_admission.rs`'s `plan_submit_symbol` (and `SYMBOL_RULES`,
//! `canonical_symbol`), a pure function of the command, the published engine blocks and the facts
//! source's answers, so every law below is stated over those three and nothing else.
//!
//! | law | what would be true without it |
//! |---|---|
//! | [`a_symbol_the_engine_trades_is_never_readmitted_or_looked_up`] | a second `AdmitSymbol`, or a facts lookup the venue budgets, for a symbol already in the engine |
//! | [`another_spelling_of_a_traded_symbol_is_never_admitted_as_a_second_instrument`] | one venue position booked twice under two spellings |
//! | [`an_opening_order_with_no_price_handle_is_refused_before_any_lookup`] | ibkr's budgeted data connect spent on an order no price can back (the v0.1.72 ordering) |
//! | [`a_reduce_only_order_is_never_refused_for_a_missing_price_feed`] | an exit held up or refused for want of a price, or an exit burning an admission slot |
//! | [`the_per_order_venues_follow_the_written_table`] | polymarket/fxcm judged by a rule nobody wrote |
//! | [`the_plan_depends_only_on_the_command_the_addressed_engine_and_the_facts`] | a verdict that moves with another account's block, or between two identical asks |
//! | [`the_door_plans_against_the_newest_block_of_a_restarted_account`] | the stopped engine's block judging the order of the engine that runs (the v0.1.72 door fix) |
//! | [`a_canonical_symbol_is_idempotent_ascii_and_inside_the_venues_alphabet`] | a character no instrument has reaching a signed query string |
//!
//! ⚠ **FAILURE POLICY: a counterexample here is a REAL door finding.** Stop and report the minimal
//! case proptest prints; do not bend the door to the property or the property to the door. This is a
//! unit-test child (not a `tests/` binary) because the planner is crate-private, like its neighbours;
//! the default `proptest-regressions` placement for a library test applies.

use std::sync::{Arc, Mutex};

use proptest::prelude::*;
use vike_exec::{CoreSnapshot, MAX_EXTRA_SYMBOLS, SymbolAdmission, VenueBlock};
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
use vike_model::{AccountTier, SymbolGrid};
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};

use crate::server::symbol_admission::{
    SYMBOL_RULES, Spelling, SymbolFacts, SymbolPlan, SymbolRule, admitting_venues,
    canonical_symbol, plan_submit_symbol,
};

// ---------------------------------------------------------------------------------------------
// The venues under test: one row per way an `Admits` venue spells its symbols.
// ---------------------------------------------------------------------------------------------

/// One engine on an `Admits` venue and the symbols the laws ask about.
#[derive(Clone, Copy, Debug)]
struct Sample {
    venue: &'static str,
    /// The symbol the engine is mounted on.
    mounted: &'static str,
    /// Another symbol of the same product that the venue lists and the door can admit.
    foreign: &'static str,
    /// Another spelling of `mounted`'s INSTRUMENT, besides its case: the door either places the order on
    /// `mounted` or refuses it as an alias, whichever the venue's rule is.
    alias: Option<&'static str>,
    /// Whether the door folds case for this venue. hyperliquid's coin names and ctrader's broker names
    /// are case-sensitive, so a case variant is ANOTHER name there (the venue's list answers it).
    folds_case: bool,
    /// Whether `mounted` is already in the door's canonical spelling, so that every case variant of
    /// it canonicalises back onto it (ibkr's `AAPL.NASDAQ.USD` is not: the door refuses a named exchange).
    canonical: bool,
}

const fn sample(
    venue: &'static str,
    mounted: &'static str,
    foreign: &'static str,
    alias: Option<&'static str>,
) -> Sample {
    Sample { venue, mounted, foreign, alias, folds_case: true, canonical: true }
}

const SAMPLES: &[Sample] = &[
    sample("binance", "BTCUSDT.P", "ETHUSDT.P", None),
    sample("binance", "BTCUSDT", "ETHUSDT", None),
    sample("aster", "BTCUSDT.P", "ETHUSDT.P", None),
    sample("bybit", "BTCUSDT", "ETHUSDT", Some("BTCUSDT.P")),
    sample("okx", "BTC-USDT-SWAP", "ETH-USDT-SWAP", None),
    sample("deribit", "BTC-PERPETUAL", "ETH-PERPETUAL", None),
    Sample { folds_case: false, ..sample("hyperliquid", "BTC", "ETH", None) },
    sample("alpaca", "AAPL", "MSFT", None),
    sample("alpaca", "BTC/USD", "MSFT", Some("BTCUSD")),
    Sample { folds_case: false, ..sample("ctrader", "EURUSD", "GBPUSD", None) },
    sample("oanda", "EURUSD", "GBPUSD", Some("EUR_USD")),
    sample("ig", "CS.D.EURUSD.MINI.IP", "CS.D.GBPUSD.MINI.IP", None),
    sample("ibkr", "AAPL.SMART.USD", "MSFT.SMART.USD", Some("AAPL")),
    sample("dukascopy", "EURUSD", "GBPUSD", Some("EUR/USD")),
    Sample {
        canonical: false,
        ..sample("ibkr", "AAPL.NASDAQ.USD", "MSFT.SMART.USD", Some("AAPL"))
    },
];

/// Every `Spelling` the table can name. The two exhaustive matches below fail to COMPILE when a
/// variant is added, so this list cannot silently fall behind.
const SPELLINGS: [Spelling; 11] = [
    Spelling::LaneSuffixed,
    Spelling::BareLinear,
    Spelling::Hyphenated,
    Spelling::CaseKept,
    Spelling::PerpetualName,
    Spelling::AssetSymbol,
    Spelling::BrokerName,
    Spelling::OandaInstrument,
    Spelling::Epic,
    Spelling::StockName,
    Spelling::PairName,
];

/// The characters, besides ASCII letters and digits, a canonical symbol of this spelling may hold.
fn extra_alphabet(spelling: Spelling) -> &'static str {
    match spelling {
        Spelling::LaneSuffixed | Spelling::StockName => ".",
        Spelling::BareLinear | Spelling::CaseKept => "",
        Spelling::Hyphenated => "-",
        Spelling::PerpetualName => "-_",
        Spelling::AssetSymbol => "./",
        Spelling::BrokerName | Spelling::OandaInstrument => "_",
        Spelling::Epic => "._-",
        Spelling::PairName => "./",
    }
}

/// Whether the door upper-cases this spelling (everything but hyperliquid's and ctrader's).
fn folds_case(spelling: Spelling) -> bool {
    match spelling {
        Spelling::LaneSuffixed
        | Spelling::BareLinear
        | Spelling::Hyphenated
        | Spelling::PerpetualName
        | Spelling::AssetSymbol
        | Spelling::OandaInstrument
        | Spelling::Epic
        | Spelling::StockName
        | Spelling::PairName => true,
        Spelling::CaseKept | Spelling::BrokerName => false,
    }
}

fn spelling_of(venue: &str) -> Spelling {
    SYMBOL_RULES
        .iter()
        .find_map(|(v, rule)| match rule {
            SymbolRule::Admits(spelling) if *v == venue => Some(*spelling),
            _ => None,
        })
        .expect("an Admits venue of the table")
}

/// The generator must reach every venue the door admits for, and only those: a venue added to
/// `SYMBOL_RULES` fails here until a row of [`SAMPLES`] exercises it (a law over a venue the generator
/// never draws is vacuously green).
#[test]
fn the_samples_cover_every_admitting_venue_and_name_sound_symbols() {
    let mut sampled: Vec<&str> = SAMPLES.iter().map(|s| s.venue).collect();
    sampled.sort_unstable();
    sampled.dedup();
    let mut admitting = admitting_venues();
    admitting.sort_unstable();
    assert_eq!(sampled, admitting, "SAMPLES must cover exactly the Admits rows");
    for (_, rule) in SYMBOL_RULES {
        if let SymbolRule::Admits(spelling) = rule {
            assert!(SPELLINGS.contains(spelling), "{spelling:?} is missing from SPELLINGS");
        }
    }
    for s in SAMPLES {
        let spelling = spelling_of(s.venue);
        assert_eq!(folds_case(spelling), s.folds_case, "{}: folds_case disagrees", s.venue);
        // The premises the laws lean on, checked once against the door's own spelling.
        assert_eq!(canonical_symbol(spelling, s.foreign).as_deref(), Ok(s.foreign), "{s:?}");
        assert_eq!(
            canonical_symbol(spelling, s.mounted).as_deref() == Ok(s.mounted),
            s.canonical,
            "{s:?}: `canonical` must say whether the mounted symbol is canonical"
        );
        if let Some(alias) = s.alias {
            assert!(canonical_symbol(spelling, alias).is_ok(), "{s:?}: the alias must be writable");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The facts source: a deterministic model that RECORDS what the planner asked of it.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum PriceAnswer {
    Ready,
    Pending,
    Failing,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum RefreshAnswer {
    NotDue,
    Due,
    ForAnotherSymbol,
    ForAnotherAccount,
}

/// Everything the planner can learn from the source; the same answers always give the same replies.
#[derive(Clone, Copy, Debug)]
struct Answers {
    /// `price_source` refuses: this node holds no price watcher for the venue.
    unpriced: bool,
    /// `admission` fails: the venue lists no such instrument.
    lookup_fails: bool,
    price: PriceAnswer,
    refresh: RefreshAnswer,
    /// The account runs with an account-wide exposure ceiling.
    capped: bool,
}

/// The words the model refuses with, so a refusal's text can be traced to the answer that caused it.
const NO_FEED: &str = "SENTINEL-no-price-watcher";
const PRICE_DOWN: &str = "SENTINEL-price-lane-down";

#[derive(Clone, Debug, PartialEq)]
enum Call {
    PriceSource,
    Admission(String),
    Price { symbol: String, subscribe: bool },
    Refresh(String),
    ExposureCap,
}

struct Model {
    answers: Answers,
    calls: Mutex<Vec<Call>>,
}

fn admission_of(route_key: &str, symbol: &str) -> SymbolAdmission {
    SymbolAdmission {
        route_key: route_key.into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.001), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: None,
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
    }
}

impl Model {
    fn new(answers: &Answers) -> Self {
        Model { answers: *answers, calls: Mutex::default() }
    }
    fn note(&self, call: Call) {
        self.calls.lock().expect("calls lock").push(call);
    }
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("calls lock").clone()
    }
    /// What the planner asked that costs the venue or the node something: everything but the
    /// exposure-ceiling read, which is the settings'.
    fn lookups(&self) -> Vec<Call> {
        self.calls().into_iter().filter(|c| *c != Call::ExposureCap).collect()
    }
}

impl SymbolFacts for Model {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn price_source(&self, _: &str) -> Result<(), String> {
        self.note(Call::PriceSource);
        if self.answers.unpriced { Err(NO_FEED.to_string()) } else { Ok(()) }
    }
    fn admission(&self, route_key: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
        self.note(Call::Admission(symbol.to_string()));
        if self.answers.lookup_fails {
            Err("the venue lists no such instrument".to_string())
        } else {
            Ok(admission_of(route_key, symbol))
        }
    }
    fn price(
        &self,
        _: &str,
        _: &str,
        symbol: &str,
        subscribe: bool,
    ) -> Result<Option<f64>, String> {
        self.note(Call::Price { symbol: symbol.to_string(), subscribe });
        match self.answers.price {
            PriceAnswer::Ready => Ok(Some(100.0)),
            PriceAnswer::Pending => Ok(None),
            PriceAnswer::Failing => Err(PRICE_DOWN.to_string()),
        }
    }
    fn refresh(&self, route_key: &str, _: &str, symbol: &str) -> Option<SymbolAdmission> {
        self.note(Call::Refresh(symbol.to_string()));
        match self.answers.refresh {
            RefreshAnswer::NotDue => None,
            RefreshAnswer::Due => Some(admission_of(route_key, symbol)),
            RefreshAnswer::ForAnotherSymbol => Some(admission_of(route_key, "ELSEWHERE")),
            RefreshAnswer::ForAnotherAccount => Some(admission_of("elsewhere#ALT", symbol)),
        }
    }
    fn exposure_cap_armed(&self, _: &str) -> bool {
        self.note(Call::ExposureCap);
        self.answers.capped
    }
}

// ---------------------------------------------------------------------------------------------
// Commands, blocks and the generator.
// ---------------------------------------------------------------------------------------------

/// Which account of the venue the order names.
#[derive(Clone, Copy, Debug)]
enum Label {
    /// The sender named none.
    Absent,
    /// `DEFAULT`: the unlabelled account again.
    Default,
    Alt,
}

impl Label {
    fn wire(self) -> Option<&'static str> {
        match self {
            Label::Absent => None,
            Label::Default => Some("DEFAULT"),
            Label::Alt => Some("ALT"),
        }
    }
    /// The route key of that account's engine, composed by the same function the door uses.
    fn key(self, venue: &str) -> String {
        let label = match self {
            Label::Alt => AccountLabel::parse("ALT").expect("a legal label"),
            Label::Absent | Label::Default => AccountLabel::Default,
        };
        route_key_of(venue, &label)
    }
}

fn submit(
    venue: &str,
    symbol: &str,
    label: Label,
    reduce_only: bool,
    price: Option<f64>,
) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: "c-1".into(),
        venue: venue.into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: if price.is_some() { "limit" } else { "market" }.into(),
        price,
        trigger_price: None,
        reduce_only,
        account: label.wire().map(str::to_string),
    })
}

/// A published engine block, as `PublisherHandle::engine_blocks` hands it to the door.
fn block(key: &str, mounted: &str, extras: &[&str], tier: Option<AccountTier>) -> VenueBlock {
    VenueBlock {
        venue: key.split('#').next().unwrap_or(key).into(),
        route_key: key.into(),
        symbol: mounted.into(),
        extra_symbols: extras.iter().map(|s| (*s).to_string()).collect::<Vec<_>>().into(),
        tier,
        ..Default::default()
    }
}

/// One random ask of the door about one sample venue.
#[derive(Clone, Debug)]
struct Case {
    sample: Sample,
    tier: Option<AccountTier>,
    label: Label,
    reduce_only: bool,
    /// A limit price rides the order (else it is a market order).
    limit_price: bool,
    answers: Answers,
    commit: bool,
    /// Which letters of a typed symbol have their case flipped, and how it is padded with whitespace.
    mask: u64,
    pad: u8,
}

impl Case {
    fn key(&self) -> String {
        self.label.key(self.sample.venue)
    }
    fn command(&self, symbol: &str) -> WireCommand {
        submit(
            self.sample.venue,
            symbol,
            self.label,
            self.reduce_only,
            self.limit_price.then_some(1.0),
        )
    }
    /// `symbol` as an operator might type it: letters flipped by `mask` on a venue that folds case,
    /// and padded with whitespace the door trims.
    fn spelled(&self, symbol: &str) -> String {
        let flip = |(i, c): (usize, char)| {
            let flipped = (self.mask >> (i % 64)) & 1 == 1;
            match (flipped, c.is_ascii_uppercase()) {
                (true, true) => c.to_ascii_lowercase(),
                (true, false) => c.to_ascii_uppercase(),
                (false, _) => c,
            }
        };
        let body: String = if self.sample.folds_case {
            symbol.chars().enumerate().map(flip).collect()
        } else {
            symbol.to_string()
        };
        match self.pad % 3 {
            0 => body,
            1 => format!(" {body}"),
            _ => format!("{body}\t "),
        }
    }
    /// Whether the door accepts a foreign symbol on this tier: a `demo` account, or a `live` one with
    /// an account-wide exposure ceiling armed.
    fn tier_admits(&self) -> bool {
        match self.tier {
            Some(AccountTier::Demo) => true,
            Some(AccountTier::Live) => self.answers.capped,
            Some(AccountTier::Paper) | None => false,
        }
    }
}

fn tiers() -> impl Strategy<Value = Option<AccountTier>> {
    prop_oneof![
        Just(Some(AccountTier::Demo)),
        Just(Some(AccountTier::Live)),
        Just(Some(AccountTier::Paper)),
        Just(None),
    ]
}

fn labels() -> impl Strategy<Value = Label> {
    prop_oneof![Just(Label::Absent), Just(Label::Default), Just(Label::Alt)]
}

fn answers() -> impl Strategy<Value = Answers> {
    (
        any::<bool>(),
        prop::bool::weighted(0.25),
        prop_oneof![
            Just(PriceAnswer::Ready),
            Just(PriceAnswer::Pending),
            Just(PriceAnswer::Failing)
        ],
        prop_oneof![
            Just(RefreshAnswer::NotDue),
            Just(RefreshAnswer::Due),
            Just(RefreshAnswer::ForAnotherSymbol),
            Just(RefreshAnswer::ForAnotherAccount)
        ],
        prop::bool::weighted(0.75),
    )
        .prop_map(|(unpriced, lookup_fails, price, refresh, capped)| Answers {
            unpriced,
            lookup_fails,
            price,
            refresh,
            capped,
        })
}

/// A random ask over the samples of `pool`.
fn cases_over(pool: Vec<Sample>) -> impl Strategy<Value = Case> {
    (
        prop::sample::select(pool),
        tiers(),
        labels(),
        any::<bool>(),
        any::<bool>(),
        answers(),
        any::<bool>(),
        any::<u64>(),
        any::<u8>(),
    )
        .prop_map(
            |(sample, tier, label, reduce_only, limit_price, answers, commit, mask, pad)| Case {
                sample,
                tier,
                label,
                reduce_only,
                limit_price,
                answers,
                commit,
                mask,
                pad,
            },
        )
}

fn cases() -> impl Strategy<Value = Case> {
    cases_over(SAMPLES.to_vec())
}

fn folding_cases() -> impl Strategy<Value = Case> {
    cases_over(SAMPLES.iter().copied().filter(|s| s.folds_case).collect())
}

/// A placement's symbol, the admission that rides it, and whether the order waits for a price.
type Placed = (String, Option<Box<SymbolAdmission>>, bool);

fn placed(plan: SymbolPlan) -> Result<Placed, TestCaseError> {
    match plan {
        SymbolPlan::Place { symbol, admit, wait } => Ok((symbol, admit, wait.is_some())),
        other => Err(TestCaseError::fail(format!("expected a placement, got {other:?}"))),
    }
}

fn refusal(plan: SymbolPlan) -> Result<String, TestCaseError> {
    match plan {
        SymbolPlan::Refuse(why) => Ok(why),
        other => Err(TestCaseError::fail(format!("expected a refusal, got {other:?}"))),
    }
}

// ---------------------------------------------------------------------------------------------
// The laws.
// ---------------------------------------------------------------------------------------------

proptest! {
    /// **A symbol the engine already trades is never re-admitted and never re-looked-up**, whatever
    /// the facts source would answer: the engine's own symbol passes untouched with no call at all,
    /// and a declared leg or earlier admission is placed as written, asking the source only for the
    /// two things its doc allows. A REFRESH of the admission rides ahead of the order exactly when
    /// committing and the source says one is due for THAT symbol on THAT account; a price is asked
    /// (and the order held for the first one) only for a real account that is committing.
    #[test]
    fn a_symbol_the_engine_trades_is_never_readmitted_or_looked_up(
        c in cases(),
        ask_the_extra in any::<bool>(),
    ) {
        let key = c.key();
        let engine = block(&key, c.sample.mounted, &[c.sample.foreign], c.tier);
        let typed = if ask_the_extra { c.sample.foreign } else { c.sample.mounted };
        let facts = Model::new(&c.answers);
        let plan = plan_submit_symbol(
            &c.command(typed),
            std::slice::from_ref(&engine),
            Some(&facts),
            c.commit,
        );
        let calls = facts.calls();
        if !ask_the_extra {
            prop_assert_eq!(plan, SymbolPlan::Pass);
            prop_assert!(calls.is_empty(), "the engine's own symbol asked the source: {calls:?}");
            return Ok(());
        }
        let (symbol, admit, waits) = placed(plan)?;
        prop_assert_eq!(&symbol, typed);
        for call in &calls {
            prop_assert!(
                !matches!(call, Call::Admission(_) | Call::PriceSource),
                "a traded symbol was looked up: {calls:?}"
            );
        }
        let refreshes = calls.iter().filter(|call| matches!(call, Call::Refresh(_))).count();
        prop_assert_eq!(refreshes, usize::from(c.commit), "a refresh is asked only when committing");
        prop_assert_eq!(
            admit.is_some(),
            c.commit && c.answers.refresh == RefreshAnswer::Due,
            "an admission rides the order only as a due refresh of this very symbol"
        );
        if let Some(admit) = admit {
            prop_assert_eq!((admit.route_key.as_str(), admit.symbol.as_str()), (key.as_str(), typed));
        }
        let real = matches!(c.tier, Some(AccountTier::Demo | AccountTier::Live));
        let priced = calls
            .iter()
            .filter(|call| matches!(call, Call::Price { subscribe: true, .. }))
            .count();
        prop_assert_eq!(priced, usize::from(c.commit && real), "the price lane is asked once, subscribing");
        prop_assert!(
            !calls.iter().any(|call| matches!(call, Call::Price { subscribe: false, .. })),
            "the planner peeked at a price"
        );
        prop_assert_eq!(
            waits,
            c.commit
                && real
                && matches!(c.answers.price, PriceAnswer::Pending)
                && !c.reduce_only,
            "an opening order waits for the first price of a symbol that has none yet"
        );
    }

    /// **Another spelling of a symbol the engine trades is placed on the engine's spelling or refused,
    /// never admitted as a second instrument.** Case, padding and (per venue) the other spelling of one
    /// instrument (bybit's `.P`, alpaca's `BTCUSD` for `BTC/USD`, ibkr's bare ticker) all name something
    /// the engine already holds; the facts source is never asked about them, and on a real account
    /// that can take a foreign symbol a case variant of a canonical symbol is placed on the engine's own.
    #[test]
    fn another_spelling_of_a_traded_symbol_is_never_admitted_as_a_second_instrument(
        c in folding_cases(),
        variant in 0u8..3,
        with_extra in any::<bool>(),
    ) {
        let key = c.key();
        let extras: Vec<&str> = if with_extra { vec![c.sample.foreign] } else { Vec::new() };
        let engine = block(&key, c.sample.mounted, &extras, c.tier);
        // (what is typed, the engine's spelling it names, whether it is a mere case variant)
        let (typed, expected, case_only) = match (variant, c.sample.alias, with_extra) {
            (1, _, true) => (c.spelled(c.sample.foreign), c.sample.foreign, true),
            (2, Some(alias), _) => (c.spelled(alias), c.sample.mounted, false),
            _ => (c.spelled(c.sample.mounted), c.sample.mounted, true),
        };
        let facts = Model::new(&c.answers);
        let plan = plan_submit_symbol(
            &c.command(&typed),
            std::slice::from_ref(&engine),
            Some(&facts),
            c.commit,
        );
        let trades: Vec<&str> =
            std::iter::once(c.sample.mounted).chain(extras.iter().copied()).collect();
        prop_assert!(
            !facts.calls().iter().any(|call| matches!(call, Call::Admission(_))),
            "`{typed}` names a traded instrument and was looked up: {:?}",
            facts.calls()
        );
        match &plan {
            SymbolPlan::Place { symbol, admit, .. } => {
                prop_assert!(
                    trades.contains(&symbol.as_str()),
                    "`{typed}` was placed on `{symbol}`, which the engine does not trade ({trades:?})"
                );
                if let Some(admit) = admit {
                    prop_assert_eq!(&admit.symbol, symbol, "an admission may only refresh a traded symbol");
                }
            }
            SymbolPlan::Pass => {
                prop_assert_eq!(typed.as_str(), c.sample.mounted, "only the engine's own spelling passes");
            }
            SymbolPlan::Refuse(_) => {}
        }
        if case_only && c.sample.canonical && c.tier_admits() {
            match plan {
                SymbolPlan::Pass => prop_assert!(
                    typed == c.sample.mounted && expected == c.sample.mounted,
                    "`{typed}` passed untouched but is not the engine's own symbol"
                ),
                SymbolPlan::Place { symbol, .. } => prop_assert_eq!(symbol, expected),
                SymbolPlan::Refuse(why) => {
                    return Err(TestCaseError::fail(format!("`{typed}` was refused: {why}")));
                }
            }
        }
    }

    /// **An opening order with no price handle is refused before ANY facts lookup** (the v0.1.72
    /// ordering: ibkr's lookup is a budgeted data connect, so an order no price can ever back must not
    /// spend it). Whatever the lookup, the price lane and the refresh would have answered, and however
    /// full the engine's symbol list is, the source is asked at most "do you price this venue at all".
    #[test]
    fn an_opening_order_with_no_price_handle_is_refused_before_any_lookup(
        c in cases(),
        fillers in 0usize..=(MAX_EXTRA_SYMBOLS + 1),
        live in any::<bool>(),
    ) {
        let answers = Answers { unpriced: true, capped: true, ..c.answers };
        let tier = Some(if live { AccountTier::Live } else { AccountTier::Demo });
        let key = c.key();
        let names: Vec<String> = (0..fillers).map(|i| format!("FILL{i}")).collect();
        let extras: Vec<&str> = names.iter().map(String::as_str).collect();
        let engine = block(&key, c.sample.mounted, &extras, tier);
        let typed = c.spelled(c.sample.foreign);
        let command = submit(c.sample.venue, &typed, c.label, false, c.limit_price.then_some(1.0));
        let facts = Model::new(&answers);
        let plan = plan_submit_symbol(&command, std::slice::from_ref(&engine), Some(&facts), c.commit);
        let why = refusal(plan)?;
        if fillers < MAX_EXTRA_SYMBOLS {
            prop_assert_eq!(facts.lookups(), vec![Call::PriceSource], "{}", why);
            prop_assert!(why.contains(NO_FEED), "the source's own words must reach the operator: {why}");
        } else {
            prop_assert!(facts.lookups().is_empty(), "a full engine was looked up for: {why}");
        }
    }

    /// **A reduce-only order is never refused for a missing price feed**, and never held up for one:
    /// the planner does not even ask whether the venue is priced. It also never spends an admission
    /// slot: on a symbol the engine does not trade it is refused outright, and on one it trades (the
    /// engine's own or a leg) it is placed whatever the tier and the facts say.
    #[test]
    fn a_reduce_only_order_is_never_refused_for_a_missing_price_feed(
        c in cases(),
        target in 0u8..3,
    ) {
        let answers = Answers { unpriced: true, ..c.answers };
        let key = c.key();
        let extras: Vec<&str> = if target == 1 { vec![c.sample.foreign] } else { Vec::new() };
        let engine = block(&key, c.sample.mounted, &extras, c.tier);
        let typed = match target {
            0 => c.sample.mounted.to_string(),
            1 => c.sample.foreign.to_string(),
            _ => c.spelled(c.sample.foreign),
        };
        let command = submit(c.sample.venue, &typed, c.label, true, c.limit_price.then_some(1.0));
        let facts = Model::new(&answers);
        let plan = plan_submit_symbol(&command, std::slice::from_ref(&engine), Some(&facts), c.commit);
        prop_assert!(
            !facts.calls().contains(&Call::PriceSource),
            "an exit asked whether the venue is priced: {:?}",
            facts.calls()
        );
        match (&plan, target) {
            (SymbolPlan::Pass, 0) | (SymbolPlan::Place { .. }, 1) | (SymbolPlan::Refuse(_), 2) => {}
            _ => return Err(TestCaseError::fail(format!("target {target} on a traded/new symbol: {plan:?}"))),
        }
        match plan {
            SymbolPlan::Refuse(why) => {
                for word in [NO_FEED, PRICE_DOWN, "no price feed", "no price on this node"] {
                    prop_assert!(!why.contains(word), "a reduce-only order was refused for `{word}`: {why}");
                }
                prop_assert!(
                    !facts.calls().iter().any(|call| matches!(call, Call::Price { .. })),
                    "a refused exit still asked for a price"
                );
            }
            SymbolPlan::Place { wait, .. } => prop_assert!(wait.is_none(), "an exit was held for a price"),
            SymbolPlan::Pass => {}
        }
    }

    /// **The per-order venues follow the written table, and the facts source is never asked.** A symbol
    /// that is the engine's own (in any case), a declared leg or no symbol at all is not foreign; for a
    /// foreign one polymarket passes, a paper account is refused and fxcm is refused. (oanda was the
    /// third row, passing only a reduce-only or priced order, until it became `Admits`: it is a
    /// [`SAMPLES`] row now.) A venue outside the roster is not this gate's.
    #[test]
    fn the_per_order_venues_follow_the_written_table(
        venue in prop::sample::select(vec!["polymarket", "fxcm", "sim"]),
        tier in tiers(),
        typed in prop::sample::select(vec!["EUR_USD", "eur_usd", "GBP_USD", "", "TOKEN-2"]),
        typed_is_a_leg in any::<bool>(),
        reduce_only in any::<bool>(),
        priced in any::<bool>(),
        label in labels(),
        commit in any::<bool>(),
    ) {
        let key = label.key(venue);
        let legs: Vec<&str> = if typed_is_a_leg && !typed.is_empty() { vec![typed] } else { Vec::new() };
        let engine = block(&key, "EUR_USD", &legs, tier);
        let command = submit(venue, typed, label, reduce_only, priced.then_some(1.0));
        let facts = Model::new(&Answers {
            unpriced: true,
            lookup_fails: true,
            price: PriceAnswer::Failing,
            refresh: RefreshAnswer::Due,
            capped: false,
        });
        let plan = plan_submit_symbol(&command, std::slice::from_ref(&engine), Some(&facts), commit);
        let own = typed.is_empty() || typed.eq_ignore_ascii_case("EUR_USD") || legs.contains(&typed);
        let refused = !(venue == "sim" || own || venue == "polymarket")
            && (tier == Some(AccountTier::Paper) || venue == "fxcm");
        prop_assert_eq!(
            matches!(plan, SymbolPlan::Refuse(_)),
            refused,
            "{} {:?} on {:?}: {:?}", venue, typed, tier, plan
        );
        prop_assert!(!matches!(plan, SymbolPlan::Place { .. }), "a per-order venue admitted a symbol");
        prop_assert!(facts.calls().is_empty(), "a per-order venue asked the source: {:?}", facts.calls());
        let without = plan_submit_symbol(&command, std::slice::from_ref(&engine), None, commit);
        prop_assert_eq!(plan, without, "a per-order verdict must not depend on having a source");
    }

    /// **The plan is a function of the command, the ADDRESSED engine's block and the facts' answers.**
    /// Two identical asks agree (the plan and the questions put to the source alike), and any number of
    /// other accounts' blocks, in any position and carrying any symbols, change neither.
    #[test]
    fn the_plan_depends_only_on_the_command_the_addressed_engine_and_the_facts(
        c in cases(),
        pick in 0u8..8,
        extras_mode in 0u8..3,
        decoys in prop::collection::vec(
            (prop::sample::select(vec!["bybit", "okx", "binance", "ibkr", ""]), 0u8..4, tiers(), 0u8..3),
            0..6,
        ),
        at in 0usize..7,
        with_facts in any::<bool>(),
    ) {
        let key = c.key();
        let over_cap: Vec<String> = (0..MAX_EXTRA_SYMBOLS).map(|i| format!("FILL{i}")).collect();
        let extras: Vec<&str> = match extras_mode {
            0 => Vec::new(),
            1 => vec![c.sample.foreign],
            _ => over_cap.iter().map(String::as_str).collect(),
        };
        let target = block(&key, c.sample.mounted, &extras, c.tier);
        let long = "A".repeat(80);
        let typed = match pick {
            0 => c.sample.mounted.to_string(),
            1 => c.sample.foreign.to_string(),
            2 => c.spelled(c.sample.mounted),
            3 => c.spelled(c.sample.foreign),
            4 => c.spelled(c.sample.alias.unwrap_or(c.sample.mounted)),
            5 => String::new(),
            6 => "!!bad symbol".to_string(),
            _ => long,
        };
        let command = c.command(&typed);
        // Decoys never share the target's route key; they carry the symbols the target is asked about.
        let decoys: Vec<VenueBlock> = decoys
            .iter()
            .enumerate()
            .map(|(i, (venue, symbol, tier, n))| {
                let symbols = [c.sample.mounted, c.sample.foreign, typed.as_str(), "X"];
                let key = if venue.is_empty() { String::new() } else { format!("{venue}#DECOY{i}") };
                let legs = [symbols[usize::from(*n) % 4]];
                block(&key, symbols[usize::from(*symbol) % 4], &legs, *tier)
            })
            .collect();
        let mut roster = decoys;
        roster.insert(at.min(roster.len()), target.clone());
        let run = |blocks: &[VenueBlock]| {
            let facts = Model::new(&c.answers);
            let plan = plan_submit_symbol(&command, blocks, with_facts.then_some(&facts as &dyn SymbolFacts), c.commit);
            (plan, facts.calls())
        };
        let alone = run(std::slice::from_ref(&target));
        prop_assert_eq!(&run(std::slice::from_ref(&target)), &alone, "the same ask twice must agree");
        prop_assert_eq!(&run(&roster), &alone, "another account's block moved the verdict");
    }

    /// **A canonical symbol is ASCII, inside its venue's alphabet and a fixed point** of the
    /// canonicaliser, for ANY text an operator (or a hostile peer) could send: the symbol reaches a
    /// signed query string, a URL path and a JSON-RPC frame downstream, so nothing outside the venue's
    /// alphabet may come out. Spellings that fold case give the same answer for any case of the input;
    /// hyperliquid's and ctrader's keep the case they were given.
    #[test]
    fn a_canonical_symbol_is_idempotent_ascii_and_inside_the_venues_alphabet(
        raw in prop_oneof![
            any::<String>(),
            "[A-Za-z0-9._/ -]{0,40}",
            (cases(), 0u8..3).prop_map(|(c, which)| {
                let s = c.sample;
                c.spelled([s.mounted, s.foreign, s.alias.unwrap_or(s.mounted)][usize::from(which)])
            }),
        ],
    ) {
        for spelling in SPELLINGS {
            let answer = canonical_symbol(spelling, &raw).ok();
            if folds_case(spelling) {
                prop_assert_eq!(
                    &answer,
                    &canonical_symbol(spelling, &raw.to_ascii_uppercase()).ok(),
                    "{:?}: case changed the verdict on {:?}", spelling, raw
                );
                prop_assert_eq!(
                    &answer,
                    &canonical_symbol(spelling, &raw.to_ascii_lowercase()).ok(),
                    "{:?}: case changed the verdict on {:?}", spelling, raw
                );
            }
            let Some(symbol) = answer else { continue };
            prop_assert!(!symbol.is_empty() && symbol.len() <= 64, "{:?}: {:?}", spelling, symbol);
            prop_assert!(
                symbol
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || extra_alphabet(spelling).contains(ch)),
                "{:?} let a character through: {:?} from {:?}", spelling, symbol, raw
            );
            let again = canonical_symbol(spelling, &symbol).ok();
            prop_assert_eq!(
                again.as_deref(),
                Some(symbol.as_str()),
                "{:?}: canonicalising twice moved {:?}", spelling, symbol
            );
            if !folds_case(spelling) {
                prop_assert_eq!(symbol.as_str(), raw.trim(), "{:?} must keep the name as written", spelling);
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// **The door plans against the NEWEST block of an account's route key** (the v0.1.72 door fix).
    /// The core's engine list only grows, so a restarted account has its stopped engine's block before
    /// the new one's; `PublisherHandle::engine_blocks` hands the door one block per key, and the plan
    /// it gets is the plan of the new block alone, whatever stale blocks and other accounts surround it.
    /// (48 cases: each one starts and stops a publisher thread.)
    #[test]
    fn the_door_plans_against_the_newest_block_of_a_restarted_account(
        c in cases(),
        stale in prop::collection::vec((0u8..3, tiers(), 0u8..2), 1..3),
        decoys in prop::collection::vec((prop::sample::select(vec!["bybit", "okx", ""]), tiers()), 0..4),
        new_at in 0usize..5,
        stale_at in prop::collection::vec(0usize..6, 2),
        ask_the_extra in any::<bool>(),
    ) {
        let key = c.key();
        let new = block(&key, c.sample.mounted, &[c.sample.foreign], c.tier);
        let typed =
            if ask_the_extra { c.sample.foreign.to_string() } else { c.spelled(c.sample.foreign) };
        let command = c.command(&typed);
        let mut roster: Vec<VenueBlock> = decoys
            .iter()
            .enumerate()
            .map(|(i, (venue, tier))| {
                let key = if venue.is_empty() { String::new() } else { format!("{venue}#DECOY{i}") };
                block(&key, "X", &[], *tier)
            })
            .collect();
        let first_new_idx = new_at.min(roster.len());
        roster.insert(first_new_idx, new.clone());
        // Each stale block lands before the new one, which moves one place down the list per insert.
        for (i, (symbol, tier, legs)) in stale.iter().enumerate() {
            let new_idx = first_new_idx + i;
            let mounted = [c.sample.mounted, c.sample.foreign, "STALE"][usize::from(*symbol)];
            let extras: Vec<&str> = if *legs == 1 { vec![c.sample.foreign] } else { Vec::new() };
            let at = stale_at[i % stale_at.len()] % (new_idx + 1);
            roster.insert(at, block(&key, mounted, &extras, *tier));
        }
        let cell = Arc::new(arc_swap::ArcSwap::from_pointee({
            let mut snap = CoreSnapshot::empty(c.sample.venue, c.sample.mounted);
            snap.portfolio.venues = roster;
            snap
        }));
        let handle = crate::publish::spawn(cell, None);
        let doors = handle.engine_blocks();
        handle.shutdown();
        prop_assert_eq!(doors.iter().filter(|b| b.route_key == key).count(), 1);
        let run = |blocks: &[VenueBlock]| {
            let facts = Model::new(&c.answers);
            let plan = plan_submit_symbol(&command, blocks, Some(&facts), c.commit);
            (plan, facts.calls())
        };
        prop_assert_eq!(run(&doors), run(std::slice::from_ref(&new)));
    }
}

/// The property above is not vacuous: handed the raw roster of a restarted account the planner judges
/// the STOPPED engine (it resolves a block by route key, first match), and only
/// `PublisherHandle::engine_blocks` makes the door judge the new one.
#[test]
fn the_raw_roster_of_a_restarted_account_would_judge_the_stopped_engine() {
    let stopped = block("bybit", "BTCUSDT", &[], Some(AccountTier::Paper));
    let running = block("bybit", "BTCUSDT", &[], Some(AccountTier::Demo));
    let facts_for = || {
        Model::new(&Answers {
            unpriced: false,
            lookup_fails: false,
            price: PriceAnswer::Ready,
            refresh: RefreshAnswer::NotDue,
            capped: true,
        })
    };
    let command = submit("bybit", "ETHUSDT", Label::Absent, false, Some(1.0));
    let plan =
        |blocks: &[VenueBlock]| plan_submit_symbol(&command, blocks, Some(&facts_for()), true);
    let raw = [stopped.clone(), running.clone()];
    assert!(
        matches!(plan(&raw), SymbolPlan::Refuse(_)),
        "a stopped paper engine refuses a foreign symbol"
    );
    assert!(matches!(plan(std::slice::from_ref(&running)), SymbolPlan::Place { .. }));
    let mut snap = CoreSnapshot::empty("bybit", "BTCUSDT");
    snap.portfolio.venues = vec![stopped, running.clone()];
    let handle = crate::publish::spawn(Arc::new(arc_swap::ArcSwap::from_pointee(snap)), None);
    let doors = handle.engine_blocks();
    handle.shutdown();
    assert_eq!(plan(&doors), plan(std::slice::from_ref(&running)));
}
