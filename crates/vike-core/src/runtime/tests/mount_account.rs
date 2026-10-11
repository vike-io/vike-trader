//! **A strategy mount names its ACCOUNT, and its orders and reads go there** — the gate over the
//! last unaddressable half of multi-account support.
//!
//! # What was wrong
//!
//! Everything below the mount could already address a second account: labelled credentials, the
//! per-account ceilings, `vike_mount::make_engine_accounts`' fan-out, inbound routing by route key,
//! per-account reconcile, the GUI. A STRATEGY could not — `StrategyMount` carried
//! `(venue, symbol, interval)` and nothing else — so the extra engines were mounted, were reachable
//! INBOUND, and were unaddressable OUTBOUND. Every strategy lane resolved its engine with
//! `engine_idx_for_route_key(RouteKey::sole_account_of(<a venue string>))`, which by construction
//! can only ever answer with a venue's DEFAULT account.
//!
//! # The two halves, and why the second is the dangerous one
//!
//! * **WRITES.** [`CoreThread::apply_strategy_intent`] is the ONE choke point every strategy-minted
//!   intent lowers through, and it now passes `EngineRoute::Mount(idx)` rather than letting the
//!   payload's venue string decide. The account never becomes a string, so a misroute has no
//!   spelling.
//! * **READS.** `Broker::position`/`equity`/`multiplier`/`lot_size` resolve through the same
//!   `CoreThread::mount_engine` index. A mount that traded account `ALT` while SIZING against the
//!   default account's position would be a worse defect than the one the field fixes, and no
//!   order-routing test would catch it — so the read half is asserted here beside the write half,
//!   on the same core, in the same test.
//!
//! # ⚠ THE HEADLINE CONFIGURATION: two accounts, ONE symbol
//!
//! That is the spread the deleted symbol-collision rule refused (`vike_config::venue_accounts`), so
//! every test here mounts both accounts on [`SYMBOL`] — which also makes the assertions strictly
//! harder, since a first-match-by-`(venue, symbol)` lookup would find the WRONG engine rather than
//! none at all.
//!
//! White-box, in-crate: `mount_engine`, `route_of`, `apply_strategy_intent` and `eng` are private
//! to the runtime module and are exactly what is under test. `use super::*` re-exports it, the
//! sibling-test-module idiom of `crates/vike-core/src/runtime/tests/safe_state.rs` /
//! `crates/vike-core/src/runtime/tests/route_key.rs`.

use super::*;
use std::sync::Mutex as TestMutex;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, RiskGate};
use vike_model::RiskLimits;
use vike_model::accounts::account_keys::AccountLabel;

// The runtime's shared white-box assembly, `core_of(primary, extras, config)`.
use crate::runtime::test_support::core_of;

#[path = "mount_account/routing.rs"]
#[cfg(test)]
mod routing;

#[path = "mount_account/core_minted_orders.rs"]
#[cfg(test)]
mod core_minted_orders;

#[path = "mount_account/ambiguity_refusal.rs"]
#[cfg(test)]
mod ambiguity_refusal;

#[path = "mount_account/account_field.rs"]
#[cfg(test)]
mod account_field;

#[path = "mount_account/reducing_verbs.rs"]
#[cfg(test)]
mod reducing_verbs;

use account_field::open_order_for;
use ambiguity_refusal::{
    open_order, refusal_note, single_account_core, submitted_anywhere, ticket_core,
};
use core_minted_orders::{closes, crashing_bar, set_position};

/// The canonical exchange both accounts belong to. A roster id, so every capability table resolves
/// a real row for either engine.
const CANON: &str = "binance";
/// ⚠ ONE symbol for BOTH accounts — the spread configuration. See the module doc.
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The UNLABELLED account, NAMED. ⚠ Not the same thing as `None`, and the difference is the whole
/// of this file's two-engine fixtures: an ABSENT account is the mount naming none, which a
/// two-engine venue refuses as ambiguous; this is the mount naming the account the venue already
/// had, which routes to it at any engine count. `AccountLabel::parse` cannot produce it — `DEFAULT`
/// is reserved there precisely so an operator cannot spell it as a label — so it is the variant.
fn default_acct() -> AccountLabel {
    AccountLabel::Default
}

/// One engine on [`CANON`]/[`SYMBOL`] whose ROUTING key is `route_key` — the shape
/// `vike_mount::make_engine_for_account` produces (canonical venue everywhere, `route_key`
/// decorated).
fn engine(route_key: &str, seed: f64) -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(seed, CANON, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        CANON,
        SYMBOL,
    );
    e.route_key = route_key.to_string();
    e.collect_applied_fills = true;
    e
}

/// What each mount reported about ITS OWN book from inside one dispatch: `(label, position,
/// equity)`. A named alias because the shared cell is three types deep and the raw spelling says
/// nothing at the three sites that pass it.
type Seen = Arc<TestMutex<Vec<(&'static str, f64, f64)>>>;

/// A strategy that submits one tagged limit on `on_feed_status` and records what its broker told it
/// about its own book at that moment — the two halves (write, read) captured from inside ONE
/// dispatch, which is the only place they can be observed together.
struct Prober {
    label: &'static str,
    seen: Seen,
}

impl Strategy<LiveBroker> for Prober {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        self.seen.lock().unwrap().push((self.label, broker.position, broker.equity));
        broker.submit_limit_tagged("bid", 1, 1.0, 100.0);
    }
}

fn mount(
    label: &'static str,
    account: Option<AccountLabel>,
    controller_id: &str,
    seen: &Seen,
) -> StrategyMount {
    StrategyMount {
        account,
        symbols: Vec::new(),
        controller_id: Some(controller_id.to_string()),
        underlying_symbol: None,
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        strategy: Box::new(Prober { label, seen: Arc::clone(seen) }),
    }
}

/// The headline core: the DEFAULT account's engine plus `binance#ALT`, and two mounts on ONE
/// symbol — one on each account.
fn spread_core(seen: &Seen) -> CoreThread<RecordingClient> {
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(mount("default", Some(default_acct()), "m-default", seen)),
        extra_mounts: vec![mount("alt", Some(alt()), "m-alt", seen)],
        ..CoreConfig::default()
    };
    core_of(engine(CANON, 1_000.0), vec![(2_000.0, engine("binance#ALT", 2_000.0))], config)
}
