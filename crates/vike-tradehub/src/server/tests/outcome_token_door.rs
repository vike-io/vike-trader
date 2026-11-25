//! The control door's Polymarket row (decision 0174): an outcome TOKEN id is the symbol, a market NAME
//! (`slug:Outcome`, the market's address) is resolved to it by the facts source (the gatewayhub reads Gamma; this
//! node opens no venue socket), and the engine behind it is mounted on NO symbol, so every token is admitted first.
//!
//! The rows are `plan_submit_symbol` over a scripted facts source that RECORDS what it was asked, in order: a
//! name is resolved once and before anything else, a token id is never looked up by name, a refusal costs the
//! source no more than the rule needs, and no admission slot is spent on an order the venue's grid would reject.
//! The row's place in the table, the engine's fail-closed flag and the advertised capability are
//! `admits_any_symbol.rs`'s, which read the table itself.

use std::sync::Mutex;
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;

use vike_execution::{SymbolAdmission, VenueBlock};
use vike_model::{AccountTier, SymbolGrid};
use vike_tradehub_client::wire::{CommandWire, OrderRequestWire};

use super::admits_any_symbol::refused;
use crate::server::symbol_admission::{
    AccountReadiness, SymbolAdmitter, SymbolPlan, needs_lookup, plan_submit_symbol,
    submit_symbol_refusal,
};

/// A YES token and its sibling NO token, shaped like the venue's (77-digit uint256 decimals).
const YES: &str = "71321045679252212594626385532706912750332728571942532289631379312455583992563";
const NO: &str = "12345678901234567890123456789012345678901234567890123456789012345678901234567";

/// A facts source over one market: `rain-market:Yes` and `rain-market:No` name its two tokens. Records every
/// call that costs something, in order.
struct Gamma {
    calls: Mutex<Vec<String>>,
    /// The market's minimum size in shares (`orderMinSize`).
    min_qty: f64,
    /// Whether the account runs with an account-wide exposure ceiling.
    capped: bool,
}

impl Gamma {
    fn new() -> Self {
        Gamma { calls: Mutex::default(), min_qty: 5.0, capped: true }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls").clone()
    }
    fn note(&self, call: String) {
        self.calls.lock().expect("calls").push(call);
    }
}

impl SymbolAdmitter for Gamma {
    fn resolve(&self, _: &str, _: &str, typed: &str) -> Result<String, String> {
        self.note(format!("resolve:{typed}"));
        match typed {
            "rain-market:Yes" | "https://polymarket.com/event/rain/rain-market" => Ok(YES.into()),
            "rain-market:No" => Ok(NO.into()),
            other => Err(format!("polymarket: Gamma lists no market with the slug `{other}`")),
        }
    }
    fn admission(
        &self,
        route_key: &str,
        _: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        self.note(format!("admission:{symbol}"));
        Ok(SymbolAdmission {
            route_key: route_key.into(),
            symbol: symbol.into(),
            grid: SymbolGrid {
                tick_size: Some(0.001),
                lot_size: Some(0.01),
                min_qty: Some(self.min_qty),
                ..SymbolGrid::default()
            },
            contract_value: None,
            initial_mark: None,
            initial_mark_ms: None,
            quote_currency: None,
            quote_rate: None,
            rate_as_of_ms: None,
            initial_margin_requirement: None,
            release: false,
            margin_mode: None,
        })
    }
    fn price(
        &self,
        _: &str,
        _: &str,
        symbol: &str,
        subscribe: bool,
    ) -> Result<Option<f64>, String> {
        self.note(format!("price:{symbol}:{subscribe}"));
        Ok(Some(0.42))
    }
}

impl AccountReadiness for Gamma {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn exposure_cap_armed(&self, _: &str) -> bool {
        self.capped
    }
}

fn submit(symbol: &str, qty: f64, reduce_only: bool) -> CommandWire {
    CommandWire::Submit(OrderRequestWire {
        order_client_id: "c-1".into(),
        venue: "polymarket".into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(0.4),
        trigger_price: None,
        reduce_only,
        account: None,
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

/// Polymarket's published engine block: an account-wide mount, so it names no symbol.
fn engine(tier: AccountTier, admitted: &[&str]) -> VenueBlock {
    VenueBlock {
        venue: "polymarket".into(),
        route_key: "polymarket".into(),
        symbol: String::new(),
        extra_symbols: admitted.iter().map(|s| (*s).to_string()).collect::<Vec<_>>().into(),
        tier: Some(tier),
        ..Default::default()
    }
}

fn placed(plan: SymbolPlan) -> (String, Option<Box<SymbolAdmission>>, bool) {
    match plan {
        SymbolPlan::Place { symbol, admit, wait } => (symbol, admit, wait.is_some()),
        other => panic!("expected a placement, got {other:?}"),
    }
}

/// **A market NAME typed at the door is resolved first and admitted as its TOKEN**: the source is asked to
/// resolve the text, then for the facts of the id it returned, then for the id's price; the order goes out on the
/// id (so one position keeps one name), after an admission that carries the market's own grid and the price the
/// door saw as the first mark. The market's address is a spelling of the same name.
#[test]
fn a_market_name_is_resolved_and_admitted_as_its_token() {
    for typed in ["rain-market:Yes", "https://polymarket.com/event/rain/rain-market"] {
        let facts = Gamma::new();
        let block = engine(AccountTier::Demo, &[]);
        let (symbol, admit, waits) = placed(plan_submit_symbol(
            &submit(typed, 10.0, false),
            std::slice::from_ref(&block),
            Some(&facts),
            true,
        ));
        assert_eq!(symbol, YES, "{typed}: the order is placed on the token");
        let admit = admit.expect("admitted first");
        assert_eq!((admit.route_key.as_str(), admit.symbol.as_str()), ("polymarket", YES));
        assert_eq!(admit.grid.tick_size, Some(0.001), "the market's own tick");
        assert_eq!(admit.grid.lot_size, Some(0.01), "the share lot");
        assert_eq!(admit.grid.min_qty, Some(5.0), "the market's own minimum size");
        assert_eq!(admit.initial_mark, Some(0.42), "the price the door saw is the first mark");
        assert!(!waits, "a price was there");
        assert_eq!(
            facts.calls(),
            [format!("resolve:{typed}"), format!("admission:{YES}"), format!("price:{YES}:true")],
            "{typed}: resolved once, then the token's facts, then its price"
        );
    }
}

/// A token id names itself: the source is never asked to resolve it, and the plan is the same admission.
#[test]
fn a_token_id_is_never_resolved() {
    let facts = Gamma::new();
    let block = engine(AccountTier::Demo, &[]);
    let (symbol, admit, _) = placed(plan_submit_symbol(
        &submit(&format!("  {YES}\t"), 10.0, false),
        std::slice::from_ref(&block),
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, YES, "trimmed");
    assert!(admit.is_some());
    assert_eq!(facts.calls(), [format!("admission:{YES}"), format!("price:{YES}:true")]);
}

/// A Preview resolves and reads and subscribes nothing: its price ask does not subscribe.
#[test]
fn a_preview_resolves_and_reads_but_subscribes_nothing() {
    let facts = Gamma::new();
    let block = engine(AccountTier::Demo, &[]);
    let cmd = submit("rain-market:No", 10.0, false);
    assert_eq!(submit_symbol_refusal(&cmd, std::slice::from_ref(&block), Some(&facts)), None);
    assert_eq!(
        facts.calls(),
        [
            "resolve:rain-market:No".to_string(),
            format!("admission:{NO}"),
            format!("price:{NO}:false")
        ]
    );
}

/// **A name that resolves to a token the engine already trades is placed on it with no second admission**: one
/// position, one name, and no slot spent twice.
#[test]
fn a_name_that_resolves_to_an_admitted_token_is_not_admitted_again() {
    let facts = Gamma::new();
    let block = engine(AccountTier::Demo, &[YES]);
    let (symbol, admit, _) = placed(plan_submit_symbol(
        &submit("rain-market:Yes", 10.0, false),
        std::slice::from_ref(&block),
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, YES);
    assert!(admit.is_none(), "the engine trades it already");
    assert!(
        !facts.calls().iter().any(|c| c.starts_with("admission:")),
        "no lookup for a token the engine trades: {:?}",
        facts.calls()
    );
}

/// **An engine mounted on no symbol takes an order with no symbol untouched** (as the adapters read it), and
/// names no symbol in a refusal it has none to list.
#[test]
fn an_order_with_no_symbol_passes_untouched_and_asks_nothing() {
    let facts = Gamma::new();
    let block = engine(AccountTier::Demo, &[]);
    assert_eq!(
        plan_submit_symbol(
            &submit("", 10.0, false),
            std::slice::from_ref(&block),
            Some(&facts),
            true
        ),
        SymbolPlan::Pass
    );
    assert!(facts.calls().is_empty());
}

/// Every way the door says no, in words, and what each costs the source.
#[test]
fn the_refusals_name_their_cause_and_cost_the_source_no_more_than_the_rule_needs() {
    let demo = engine(AccountTier::Demo, &[]);

    // A name Gamma does not list: Gamma's own words, then nothing else is asked.
    let facts = Gamma::new();
    let why = refused(plan_submit_symbol(
        &submit("no-such-market:Yes", 10.0, false),
        std::slice::from_ref(&demo),
        Some(&facts),
        true,
    ));
    assert!(why.contains("Gamma lists no market with the slug `no-such-market:Yes`"), "{why}");
    assert!(why.contains("started on no symbol and has admitted none"), "{why}");
    assert!(why.contains("REFUSED"), "{why}");
    assert_eq!(facts.calls(), ["resolve:no-such-market:Yes"]);

    // A surface with no source cannot resolve a name; it says so, naming the spelling that works.
    let why = refused(plan_submit_symbol(
        &submit("rain-market:Yes", 10.0, false),
        std::slice::from_ref(&demo),
        None,
        true,
    ));
    assert!(
        why.contains("cannot look up a market name") && why.contains("20 or more digits"),
        "{why}"
    );

    // A paper account mounted on no token admits one like a demo account (the owner's ruling of 2026-11-16: the
    // same for ALL; its facts are the datahub's, `TradeLeaseRegistry::bind_keyless_facts`), and a surface with no
    // source still refuses it before any read.
    let facts = Gamma::new();
    let paper = engine(AccountTier::Paper, &[]);
    let (symbol, admit, _) = placed(plan_submit_symbol(
        &submit(YES, 10.0, false),
        std::slice::from_ref(&paper),
        Some(&facts),
        true,
    ));
    assert_eq!(symbol, YES);
    assert!(admit.is_some(), "admitted first");
    assert_eq!(facts.calls(), [format!("admission:{YES}"), format!("price:{YES}:true")]);
    let why = refused(plan_submit_symbol(
        &submit(YES, 10.0, false),
        std::slice::from_ref(&paper),
        None,
        true,
    ));
    assert!(why.contains("trades only the symbol it was started on"), "{why}");

    // A live account takes a new token only with an account-wide ceiling armed, before any read.
    let facts = Gamma { capped: false, ..Gamma::new() };
    let live = engine(AccountTier::Live, &[]);
    let why = refused(plan_submit_symbol(
        &submit("rain-market:Yes", 10.0, false),
        std::slice::from_ref(&live),
        Some(&facts),
        true,
    ));
    assert!(why.contains("LIVE account with no account-wide exposure ceiling"), "{why}");
    assert!(facts.calls().is_empty(), "{:?}", facts.calls());
    let facts = Gamma::new();
    placed(plan_submit_symbol(
        &submit("rain-market:Yes", 10.0, false),
        std::slice::from_ref(&live),
        Some(&facts),
        true,
    ));

    // An order under the market's minimum size never spends a slot.
    let facts = Gamma::new();
    let why = refused(plan_submit_symbol(
        &submit(YES, 2.0, false),
        std::slice::from_ref(&demo),
        Some(&facts),
        true,
    ));
    assert!(
        why.contains("below the symbol's minimum 5") && why.contains("no symbol slot was spent"),
        "{why}"
    );
    assert_eq!(facts.calls(), [format!("admission:{YES}")], "refused before any price is asked");

    // A reduce-only order can only close a position the engine holds: none in a token it never admitted.
    let facts = Gamma::new();
    let why = refused(plan_submit_symbol(
        &submit(YES, 10.0, true),
        std::slice::from_ref(&demo),
        Some(&facts),
        true,
    ));
    assert!(why.contains("a reduce-only order can only close a position"), "{why}");
}

/// A token the engine admitted is listed in a refusal for another symbol, with the mount (none) left out.
#[test]
fn a_refusal_lists_what_the_engine_admitted_and_never_an_empty_mount() {
    let facts = Gamma::new();
    let block = engine(AccountTier::Demo, &[YES]);
    let why = refused(plan_submit_symbol(
        &submit("no-such-market:Yes", 10.0, false),
        std::slice::from_ref(&block),
        Some(&facts),
        true,
    ));
    assert!(
        why.contains(&format!("engine trades `{YES}` only, not `no-such-market:Yes`")),
        "{why}"
    );
}

/// **A market order that names a price is refused at the door, by the venue's name, so a Preview says so too.** Every
/// order here is a signed limit at its price (`crates/bridges/polymarket/src/execution/mod.rs`'s `run` refuses it
/// after the `Ack`, and stays the backstop): it would rest or fill at that number while the node's ceilings size a
/// market order at the mark. A limit at that number and a market order with none pass.
#[test]
fn a_market_order_that_names_a_price_is_refused_by_the_venues_name() {
    let demo = engine(AccountTier::Demo, &[YES]);
    let with = |order_type: &str, price: Option<f64>, flag| {
        let CommandWire::Submit(mut req) = submit(YES, 10.0, flag) else { unreachable!() };
        (req.order_type, req.price) = (order_type.into(), price);
        CommandWire::Submit(req)
    };
    let facts = Gamma::new();
    for flag in [false, true] {
        let cmd = with("market", Some(0.5), flag);
        let why =
            refused(plan_submit_symbol(&cmd, std::slice::from_ref(&demo), Some(&facts), true));
        assert!(why.contains("polymarket") && why.contains("takes no price"), "{why}");
        assert!(why.contains("REFUSED"), "{why}");
        assert_eq!(
            crate::server::symbol_admission::submit_symbol_refusal(
                &cmd,
                std::slice::from_ref(&demo),
                Some(&facts)
            )
            .map(|r| r.text),
            Some(why),
            "a Preview says what the send says"
        );
    }
    for cmd in [with("limit", Some(0.5), false), with("market", None, false)] {
        assert!(
            !matches!(
                plan_submit_symbol(&cmd, std::slice::from_ref(&demo), Some(&facts), false),
                SymbolPlan::Refuse(_)
            ),
            "{cmd:?}"
        );
    }
}

/// **The Preview's rate token and the notional ceiling's sizing ask the same question the plan does**: a foreign
/// token of an engine mounted on no symbol needs a lookup; a token it trades, and an order with no symbol, do not.
#[test]
fn a_foreign_token_of_an_engine_mounted_on_no_symbol_needs_a_lookup() {
    let none = engine(AccountTier::Demo, &[]);
    let trades = engine(AccountTier::Demo, &[YES]);
    assert!(needs_lookup(&submit(YES, 10.0, false), std::slice::from_ref(&none)));
    assert!(needs_lookup(&submit("rain-market:Yes", 10.0, false), std::slice::from_ref(&none)));
    assert!(!needs_lookup(&submit(YES, 10.0, false), std::slice::from_ref(&trades)));
    assert!(!needs_lookup(&submit("", 10.0, false), std::slice::from_ref(&none)));
}
