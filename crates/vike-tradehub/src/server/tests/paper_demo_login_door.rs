//! A `paper` account of a venue priced through its DEMO login (dukascopy, fxcm, ibkr; the owner's ruling of
//! 2026-11-16) at the control door: with a demo login running, an order on its own symbol holds that symbol's
//! price lane and an opening market order waits for the first price, as a demo account's does; with none, every
//! order is refused in the words `trade health` shows (`symbol_admission::no_demo_login`). Paper accounts of every
//! other venue, and demo accounts, are untouched.

use std::sync::Mutex;

use vike_execution::{SymbolAdmission, VenueBlock};
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;
use vike_model::AccountTier;
use vike_tradehub_client::wire::CommandWire;

use super::admits_any_symbol::{limit, refused, sell_to_close};
use crate::server::symbol_admission::{
    AccountReadiness, SymbolAdmitter, SymbolPlan, no_demo_login, plan_submit_symbol,
};

/// Each venue priced through its demo login, with a symbol its paper engine runs on.
const VENUES: [(&str, &str); 3] =
    [("dukascopy", "EURUSD"), ("fxcm", "EURUSD"), ("ibkr", "AAPL.SMART.USD")];

/// A source that says whether a demo login runs and whether a bar series reaches a symbol, and records the lanes it is
/// asked to hold.
struct Demo {
    login: bool,
    bars: bool,
    holds: Mutex<Vec<String>>,
    /// Why no price feed of the venue runs here (`SymbolAdmitter::price_source`), or `None`: one runs.
    no_feed: Option<String>,
}

impl Demo {
    fn running(login: bool) -> Self {
        Demo { login, bars: true, holds: Mutex::new(Vec::new()), no_feed: None }
    }

    /// A source with a demo login, whose node holds no bar series of any symbol (no strategy mount).
    fn unmounted() -> Self {
        Demo { bars: false, ..Demo::running(true) }
    }
}

impl SymbolAdmitter for Demo {
    fn admission(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        panic!("an order on the paper engine's own symbol looks nothing up at the door")
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(None)
    }
    fn hold_lane(&self, route_key: &str, _: &str, symbol: &str) {
        self.holds.lock().unwrap().push(format!("{route_key}/{symbol}"));
    }
    fn demo_login(&self, _: &str) -> bool {
        self.login
    }
    fn bar_series(&self, _: &str, _: &str) -> bool {
        self.bars
    }
    fn price_source(&self, _: &str) -> Result<(), String> {
        self.no_feed.clone().map_or(Ok(()), Err)
    }
}

impl AccountReadiness for Demo {
    fn generation(&self, _: &str) -> u64 {
        0
    }
}

fn engine(venue: &str, symbol: &str, tier: AccountTier) -> VenueBlock {
    VenueBlock {
        venue: venue.into(),
        route_key: venue.into(),
        symbol: symbol.into(),
        tier: Some(tier),
        ..Default::default()
    }
}

fn market(venue: &str, symbol: &str) -> CommandWire {
    let CommandWire::Submit(mut req) = limit("m-1", venue, symbol, 1.0, 1.0) else {
        unreachable!()
    };
    req.order_type = "market".into();
    req.price = None;
    CommandWire::Submit(req)
}

/// **No demo login, no order**: a paper account of dukascopy, fxcm or ibkr refuses every order (opening, priced,
/// closing) in the words `trade health` shows, at the send and at a Preview alike, and so does a node with no
/// source to ask. A demo account of the same venue, and a paper account of a venue with its own feed, are untouched.
#[test]
fn with_no_demo_login_every_order_of_the_paper_account_says_no_price() {
    let none = Demo::running(false);
    for (venue, symbol) in VENUES {
        let paper = [engine(venue, symbol, AccountTier::Paper)];
        let words = no_demo_login(venue);
        assert_eq!(words, format!("no price: no demo login for {venue}"));
        for order in [
            market(venue, symbol),
            limit("l-1", venue, symbol, 1.0, 1.0),
            sell_to_close("c-1", venue, symbol, 1.0, 1.0),
        ] {
            for commit in [true, false] {
                let why = refused(plan_submit_symbol(&order, &paper, Some(&none), commit));
                assert!(why.starts_with(&words), "{venue}: {why}");
                let why = refused(plan_submit_symbol(&order, &paper, None, commit));
                assert!(why.starts_with(&words), "{venue}, no source: {why}");
            }
        }
        assert!(none.holds.lock().unwrap().is_empty(), "a refused order holds no lane");
        let demo = [engine(venue, symbol, AccountTier::Demo)];
        let order = limit("l-1", venue, symbol, 1.0, 1.0);
        assert_eq!(plan_submit_symbol(&order, &demo, Some(&none), false), SymbolPlan::Pass);
    }
    let oanda = [engine("oanda", "EURUSD", AccountTier::Paper)];
    let order = limit("l-1", "oanda", "EURUSD", 1.0, 1.0);
    assert_eq!(plan_submit_symbol(&order, &oanda, Some(&none), true), SymbolPlan::Pass);
}

/// **With a demo login the paper account is a demo account's twin at the door**: an order on its own symbol holds
/// that symbol's price lane (through the demo login: the paper account itself never logs in), and an opening market
/// order is held for the first price instead of resting on a book nothing prices.
#[test]
fn with_a_demo_login_the_paper_account_holds_its_lane_and_waits_for_its_first_price() {
    for (venue, symbol) in VENUES {
        let login = Demo::running(true);
        let paper = [engine(venue, symbol, AccountTier::Paper)];
        match plan_submit_symbol(&market(venue, symbol), &paper, Some(&login), true) {
            SymbolPlan::Place { symbol: placed, admit: None, wait: Some(_) } => {
                assert_eq!(placed, symbol);
            }
            other => {
                panic!("{venue}: an opening market order waits for its first price: {other:?}")
            }
        }
        let order = limit("l-1", venue, symbol, 1.0, 1.0);
        assert_eq!(plan_submit_symbol(&order, &paper, Some(&login), true), SymbolPlan::Pass);
        assert_eq!(
            *login.holds.lock().unwrap(),
            vec![format!("{venue}/{symbol}"), format!("{venue}/{symbol}")],
            "{venue}: each committed order holds the lane of the engine's own symbol"
        );
        assert_eq!(plan_submit_symbol(&order, &paper, Some(&login), false), SymbolPlan::Pass);
        assert_eq!(login.holds.lock().unwrap().len(), 2, "a Preview holds nothing");
    }
}

/// The identity block of a node: `live` is the switch, `priced` what a paper-only node prices.
fn identity(live: bool, priced: Option<&[&str]>) -> vike_tradehub_client::wire::HubIdentityWire {
    vike_tradehub_client::wire::HubIdentityWire {
        name: "n".into(),
        strategy: "s".into(),
        params: String::new(),
        live,
        build: "b".into(),
        advertise_addr: String::new(),
        priced_venues: priced.map(|venues| venues.iter().map(|v| v.to_string()).collect()),
    }
}

/// **A paper-only node refuses every manual order on a venue it does not price, by name** (`flags.tradehub_live` off,
/// the owner's yes of 2026-11-16: it prices only the keyless venues it wired, told to the surface by
/// `ControlLimits::with_identity` from `HubIdentityWire::priced_venues`), in the words `trade health` lists such a mount
/// with. An order on a venue it prices, a cancel and a mount pass; a live node and a surface never told refuse nothing so.
#[test]
fn a_paper_only_node_refuses_an_order_on_a_venue_it_does_not_price_by_name() {
    use crate::server::control::{ControlLimits, ControlLimitsConfig, no_price_feed_refusal};
    use vike_tradehub_client::wire::no_price_feed;
    let untold = ControlLimits::new(ControlLimitsConfig::default());
    let paper_only = ControlLimits::new(ControlLimitsConfig::default())
        .with_identity(Some(identity(false, Some(&["binance"]))));
    let live = ControlLimits::new(ControlLimitsConfig::default())
        .with_identity(Some(identity(true, None)));
    for (venue, order) in [
        ("oanda", market("oanda", "EUR_USD")),
        ("bybit", market("bybit", "BTCUSDT")),
        ("dukascopy", limit("l-1", "dukascopy", "EURUSD", 1.0, 1.0)),
        ("ig", sell_to_close("c-1", "ig", "CS.D.EURUSD.MINI.IP", 1.0, 1.0)),
    ] {
        let why = no_price_feed_refusal(&order, &paper_only).expect("refused").text;
        assert!(why.starts_with(&format!("{}: ", no_price_feed(venue))), "{why}");
        assert!(why.contains(&format!("no order on {venue} can fill here")), "{why}");
        assert_eq!(no_price_feed_refusal(&order, &untold), None, "a surface never told");
        assert_eq!(no_price_feed_refusal(&order, &live), None, "a live node");
    }
    assert_eq!(
        no_price_feed_refusal(&market("binance", "BTCUSDT"), &paper_only),
        None,
        "a venue the node prices"
    );
    assert_eq!(
        no_price_feed("oanda"),
        "no price: this node has no price feed for oanda (paper-only mode, flags.tradehub_live off)"
    );
    assert_eq!(no_price_feed_refusal(&CommandWire::Cancel("c-1".into()), &paper_only), None);
    // A node stating no list (one built before it) prices by its switch alone, as the node-wide fact did.
    let unstated = ControlLimits::new(ControlLimitsConfig::default())
        .with_identity(Some(identity(false, None)));
    assert!(no_price_feed_refusal(&market("binance", "BTCUSDT"), &unstated).is_some());
}

/// The series a paper-only node's feeds were wired for at its start (its boot mounts').
fn fed(series: &[(&str, &str)]) -> Vec<vike_execution::SeriesKey> {
    series.iter().map(|(v, s)| (v.to_string(), s.to_string(), "1m".to_string())).collect()
}

/// **A runtime mount on a paper-only node is told when nothing will price it**: on a venue the node does not price, in
/// the door's own words; on a priced venue at a series no BOOT mount trades (the feeds were wired at the start), that
/// it gets no bar, even when an earlier runtime mount named the series; on the series a boot mount trades, nothing. A
/// live node says neither.
#[test]
fn a_runtime_mount_on_a_paper_only_node_is_told_when_nothing_prices_it() {
    use crate::server::control::{ControlLimits, ControlLimitsConfig, runtime_mount_price_note};
    let mount = |venue: &str, symbol: &str| CommandWire::MountStrategy {
        venue: venue.into(),
        account: None,
        symbol: symbol.into(),
        interval: "1m".into(),
        controller_id: Some(format!("runtime-{venue}-{symbol}")),
        file: Some("buy_hold.rhai".into()),
        name: None,
        rhai: None,
        params: serde_json::json!({}),
    };
    let note = |identity: vike_tradehub_client::wire::HubIdentityWire, cmd: CommandWire| {
        let limits = ControlLimits::new(ControlLimitsConfig::default())
            .with_identity(Some(identity))
            .with_fed_series(fed(&[("binance", "BTCUSDT")]));
        runtime_mount_price_note(&cmd, &limits)
    };
    let paper_only = || identity(false, Some(&["binance"]));

    let said = note(paper_only(), mount("oanda", "EUR_USD")).expect("oanda is not priced");
    assert!(said.starts_with(&vike_tradehub_client::wire::no_price_feed("oanda")), "{said}");
    let said = note(paper_only(), mount("binance", "ETHUSDT")).expect("no feed for the series");
    assert!(
        said.starts_with("no price: this paper-only node wired its binance feed at its start")
            && said.contains("ETHUSDT 1m"),
        "{said}"
    );
    assert_eq!(note(paper_only(), mount("binance", "BTCUSDT")), None, "the boot mount's series");
    assert_eq!(note(identity(true, None), mount("binance", "ETHUSDT")), None, "a live node");
    assert_eq!(note(paper_only(), CommandWire::Cancel("c-1".into())), None, "not a mount");
}

/// A published strategy mount on `(venue, symbol)`, with `legs` declared beside it.
fn a_mount(venue: &str, symbol: &str, legs: &[(&str, &str)]) -> vike_execution::MountView {
    vike_execution::MountView {
        kind: vike_execution::MountViewKind::Mount,
        mount_id: format!("{venue}__{symbol}__1m"),
        venue: venue.into(),
        symbol: symbol.into(),
        interval: "1m".into(),
        ready: true,
        position: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        notional: 0.0,
        budget: None,
        latched: false,
        params: None,
        currency: Default::default(),
        legs: (!legs.is_empty())
            .then(|| legs.iter().map(|(v, s)| (v.to_string(), s.to_string())).collect()),
    }
}

/// **The paper-only node (no source to ask: `flags.tradehub_live` off, the default setup) says it too**, from the mounts it
/// publishes: an order of a paper account on a symbol no published strategy mount trades, as its own symbol or a declared
/// leg, is refused in the same words; a mounted symbol, a leg, a node WITH a source (judged inside the plan), a demo account
/// and a venue priced through its demo login (the plan's own refusal) are not this check's. The door's mount rows now come
/// with an order (`crate::publish`'s `named_mounts`).
#[test]
fn a_paper_only_node_refuses_an_order_no_published_mount_trades() {
    use crate::server::symbol_admission::paper_unmounted_refusal;
    let paper = [engine("bybit", "BTCUSDT", AccountTier::Paper)];
    for order in [
        market("bybit", "BTCUSDT"),
        limit("l-1", "bybit", "BTCUSDT", 1.0, 1.0),
        sell_to_close("c-1", "bybit", "BTCUSDT", 1.0, 1.0),
    ] {
        let why =
            paper_unmounted_refusal(&order, &paper, None, &[], None).expect("no mount at all").text;
        assert!(
            why.starts_with("no price: no strategy mount trades `BTCUSDT` on bybit on this node"),
            "{why}"
        );
        let elsewhere = [a_mount("bybit", "ETHUSDT", &[]), a_mount("okx", "BTCUSDT", &[])];
        assert!(
            paper_unmounted_refusal(&order, &paper, None, &elsewhere, None).is_some(),
            "other pairs"
        );
        let mounted = [a_mount("bybit", "BTCUSDT", &[])];
        assert_eq!(
            paper_unmounted_refusal(&order, &paper, None, &mounted, None),
            None,
            "its own mount"
        );
        let leg = [a_mount("okx", "BTC-USDT-SWAP", &[("bybit", "BTCUSDT")])];
        assert_eq!(
            paper_unmounted_refusal(&order, &paper, None, &leg, None),
            None,
            "a declared leg"
        );
        let source = Demo::running(true);
        assert_eq!(
            paper_unmounted_refusal(&order, &paper, Some(&source), &[], None),
            None,
            "a source"
        );
    }
    let order = limit("l-1", "bybit", "BTCUSDT", 1.0, 1.0);
    let demo = [engine("bybit", "BTCUSDT", AccountTier::Demo)];
    assert_eq!(paper_unmounted_refusal(&order, &demo, None, &[], None), None, "a demo account");
    let dukascopy = [engine("dukascopy", "EURUSD", AccountTier::Paper)];
    let order = limit("l-1", "dukascopy", "EURUSD", 1.0, 1.0);
    assert_eq!(
        paper_unmounted_refusal(&order, &dukascopy, None, &[], None),
        None,
        "the plan's refusal"
    );
    // An unmounted IG paper epic is refused too, though its book now takes the epic's stated unit (PAPER-IG):
    // no bar reaches it, so the order would rest silent.
    let ig = [engine("ig", "CS.D.EURUSD.MINI.IP", AccountTier::Paper)];
    let why = paper_unmounted_refusal(&market("ig", "CS.D.EURUSD.MINI.IP"), &ig, None, &[], None)
        .expect("an unmounted ig paper epic")
        .text;
    assert!(
        why.starts_with(
            "no price: no strategy mount trades `CS.D.EURUSD.MINI.IP` on ig on this node"
        ),
        "{why}"
    );
    // ...and the door holds the mount rows for an order, as for an un-latch.
    let rows = [a_mount("bybit", "BTCUSDT", &[])];
    assert_eq!(crate::publish::named_mounts(&order, &rows), rows.to_vec());
}

/// A paper combo of `venue` with one leg per symbol.
fn combo(venue: &str, legs: &[&str]) -> CommandWire {
    CommandWire::Combo(vike_tradehub_client::wire::OrderComboWire {
        venue: venue.into(),
        side: 1,
        qty: 1.0,
        legs: legs
            .iter()
            .map(|symbol| vike_model::OrderComboLeg {
                symbol: symbol.to_string(),
                ratio: 1,
                limit: None,
            })
            .collect(),
        net_limit: None,
        account: None,
        time_in_force: None,
        order_client_id: Some("combo-1".into()),
    })
}

/// **Never silent on a paper-only node: an order on a symbol its feeds do not reach is refused, naming it** (the
/// coordinator's follow-up of 2026-11-16). The feeds were wired at the start for the boot mounts' series, so a symbol
/// only a RUNTIME mount trades, or only a declared leg, gets no price, and an order on it would rest unfilled: it is
/// refused `no price: no price feed reaches …`, at the door and the Preview alike (both ask this function). The boot
/// mount's own symbol passes, and so does every order on a node that is not paper-only (`fed` is `None`). A COMBO is
/// judged on every leg, and on a venue the node does not price it is refused by the venue first
/// (`no_price_feed_refusal`).
#[test]
fn a_paper_only_node_refuses_an_order_its_feeds_do_not_reach() {
    use crate::server::control::{ControlLimits, ControlLimitsConfig, no_price_feed_refusal};
    use crate::server::symbol_admission::paper_unmounted_refusal;
    let mut paper = engine("bybit", "BTCUSDT", AccountTier::Paper);
    paper.extra_symbols = vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()].into();
    let paper = [paper];
    let boot = fed(&[("bybit", "BTCUSDT")]);
    let published =
        [a_mount("bybit", "BTCUSDT", &[("bybit", "SOLUSDT")]), a_mount("bybit", "ETHUSDT", &[])];
    let judged = |order: &CommandWire, fed: Option<&[vike_execution::SeriesKey]>| {
        paper_unmounted_refusal(order, &paper, None, &published, fed)
    };
    for (symbol, why) in [("ETHUSDT", "a runtime mount's"), ("SOLUSDT", "a declared leg's")] {
        for order in [market("bybit", symbol), limit("l-1", "bybit", symbol, 1.0, 1.0)] {
            let said = judged(&order, Some(&boot)).unwrap_or_else(|| panic!("{why} symbol")).text;
            assert!(
                said.starts_with(&format!(
                    "no price: no price feed reaches `{symbol}` on bybit on this node"
                )),
                "{said}"
            );
            assert_eq!(judged(&order, None), None, "{why}: a node whose feeds follow its mounts");
        }
    }
    assert_eq!(judged(&market("bybit", "BTCUSDT"), Some(&boot)), None, "the boot mount's symbol");

    let said =
        judged(&combo("bybit", &["BTCUSDT", "ETHUSDT"]), Some(&boot)).expect("a leg unfed").text;
    assert!(said.starts_with("no price: no price feed reaches `ETHUSDT` on bybit"), "{said}");
    assert_eq!(judged(&combo("bybit", &["BTCUSDT"]), Some(&boot)), None, "every leg reached");
    let unpriced = ControlLimits::new(ControlLimitsConfig::default())
        .with_identity(Some(identity(false, Some(&["binance"]))));
    let said = no_price_feed_refusal(&combo("bybit", &["BTCUSDT"]), &unpriced)
        .expect("bybit unpriced")
        .text;
    assert!(said.starts_with(&vike_tradehub_client::wire::no_price_feed("bybit")), "{said}");
    assert_eq!(
        crate::publish::named_mounts(&combo("bybit", &["BTCUSDT"]), &published).len(),
        2,
        "the door holds the mount rows for a combo too"
    );
}

/// **The refusal names the account the venue's stream logs in as** (the order check and the stream agree): ibkr's
/// stream is its DEFAULT account's IB Gateway session at demo, so the refusal names `ibkr` and says a labelled demo
/// account does not carry it; dukascopy's and fxcm's ride whatever session a running demo account holds.
#[test]
fn the_no_login_refusal_names_the_account_the_stream_logs_in_as() {
    let none = Demo::running(false);
    let ibkr = [engine("ibkr", "AAPL.SMART.USD", AccountTier::Paper)];
    let why = refused(plan_submit_symbol(
        &limit("l-1", "ibkr", "AAPL.SMART.USD", 1.0, 1.0),
        &ibkr,
        Some(&none),
        true,
    ));
    assert!(why.contains("DEFAULT account (`ibkr`) at the demo tier"), "{why}");
    assert!(why.contains("a labelled demo account does not carry this stream"), "{why}");
    for venue in ["dukascopy", "fxcm"] {
        let paper = [engine(venue, "EURUSD", AccountTier::Paper)];
        let order = limit("l-1", venue, "EURUSD", 1.0, 1.0);
        let why = refused(plan_submit_symbol(&order, &paper, Some(&none), true));
        assert!(why.contains(&format!("a running demo account of {venue} holds")), "{why}");
        assert_eq!(
            why,
            format!(
                "no price: no demo login for {venue}: a paper account of {venue} is priced and \
                 filled from the quotes of {venue}'s demo login, so this order would never fill. \
                 {venue}'s price stream rides the session a running demo account of {venue} \
                 holds, and no demo account of {venue} runs here. Arm one (`account.active = 1` \
                 on its `demo` row)"
            ),
            "any demo account's session, never the DEFAULT account's"
        );
    }
}

/// Every venue whose paper account no strategy mount trades is priced from its own symbol's HELD quote lane through
/// the venue's feed (the owner, 2026-11-16: "the same for ALL"), with a symbol its engine runs on. dukascopy, fxcm and
/// ibkr ride the demo login ([`VENUES`]); polymarket's engine is account-wide (no own symbol: every token is an
/// admission); alpaca has no quote lane.
const LANE_VENUES: [(&str, &str); 9] = [
    ("binance", "BTCUSDT"),
    ("bybit", "BTCUSDT"),
    ("okx", "BTC-USDT-SWAP"),
    ("aster", "BTCUSDT.P"),
    ("hyperliquid", "BTC"),
    ("deribit", "BTC-PERPETUAL"),
    ("oanda", "EUR_USD"),
    ("ctrader", "EURUSD"),
    ("ig", "CS.D.EURUSD.MINI.IP"),
];

/// **An unmounted paper account is never refused for want of a strategy mount: its own symbol's quote lane is held and
/// a manual market order waits for the CURRENT price** (the owner's example 3 and "the same for ALL", 2026-11-16), on
/// every venue of [`LANE_VENUES`], as on the three priced through the demo login: a committing market buy holds the
/// lane and waits for the first price, a limit holds it and passes, a Preview holds nothing. A demo account, a mounted
/// symbol and a node with no source are untouched.
#[test]
fn an_unmounted_paper_account_holds_its_quote_lane_and_waits_for_the_current_price_on_every_venue()
{
    for (venue, symbol) in LANE_VENUES {
        let unmounted = Demo::unmounted();
        let paper = [engine(venue, symbol, AccountTier::Paper)];
        match plan_submit_symbol(&market(venue, symbol), &paper, Some(&unmounted), true) {
            SymbolPlan::Place { symbol: placed, admit: None, wait: Some(_) } => {
                assert_eq!(placed, symbol, "{venue}");
            }
            other => {
                panic!("{venue}: an opening market order waits for its current price: {other:?}")
            }
        }
        let order = limit("l-1", venue, symbol, 1.0, 1.0);
        assert_eq!(plan_submit_symbol(&order, &paper, Some(&unmounted), true), SymbolPlan::Pass);
        assert_eq!(
            *unmounted.holds.lock().unwrap(),
            vec![format!("{venue}/{symbol}"), format!("{venue}/{symbol}")],
            "{venue}: each committed order holds the lane of the engine's own symbol"
        );
        assert_eq!(
            plan_submit_symbol(&market(venue, symbol), &paper, Some(&unmounted), false),
            SymbolPlan::Pass,
            "{venue}: a Preview"
        );
        assert_eq!(unmounted.holds.lock().unwrap().len(), 2, "{venue}: a Preview holds nothing");
        let mounted = Demo::running(true);
        assert_eq!(plan_submit_symbol(&order, &paper, Some(&mounted), true), SymbolPlan::Pass);
        assert_eq!(plan_submit_symbol(&order, &paper, None, true), SymbolPlan::Pass);
        let demo = [engine(venue, symbol, AccountTier::Demo)];
        assert_eq!(plan_submit_symbol(&order, &demo, Some(&unmounted), false), SymbolPlan::Pass);
    }
}

/// **Where no quote can reach an unmounted paper account, every order says why** (never a fill at a stale or a bar
/// price): alpaca's lane is one this node never opens on its own initiative (deferred by the owner), and a venue whose
/// price feed does not run here is named with the feed's own reason.
#[test]
fn an_unmounted_paper_account_no_quote_can_reach_refuses_naming_why() {
    let unmounted = Demo::unmounted();
    let alpaca = [engine("alpaca", "AAPL", AccountTier::Paper)];
    for commit in [true, false] {
        let why = refused(plan_submit_symbol(
            &market("alpaca", "AAPL"),
            &alpaca,
            Some(&unmounted),
            commit,
        ));
        assert!(
            why.starts_with("no price: no quote of `AAPL` on alpaca can reach this paper account (deferred by the owner"),
            "{why}"
        );
    }
    let feed_down = Demo {
        no_feed: Some("this node runs no price feed for ig".to_string()),
        ..Demo::unmounted()
    };
    let ig = [engine("ig", "CS.D.EURUSD.MINI.IP", AccountTier::Paper)];
    let why = refused(plan_submit_symbol(
        &market("ig", "CS.D.EURUSD.MINI.IP"),
        &ig,
        Some(&feed_down),
        true,
    ));
    assert!(
        why.starts_with(
            "no price: no quote of `CS.D.EURUSD.MINI.IP` on ig can reach this paper account (this node runs no price \
             feed for ig)"
        ),
        "{why}"
    );
    assert!(feed_down.holds.lock().unwrap().is_empty(), "a refused order holds no lane");
}
