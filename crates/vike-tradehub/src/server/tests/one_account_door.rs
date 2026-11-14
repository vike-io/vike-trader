//! **A venue-scoped command that names no account reaches exactly ONE account, or it does not go** (the
//! owner's ruling of 2026-11-07, decision 0254). The table is the CI box's four shapes: one account (deribit), one
//! account with a lane twin (binance), a default and a labelled account (hyperliquid), two labelled accounts
//! (bybit). Each verb's raw frame meets the door, so a client that skips every client-side check meets it too.

use super::*;
use crate::server::control::lower_command;
use crate::server::refusal::one_account_addressed;
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;
use vike_tradehub_client::wire::OrderBracketWire;

/// A venue's published shape: its route keys and what each is mounted on, and the books a refusal lists.
pub(super) struct Shape {
    pub(super) venue: &'static str,
    engines: &'static [(&'static str, &'static str)],
    refuses_with: &'static [&'static str],
}

pub(super) const SHAPES: [Shape; 4] = [
    Shape { venue: "deribit", engines: &[("deribit", "BTC-PERPETUAL")], refuses_with: &[] },
    Shape {
        venue: "binance",
        engines: &[("binance", "BTCUSDT"), ("binance#PERP", "BTCUSDT.P")],
        refuses_with: &[],
    },
    Shape {
        venue: "hyperliquid",
        engines: &[("hyperliquid", "BTC"), ("hyperliquid#LIVE", "BTC")],
        refuses_with: &["hyperliquid/DEFAULT", "hyperliquid/LIVE"],
    },
    Shape {
        venue: "bybit",
        engines: &[("bybit#DEMO", "BTCUSDT"), ("bybit#LIVE", "BTCUSDT")],
        refuses_with: &["bybit/DEMO", "bybit/LIVE"],
    },
];

impl Shape {
    fn route_keys(&self) -> Vec<String> {
        self.engines.iter().map(|(key, _)| (*key).to_string()).collect()
    }

    pub(super) fn blocks(&self) -> Vec<vike_execution::VenueBlock> {
        self.engines.iter().map(|(key, symbol)| engine(key, symbol)).collect()
    }
}

fn mount(venue: &str, account: Option<&str>) -> CommandWire {
    CommandWire::MountStrategy {
        venue: venue.to_string(),
        account: account.map(str::to_string),
        symbol: "BTCUSDT".to_string(),
        interval: "1m".to_string(),
        controller_id: None,
        file: Some("market_making/spread_maker.rs".to_string()),
        name: None,
        rhai: None,
        params: serde_json::json!({}),
    }
}

/// An opening order list: a buy limit under the market and a buy stop over it, the first to fill cancels the other.
fn breakout_oco(venue: &str, account: Option<&str>) -> CommandWire {
    use vike_model::ContingencyType;
    use vike_tradehub_client::wire::{OrderListLegWire, OrderListWire};
    let leg = |order_type: &str, price, trigger_price, linked: usize| OrderListLegWire {
        side: 1,
        qty: 1.0,
        order_type: order_type.to_string(),
        price,
        trigger_price,
        trail: None,
        reduce_only: false,
        time_in_force: None,
        parent: None,
        contingency: Some(ContingencyType::Oco),
        linked: vec![linked],
        order_client_id: Some(format!("bo{linked}")),
    };
    CommandWire::OrderList(OrderListWire {
        venue: venue.to_string(),
        symbol: "BTCUSDT".to_string(),
        legs: vec![leg("limit", Some(90.0), None, 1), leg("stop", None, Some(110.0), 0)],
        account: account.map(str::to_string),
    })
}

/// An opening combo: a call spread priced by its net, two legs traded as one order.
fn call_spread(venue: &str, account: Option<&str>) -> CommandWire {
    use vike_model::OrderComboLeg;
    use vike_tradehub_client::wire::OrderComboWire;
    let leg =
        |symbol: &str, ratio| OrderComboLeg { symbol: symbol.to_string(), ratio, limit: None };
    CommandWire::Combo(OrderComboWire {
        venue: venue.to_string(),
        side: 1,
        qty: 1.0,
        legs: vec![leg("BTC-27DEC26-100000-C", 1), leg("BTC-27DEC26-110000-C", -1)],
        net_limit: Some(0.03),
        account: account.map(str::to_string),
        time_in_force: None,
        order_client_id: Some("cs1".into()),
    })
}

/// Every venue-scoped verb the rule covers, as the frame a client sends, naming `account`.
pub(super) fn frames(venue: &str, account: Option<&str>) -> Vec<(&'static str, CommandWire)> {
    let account_text = account.map(str::to_string);
    vec![
        ("the order", submit(venue, "BTCUSDT", account)),
        (
            "the bracket",
            CommandWire::Bracket(OrderBracketWire {
                order_client_id: None,
                venue: venue.to_string(),
                symbol: "BTCUSDT".to_string(),
                side: 1,
                qty: 1.0,
                entry_price: Some(100.0),
                stop_loss: 90.0,
                take_profit: 110.0,
                account: account_text.clone(),
            }),
        ),
        ("the mount", mount(venue, account)),
        ("the order list", breakout_oco(venue, account)),
        ("the combo", call_spread(venue, account)),
        (
            "the flatten",
            CommandWire::Flatten {
                venue: venue.to_string(),
                symbol: "BTCUSDT".to_string(),
                account: account_text.clone(),
            },
        ),
        (
            "the mass-cancel",
            CommandWire::MassCancel {
                venue: Some(venue.to_string()),
                symbol: None,
                account: account_text.clone(),
            },
        ),
        (
            "the market exit",
            CommandWire::MarketExit { venue: Some(venue.to_string()), account: account_text },
        ),
    ]
}

/// The two verbs that act on ONE account's engine, each as the bytes a client sends with no account.
pub(super) fn one_engine_frames(venue: &str) -> Vec<(&'static str, CommandWire)> {
    let raw = |frame: serde_json::Value| serde_json::from_value(frame).expect("the frame decodes");
    vec![
        (
            "the trading-state change",
            raw(
                serde_json::json!({ "SetAccountTradingState": { "venue": venue, "state": "Halted" } }),
            ),
        ),
        (
            "the give-back",
            raw(serde_json::json!({ "UnadmitSymbol": { "venue": venue, "symbol": "ETHUSDT" } })),
        ),
    ]
}

/// The door's one judgement, with what the node lists NOT RUNNING beside what it runs.
fn judged(
    cmd: CommandWire,
    keys: &[String],
    not_running: &[vike_execution::ArmingNotRunningView],
    blocks: &[vike_execution::VenueBlock],
    facts: Option<&dyn SymbolAdmitter>,
) -> Result<CommandWire, String> {
    one_account_addressed(cmd, keys, not_running, blocks, facts)
}

fn stopped(venue: &str, route_key: &str) -> vike_execution::ArmingNotRunningView {
    vike_execution::ArmingNotRunningView {
        account: vike_model::accounts::account_keys::AccountNamed::from_route_key(venue, route_key),
        tier: Some(AccountTier::Demo),
        reason: "account_inactive".to_string(),
        block: None,
    }
}

/// **A stopped account is still an account.** bybit has DEMO and LIVE, and with DEMO stopped a bare `bybit` reached
/// LIVE: stopping one account changed what the same words mean, and the money went to the other. Whatever the verb.
#[test]
fn a_stopped_account_still_counts_so_a_bare_venue_cannot_slide_onto_the_other_one() {
    let keys = ["bybit#LIVE".to_string()];
    let blocks = [engine("bybit#LIVE", "BTCUSDT")];
    let not_running = [stopped("bybit", "bybit#DEMO")];
    for (subject, cmd) in frames("bybit", None).into_iter().chain(one_engine_frames("bybit")) {
        let why = judged(cmd, &keys, &not_running, &blocks, None).expect_err(subject);
        assert!(why.contains("bybit/DEMO") && why.contains("bybit/LIVE"), "{subject}: {why}");
    }
}

/// A source that says only which engines are the derived lane twins.
struct DerivedTwin(&'static str);

impl SymbolAdmitter for DerivedTwin {
    fn admission(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<vike_execution::SymbolAdmission, SymbolPropertiesRequestError> {
        Err(SymbolPropertiesRequestError::Refused("no facts here".to_string()))
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(None)
    }
}

impl AccountReadiness for DerivedTwin {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn derived_lane_twin(&self, route_key: &str) -> bool {
        route_key == self.0
    }
}

/// **A derived lane twin is no second account, whatever symbol a profile mounted it on**: `account_symbols_for` keeps
/// a `binance`/`PERP` mount on `ETHUSDT.P`, which is off the twin table's symbol, and the watcher's word says what the
/// shape cannot.
#[test]
fn a_derived_twin_mounted_on_any_symbol_is_no_second_account() {
    let keys = ["binance".to_string(), "binance#PERP".to_string()];
    let blocks = [engine("binance", "BTCUSDT"), engine("binance#PERP", "ETHUSDT.P")];
    let facts = DerivedTwin("binance#PERP");
    for (subject, cmd) in frames("binance", None) {
        let answer = judged(cmd.clone(), &keys, &[], &blocks, Some(&facts));
        assert_eq!(answer, Ok(cmd), "{subject}");
    }
}

/// **`trade account halt|reduce|resume|remove-symbol` follow the same rule**: a bare venue goes on the shapes of
/// one account and is refused on the shapes of several, listing the books, because naming the default account there
/// halted one and read as halting the venue. Before the first publish nothing can back "one account", so it waits.
#[test]
fn a_bare_venue_on_a_verb_of_one_engine_follows_the_same_rule() {
    for shape in &SHAPES {
        for (subject, cmd) in one_engine_frames(shape.venue) {
            let answer = judged(cmd.clone(), &shape.route_keys(), &[], &shape.blocks(), None);
            if shape.refuses_with.is_empty() {
                assert_eq!(answer, Ok(cmd), "{}: {subject} is left as it came", shape.venue);
            } else {
                let why = answer.expect_err(subject);
                assert!(why.contains(&format!("{subject} names none of them")), "{why}");
                for book in shape.refuses_with {
                    assert!(why.contains(book), "{}: {book} is listed: {why}", shape.venue);
                }
            }
        }
    }
    for (subject, cmd) in one_engine_frames("hyperliquid") {
        let why = judged(cmd, &[], &[], &[], None).expect_err(subject);
        assert!(why.contains("has not published its accounts yet"), "{why}");
    }
}

/// **The one account being LABELLED, the verb is written out to it** (the default account's route key would be
/// refused as an account the node does not run); the default account stays bare, and lowers to the bare venue.
#[test]
fn a_verb_of_one_engine_is_written_out_to_the_one_labelled_account_and_lowers_to_the_default() {
    let blocks = [engine("okx#DEMO", "BTC-USDT-SWAP"), engine("bybit", "BTCUSDT")];
    let keys = ["okx#DEMO".to_string(), "bybit".to_string()];
    for (subject, cmd) in one_engine_frames("okx") {
        let addressed = judged(cmd, &keys, &[], &blocks, None).expect(subject);
        let frame = serde_json::to_value(&addressed).expect("a frame is JSON");
        let body = frame.as_object().and_then(|o| o.values().next()).expect("one variant");
        assert_eq!(body["account"], "DEMO", "{subject}: {frame}");
    }
    let (state_change, give_back) = {
        let mut frames = one_engine_frames("bybit").into_iter().map(|(_, cmd)| cmd);
        (frames.next().expect("the state"), frames.next().expect("the give-back"))
    };
    let (lowered, _) = lower_command(state_change).expect("the state change lowers");
    assert!(
        matches!(&lowered, vike_execution::Command::SetAccountTradingState { route_key, .. } if route_key == "bybit"),
        "{lowered:?}"
    );
    let (lowered, _) = lower_command(give_back).expect("the give-back lowers");
    assert!(
        matches!(&lowered, vike_execution::Command::AdmitSymbol(a) if a.route_key == "bybit"),
        "{lowered:?}"
    );
}

/// **A bare venue passes on the two shapes of one account, as it came, and is refused on the two of several,
/// naming the books to choose from.** The twin's shape keeps the account-less frame: that is what reaches
/// both lanes.
#[test]
fn a_bare_venue_is_accepted_only_where_the_venue_runs_one_account() {
    for shape in &SHAPES {
        for (subject, cmd) in frames(shape.venue, None) {
            let answer = judged(cmd.clone(), &shape.route_keys(), &[], &shape.blocks(), None);
            if shape.refuses_with.is_empty() {
                assert_eq!(answer, Ok(cmd), "{}: {subject} is left as it came", shape.venue);
            } else {
                let why = answer.expect_err(subject);
                assert!(why.contains(&format!("{subject} names none of them")), "{why}");
                assert!(why.contains("REFUSED and nothing was sent"), "{why}");
                for book in shape.refuses_with {
                    assert!(why.contains(book), "{}: {book} is listed: {why}", shape.venue);
                }
            }
        }
    }
}

/// **A named book is never this gate's**, `DEFAULT` included, on any shape (the routing gates judge it next),
/// and neither is what names no venue or names an order: the node-wide panic button stays the panic button.
#[test]
fn a_named_book_the_panic_button_and_an_order_id_pass_on_every_shape() {
    for shape in &SHAPES {
        for account in ["DEFAULT", "LIVE", "DEMO"] {
            for (subject, cmd) in frames(shape.venue, Some(account)) {
                let answer = judged(cmd.clone(), &shape.route_keys(), &[], &shape.blocks(), None);
                assert_eq!(answer, Ok(cmd), "{}/{account}: {subject}", shape.venue);
            }
        }
        for cmd in [
            CommandWire::MarketExit { venue: None, account: None },
            CommandWire::MassCancel { venue: None, symbol: None, account: None },
            CommandWire::Cancel("c-1".into()),
            CommandWire::Modify {
                order_client_id: "c-1".into(),
                new_qty: None,
                new_price: None,
                new_trigger_price: None,
            },
        ] {
            let answer = judged(cmd.clone(), &shape.route_keys(), &[], &shape.blocks(), None);
            assert_eq!(answer, Ok(cmd), "{}", shape.venue);
        }
    }
}

/// **One LABELLED account is one account**: a bare venue is accepted, and an opening command is written out to
/// it, because the core would hand an account-less order to engine 0, possibly another exchange. A reducing verb
/// stays account-less: its fan-out over the venue's engines reaches that one.
#[test]
fn the_one_labelled_account_of_a_venue_is_named_on_an_opening_command() {
    let blocks = [engine("okx#DEMO", "BTC-USDT-SWAP"), engine("bybit", "BTCUSDT")];
    let keys = ["okx#DEMO".to_string(), "bybit".to_string()];
    for (subject, cmd) in frames("okx", None) {
        let addressed = judged(cmd.clone(), &keys, &[], &blocks, None).expect(subject);
        let named = match &addressed {
            CommandWire::Submit(req) => req.account.as_deref(),
            CommandWire::Bracket(b) => b.account.as_deref(),
            CommandWire::OrderList(l) => l.account.as_deref(),
            CommandWire::Combo(c) => c.account.as_deref(),
            CommandWire::MountStrategy { account, .. } => account.as_deref(),
            _ => {
                assert_eq!(addressed, cmd, "{subject} stays account-less");
                continue;
            }
        };
        assert_eq!(named, Some("DEMO"), "{subject}");
    }
}

/// **The twin is told from an account by what it IS, not by the label**: an operator's own account spelling the
/// twin's label and mounted elsewhere is a second account, and so is a `live` engine of the twin's shape the
/// node has not said it derived. Each bare verb then refuses and lists both books.
#[test]
fn an_account_under_the_twins_label_is_a_second_account() {
    let live = |block: vike_execution::VenueBlock| vike_execution::VenueBlock {
        tier: Some(AccountTier::Live),
        ..block
    };
    let keys = ["binance".to_string(), "binance#PERP".to_string()];
    for blocks in [
        vec![engine("binance", "BTCUSDT"), engine("binance#PERP", "ETHUSDT")],
        vec![live(engine("binance", "BTCUSDT")), live(engine("binance#PERP", "BTCUSDT.P"))],
    ] {
        for (subject, cmd) in frames("binance", None) {
            let why = judged(cmd, &keys, &[], &blocks, None).expect_err(subject);
            assert!(why.contains("binance/DEFAULT") && why.contains("binance/PERP"), "{why}");
        }
    }
}

/// A venue no engine serves and no row names passes, for the venue gate to answer in its own words.
#[test]
fn a_venue_nothing_serves_is_the_venue_gates() {
    let blocks = [engine("hyperliquid", "BTC")];
    let running = ["hyperliquid".to_string()];
    for (subject, cmd) in frames("okx", None) {
        assert_eq!(judged(cmd.clone(), &running, &[], &blocks, None), Ok(cmd), "{subject}");
    }
}

/// A source that says only what the account rows configure.
struct Rows(Result<Vec<String>, String>);

impl SymbolAdmitter for Rows {
    fn admission(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<vike_execution::SymbolAdmission, SymbolPropertiesRequestError> {
        Err(SymbolPropertiesRequestError::Refused("no facts here".to_string()))
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(None)
    }
}

impl AccountReadiness for Rows {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn configured_accounts(&self, _: &str) -> Option<Result<Vec<String>, String>> {
        Some(self.0.clone())
    }
}

/// **The rows are the count, running or not**: a row nothing ever started still makes a second account, a lane twin
/// is no row, and rows that cannot be read refuse a bare venue, because "one account" is then a claim nothing backs.
#[test]
fn the_rows_are_the_count_and_rows_that_cannot_be_read_refuse_a_bare_venue() {
    let verbs = |venue: &str| frames(venue, None).into_iter().chain(one_engine_frames(venue));
    let keys = ["bybit#LIVE".to_string()];
    let blocks = [engine("bybit#LIVE", "BTCUSDT")];
    let never_started = Rows(Ok(vec!["bybit#DEMO".to_string(), "bybit#LIVE".to_string()]));
    for (subject, cmd) in verbs("bybit") {
        let why = judged(cmd, &keys, &[], &blocks, Some(&never_started)).expect_err(subject);
        assert!(why.contains("bybit/DEMO") && why.contains("bybit/LIVE"), "{subject}: {why}");
    }

    let keys = ["binance".to_string(), "binance#PERP".to_string()];
    let blocks = [engine("binance", "BTCUSDT"), engine("binance#PERP", "BTCUSDT.P")];
    let one_row = Rows(Ok(vec!["binance".to_string()]));
    for (subject, cmd) in verbs("binance") {
        let answer = judged(cmd.clone(), &keys, &[], &blocks, Some(&one_row));
        assert_eq!(answer, Ok(cmd), "{subject}");
    }

    let unreadable = Rows(Err("the settings database is locked".to_string()));
    for (subject, cmd) in verbs("binance") {
        let why = judged(cmd, &keys, &[], &blocks, Some(&unreadable)).expect_err(subject);
        assert!(why.contains("cannot read its account rows") && why.contains(subject), "{why}");
    }
    let named =
        CommandWire::MarketExit { venue: Some("binance".into()), account: Some("PERP".into()) };
    let answer = judged(named.clone(), &keys, &[], &blocks, Some(&unreadable));
    assert_eq!(answer, Ok(named), "a named book asks nothing of the rows");

    // The one account configured is the one the words mean, running or not: its own reason answers next.
    let stopped_alone = Rows(Ok(vec!["okx#DEMO".to_string()]));
    let addressed =
        judged(submit("okx", "BTC-USDT-SWAP", None), &[], &[], &[], Some(&stopped_alone));
    let CommandWire::Submit(req) = addressed.expect("an order waits on no roster") else {
        panic!("an order stays an order");
    };
    assert_eq!(req.account, None, "before the first publish nothing is written");
    let keys = ["bybit".to_string()];
    let blocks = [engine("bybit", "BTCUSDT")];
    let addressed =
        judged(submit("okx", "BTC-USDT-SWAP", None), &keys, &[], &blocks, Some(&stopped_alone));
    let CommandWire::Submit(req) = addressed.expect("one account") else {
        panic!("an order stays an order");
    };
    assert_eq!(
        req.account.as_deref(),
        Some("DEMO"),
        "named, so the NOT RUNNING gate answers for it"
    );
}

/// **A roster not published yet**: a bare reducing verb is refused, since the fan-out is what the ruling forbids
/// and "one account" is a claim an empty roster cannot back; an opening command passes under the venue gate's
/// deadlock argument (a refused command never makes the core publish), and the core refuses it on several engines.
#[test]
fn before_the_first_publish_a_bare_reducing_verb_is_refused_and_an_order_is_not() {
    for (subject, cmd) in frames("hyperliquid", None) {
        let answer = judged(cmd.clone(), &[], &[], &[], None);
        if matches!(
            subject,
            "the order" | "the bracket" | "the order list" | "the combo" | "the mount"
        ) {
            assert_eq!(answer, Ok(cmd), "{subject}");
        } else {
            let why = answer.expect_err(subject);
            assert!(why.contains("has not published its accounts yet"), "{why}");
            assert!(why.contains(subject), "{why}");
        }
    }
}

/// **The raw frame, through the ONE acceptance path, into a real two-account core**: nothing reaches the venue
/// and no leg is minted, whichever verb it is; the same verb naming its book goes.
#[test]
fn a_raw_account_less_frame_is_refused_by_name_at_the_door_of_a_two_account_node() {
    let mut node = Node::start_with("hyperliquid", "BTC", AccountTier::Demo, &["LIVE"]);
    for (subject, cmd) in
        frames("hyperliquid", None).into_iter().chain(one_engine_frames("hyperliquid"))
    {
        let why = node.send_rostered(cmd, None).expect_err(subject);
        assert!(why.contains(&format!("{subject} names none of them")), "{why}");
        assert!(why.contains("hyperliquid/DEFAULT") && why.contains("hyperliquid/LIVE"), "{why}");
    }
    assert!(node.submitted.lock().unwrap().is_empty(), "a refusal sends the venue nothing");
    node.send_rostered(
        CommandWire::MarketExit { venue: Some("hyperliquid".into()), account: Some("LIVE".into()) },
        None,
    )
    .expect("the book it names");
    node.send_rostered(CommandWire::MarketExit { venue: None, account: None }, None)
        .expect("the node-wide panic button");
    node.core.shutdown_and_join();
}

/// **A bare `close-all binance` reaches BOTH lanes of the one account.** It stays account-less at the door, so
/// the core's fan-out reads the spot engine's position and the twin's, and each gets its own reduce-only leg.
/// (A `binance/DEFAULT` would stop at the spot engine and lose the perpetual.)
#[test]
fn a_bare_market_exit_of_one_account_with_a_twin_closes_both_lanes() {
    let facts = FirstTick::new(false);
    let mut node = Node::start_lane_twin();
    node.tick("BTCUSDT.P", 74_299.9, 74_300.1);
    facts.tick(74_300.0);
    for (order_client_id, symbol) in [("spot-1", "BTCUSDT"), ("perp-1", "BTCUSDT.P")] {
        node.send_rostered(
            limit(order_client_id, "binance", symbol, 0.002, 74_288.4),
            Some(&facts),
        )
        .expect("the bare order is addressed to its lane");
        node.venue_saw(order_client_id);
    }
    let holds = |key: &str, symbol: &str| {
        node.blocks().iter().any(|b| {
            b.route_key == key && b.positions.iter().any(|p| p.symbol == *symbol && p.size != 0.0)
        })
    };
    wait_until("both lanes to hold their fill", || {
        holds("binance", "BTCUSDT") && holds("binance#PERP", "BTCUSDT.P")
    });

    node.send_rostered(
        CommandWire::MarketExit { venue: Some("binance".into()), account: None },
        Some(&facts),
    )
    .expect("one account, two lanes: a bare venue is accepted");
    let closed = |symbol: &str| {
        node.submitted.lock().unwrap().iter().any(|o| o.reduce_only && o.symbol == symbol)
    };
    wait_until("a reduce-only leg on each lane", || closed("BTCUSDT") && closed("BTCUSDT.P"));
    node.core.shutdown_and_join();
}
