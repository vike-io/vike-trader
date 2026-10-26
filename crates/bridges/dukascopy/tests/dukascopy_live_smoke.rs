//! LIVE Dukascopy JForex smoke (network + demo creds + built sidecar jar — run manually):
//!
//! ```text
//! cargo test -p vike-dukascopy --test dukascopy_live_smoke -- --ignored --nocapture dukascopy_jforex_login
//! cargo test -p vike-dukascopy --test dukascopy_live_smoke -- --ignored --nocapture dukascopy_jforex_ladder
//! ```
//!
//! Requirements: the sidecar jar installed at `<project>/bin/jforex/jforex-bridge.jar` (a RELEASE
//! ASSET, not a committed file: `scripts/fetch_release_tools.sh` installs it there, and
//! `crates/bridges/dukascopy/scripts/provision-jforex.sh` also supplies a JRE at
//! `<project>/bin/jre/`), a JVM from that directory or `JAVA_HOME` or `PATH`, and
//! the smoke account's `LOGIN` / `PASSWORD` fields in the credential store — the default demo
//! account, or the demo account whose LABEL `DUKASCOPY_SMOKE_ACCOUNT` names (absent creds → the
//! test self-skips, the standard live gate).
//!
//! ⚠ **This file is the COMPOSITION ROOT for the two runtime tool paths of the sidecars it
//! spawns** — the gatewayhub starts the venue in production (decision 0168), but it resolves the
//! tools from the directories ITS root resolved, never from this process's environment, so no daemon
//! resolves them for these tests. The env sweep, the project walk and
//! the `resolve_dukascopy_tools` call therefore happen HERE, in [`live_tools`] — the same call
//! `crates/bridges/dukascopy/src/adapter.rs`'s `DukascopyVenueAdapter` makes over the directories its
//! root resolved: the library takes the answer as a parameter and resolves nothing.
//!
//! Run the two tests separately — each spawns its own sidecar login and Dukascopy may
//! reject concurrent sessions on one demo account.
//!
//! The ladder is weekend-tolerant: FX is closed Fri 22:00 → Sun 22:00 GMT, and a
//! market order then comes back venue-rejected. That still proves the full
//! submit→sidecar→venue→event round trip, so the test logs it and passes; the
//! fill/net-close/cancel legs only run against an open market.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use vike_dukascopy::quotes::QuoteBus;
use vike_dukascopy::{
    DukascopyConfig, DukascopyExecutionClient, DukascopyTools, load_dukascopy_config_from,
    resolve_dukascopy_tools,
};
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::AccountTier;
use vike_model::OrderRequest;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;

/// The smoke account's login from the credential store (`smoke_account`, resolved through
/// `vike_secrets::smoke_directory` at the demo tier); `None` (skip) when absent — the live gate.
/// `server` is force-blanked: the `.env` SERVER var holds the web-platform login URL,
/// not a JNLP — the client's built-in demo JNLP default is the correct connect target.
/// (The client now also self-defends: a non-`.jnlp` server value is warned about and
/// replaced by the default demo JNLP; blanking here just keeps the smoke explicit.)
fn live_config() -> Option<DukascopyConfig> {
    // DUKASCOPY_SMOKE_ACCOUNT=<LABEL> switches to that demo account (differential testing: a
    // second account has its own login counter and server backend).
    let directory = vike_secrets::smoke_directory();
    let credentials = directory.credentials_for("dukascopy", &smoke_account(), AccountTier::Demo);
    // No settings: the server is blanked just below, so this smoke reads no venue setting.
    let mut cfg = load_dukascopy_config_from(
        &credentials,
        &vike_secrets::venue_setting::VenueSettings::default(),
    )?;
    cfg.server = String::new();
    Some(cfg)
}

/// The account LABEL `DUKASCOPY_SMOKE_ACCOUNT` names — the default (unlabelled) account when the
/// variable is unset or blank.
///
/// # Panics
/// When the variable names no legal label (`AccountLabel::parse`'s refusal): a smoke that cannot
/// tell which account it is meant to log into must not log into another.
fn smoke_account() -> AccountLabel {
    match std::env::var("DUKASCOPY_SMOKE_ACCOUNT") {
        Ok(text) if !text.trim().is_empty() => AccountLabel::parse(text.trim())
            .unwrap_or_else(|e| panic!("DUKASCOPY_SMOKE_ACCOUNT: {e}")),
        _ => AccountLabel::Default,
    }
}

/// `$VIKE_SETTINGS_DIR`, the ONE fact this root pulls out of the process environment by name.
///
/// Spent twice, deliberately: on BOTH project walks below (`bin/` and `settings/state`, which this
/// variable moves independently of each other); the credential store above reads the same variable
/// inside `vike_secrets::smoke_directory`. Reading it once and threading it is what keeps those
/// answers from disagreeing — the split
/// `vike_paths::state_path::project_bin_dir_from` exists to close, and the one a unit file that
/// relocates the project would otherwise hit.
fn settings_dir_override() -> Option<String> {
    std::env::var("VIKE_SETTINGS_DIR").ok()
}

/// The composition root's one impure act: sweep the environment, walk for the project, and hand
/// `resolve_dukascopy_tools` both. See this file's module doc for why it lives in a test.
///
/// `crates/bridges/dukascopy/src/adapter.rs`'s `DukascopyVenueAdapter` makes the same call over what
/// its root resolved — the deployment's named values plus the boot's one walk — which is the
/// point: the resolution is the CALLER's, and the library takes the result.
fn live_tools() -> DukascopyTools {
    let env: HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let override_dir = settings_dir_override();
    let bin = vike_paths::state_path::project_bin_dir_from(override_dir.as_deref(), &cwd);
    // The STATE directory in its own right — `$VIKE_SETTINGS_DIR` moves it without moving `bin/`,
    // which is why it is a second resolver call rather than a derivation from `bin`. It is what
    // the sidecar's JVM `user.home` is built from.
    let state = vike_paths::state_path::project_state_dir_from(override_dir.as_deref(), &cwd);
    let tools = resolve_dukascopy_tools(&env, bin.as_deref(), state.as_deref());
    tracing::info!(
        target: "vike_dukascopy",
        jar = %tools.bridge_jar.display(),
        java = %tools.java,
        jforex_home = ?tools.jforex_home,
        "resolved sidecar tools"
    );
    tools
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

fn order(coid: &str, side: i32, order_type: &str, price: Option<f64>) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        account: None,
        combo_legs: Vec::new(),
        client_order_id: coid.into(),
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        side,
        qty: 1000.0, // Dukascopy FX minimum
        order_type: vike_model::OrderType::from_stored(order_type),
        price,
        trigger_price: None,
        reduce_only: false,
        time_in_force: Default::default(),
        gtd_expiry: None,
        ts: now_ms(),
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}

/// Next event off the ingest lane within `secs` (venue round trips are seconds, not ms).
fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, secs: u64) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let ingest = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(secs), rx.recv()).await })
        .expect("timed out waiting for venue event")
        .expect("ingest channel closed");
    match ingest {
        Ingest::Event(ev) => ev,
        other => panic!("expected Ingest::Event, got {other:?}"),
    }
}

/// LIVE: real JForex login on the smoke's demo account — spawn returns Ok only after the
/// sidecar's `ready` handshake (account id + balance logged on stderr). Proves jar, java, JNLP
/// and creds.
#[test]
#[ignore = "live: network + demo creds + <project>/bin/jforex/jforex-bridge.jar"]
fn dukascopy_jforex_login() {
    vike_log::test_init();
    let Some(cfg) = live_config() else {
        tracing::warn!(target: "vike_dukascopy", "dukascopy live smoke: no demo account creds in the store — skipping");
        return;
    };
    let (events, _rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn(cfg, &live_tools(), events)
        .expect("live JForex login (ready)");
    tracing::info!(target: "vike_dukascopy", "LOGIN OK — JForex session established; detaching");
    client.detach();
}

/// LIVE ladder: market → fill → net-close → resting limit → cancel. Weekend-tolerant:
/// a market-closed rejection ends the test early (round trip still proven).
#[test]
#[ignore = "live: network + demo creds + <project>/bin/jforex/jforex-bridge.jar; full fills need an open market"]
fn dukascopy_jforex_ladder() {
    vike_log::test_init();
    let Some(cfg) = live_config() else {
        tracing::warn!(target: "vike_dukascopy", "dukascopy live smoke: no demo account creds in the store — skipping");
        return;
    };
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn(cfg, &live_tools(), events)
        .expect("live JForex login (ready)");
    tracing::info!(target: "vike_dukascopy", "LOGIN OK — starting ladder");

    // Leg 1: market BUY 1000 EURUSD.
    let coid1 = format!("smk{}", now_ms());
    client.submit(&order(&coid1, 1, "market", None));
    match recv_event(&mut rx, 30) {
        Event::OrderSubmitted(e) => assert_eq!(e.client_order_id, coid1),
        other => panic!("expected OrderSubmitted, got {other:?}"),
    }
    match recv_event(&mut rx, 60) {
        Event::OrderRejected(e) => {
            // Weekend / closed market: the venue answered — full round trip proven.
            tracing::warn!(target: "vike_dukascopy", "VENUE REJECTED (acceptable when market closed): {}", e.reason);
            client.detach();
            return;
        }
        Event::OrderAccepted(e) => {
            assert_eq!(e.client_order_id, coid1);
            tracing::info!(target: "vike_dukascopy", "ACCEPTED venue_order_id={:?}", e.venue_order_id);
        }
        other => panic!("expected OrderAccepted or OrderRejected, got {other:?}"),
    }
    // Dual-publish contract (Proto.orderFilled, proven by the dukascopy_exec.rs fake): a
    // fill emits BOTH the bare FillEvent — which the core Account folds into position/PnL —
    // FIRST, then the wrapping OrderFilled the order FSM applies, both carrying the same
    // fill. The bare Fill is therefore the event that arrives here, before the wrap.
    let fill = match recv_event(&mut rx, 60) {
        Event::Fill(fill) => {
            assert_eq!(fill.client_order_id, coid1);
            assert_eq!(fill.venue, "dukascopy");
            assert_eq!(fill.symbol, "EURUSD");
            fill
        }
        other => panic!("expected bare Fill, got {other:?}"),
    };
    match recv_event(&mut rx, 60) {
        Event::OrderFilled(e) => assert_eq!(e.client_order_id, coid1),
        other => panic!("expected OrderFilled wrap, got {other:?}"),
    }
    tracing::info!(target: "vike_dukascopy", "FILLED {} @ {}", fill.last_qty, fill.last_px);

    // Leg 2: opposite market SELL nets the position flat (bridge close emulation).
    // Contract: synthetic OrderAccepted(venue_order_id="net:<order>") first, then
    // OrderPartiallyFilled per non-final close leg, and exactly ONE terminal
    // OrderFilled. NOTE the netting may close a STALE opposing position left by an
    // earlier crashed run before this leg's — that's correct netting behavior.
    let coid2 = format!("smk{}", now_ms());
    client.submit(&order(&coid2, -1, "market", None));
    loop {
        match recv_event(&mut rx, 60) {
            Event::OrderSubmitted(_) => continue,
            Event::OrderAccepted(e) => {
                assert_eq!(e.client_order_id, coid2, "net-accept under submitting coid");
                tracing::info!(target: "vike_dukascopy", "NET-ACCEPTED {:?}", e.venue_order_id);
                continue;
            }
            Event::Fill(f) => {
                // Dual-publish: the bare FillEvent precedes each partial/terminal wrap.
                assert_eq!(f.client_order_id, coid2, "net-close fill under submitting coid");
                tracing::info!(target: "vike_dukascopy", "NET-CLOSE FILL {} @ {}", f.last_qty, f.last_px);
                continue;
            }
            Event::OrderPartiallyFilled(e) => {
                assert_eq!(e.client_order_id, coid2);
                tracing::info!(target: "vike_dukascopy", "NET-CLOSE LEG {} @ {}", e.fill.last_qty, e.fill.last_px);
                continue;
            }
            Event::OrderFilled(e) => {
                assert_eq!(e.client_order_id, coid2, "close fill under submitting coid");
                tracing::info!(target: "vike_dukascopy", "NET-CLOSED {} @ {}", e.fill.last_qty, e.fill.last_px);
                break;
            }
            other => panic!("expected net-close lifecycle event, got {other:?}"),
        }
    }

    // Leg 3: resting limit far below market, then cancel.
    let coid3 = format!("smk{}", now_ms());
    client.submit(&order(&coid3, 1, "limit", Some(0.9000)));
    loop {
        match recv_event(&mut rx, 60) {
            Event::OrderSubmitted(_) => continue,
            Event::OrderAccepted(e) => {
                assert_eq!(e.client_order_id, coid3);
                tracing::info!(target: "vike_dukascopy", "LIMIT RESTING — canceling");
                break;
            }
            other => panic!("expected limit OrderAccepted, got {other:?}"),
        }
    }
    client.cancel(&coid3);
    match recv_event(&mut rx, 60) {
        Event::OrderCanceled(e) => assert_eq!(e.client_order_id, coid3),
        other => panic!("expected OrderCanceled, got {other:?}"),
    }
    tracing::info!(target: "vike_dukascopy", "LADDER GREEN: login -> market fill -> net close -> limit rest -> cancel");
    client.detach();
}

/// The word `dukascopy_jforex_metal_unit_far_limit` needs in `DUKASCOPY_METAL_PROBE` before it places anything.
const METAL_PROBE_TOKEN: &str = "place-one-far-xauusd-limit-on-the-dukascopy-demo-account";

/// The sidecar's smallest order (`OrderMap.MIN_UNITS`): the amount the metal probe places, and cannot go under.
const METAL_PROBE_UNITS: f64 = 1000.0;

/// The most notional (USD) the probe will place an order for unless `DUKASCOPY_METAL_MAX_NOTIONAL_USD` says more.
const METAL_MAX_NOTIONAL_DEFAULT_USD: f64 = 5000.0;

/// How far under the live ask the limit must sit: a BUY limit at or above `ask * METAL_NOT_MARKETABLE_BELOW` could fill.
const METAL_NOT_MARKETABLE_BELOW: f64 = 0.95;

/// `DUKASCOPY_METAL_MAX_NOTIONAL_USD` read (`None`: unset): a positive finite number, else the default.
fn metal_max_notional(raw: Option<&str>) -> f64 {
    raw.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|cap| cap.is_finite() && *cap > 0.0)
        .unwrap_or(METAL_MAX_NOTIONAL_DEFAULT_USD)
}

/// The `DUKASCOPY_METAL_MAX_NOTIONAL_USD` value that admits an order of `notional` USD with 5% headroom for the ask
/// moving before the next run, rounded UP to a whole dollar (multiplied by 105 before the division by 100: the
/// product of a round notional is exact, so no `...0000001` rounds the figure an operator types up a dollar).
fn metal_cap_that_admits(notional: f64) -> f64 {
    (notional * 105.0 / 100.0).ceil()
}

/// **The limit price the metal probe may rest, or why it must not be placed at all.** `reference` is the
/// operator's public XAU/USD price (typed by hand, so it is never trusted on its own), `live` the sidecar's own
/// `(bid, ask)` of the instrument just now, `units` the amount and `max_notional_usd` the cap.
///
/// * the live quote must be two-sided, or nothing can be checked and nothing is placed;
/// * the notional is `units * ask`, the WORST case (a unit read as one ounce, the vendor's documented unit for a
///   metal, so the figure is the notional), and above the cap nothing is placed: the refusal says which variable to
///   raise and to what value ([`metal_cap_that_admits`]), the default is never raised for the operator;
/// * the limit is 30% under `reference`, on XAU/USD's 0.001 grid and never a multiple of ten ticks, and it must sit
///   under `ask * 0.95`: a mistyped reference (or a fast market) that makes it marketable would FILL a real demo
///   order, so it refuses.
fn metal_plan(
    reference: f64,
    (bid, ask): (f64, f64),
    units: f64,
    max_notional_usd: f64,
) -> Result<f64, String> {
    if !(bid.is_finite() && ask.is_finite() && bid > 0.0 && ask >= bid) {
        return Err(format!(
            "the live XAUUSD quote ({bid}/{ask}) is not two-sided: nothing can be checked"
        ));
    }
    let notional = units * ask;
    if notional > max_notional_usd {
        return Err(format!(
            "{units} units ({units} oz) at the live ask {ask} are {notional:.0} USD, over \
             DUKASCOPY_METAL_MAX_NOTIONAL_USD={max_notional_usd}. To place this order and accept that figure on the \
             demo account, run again with DUKASCOPY_METAL_MAX_NOTIONAL_USD={} (the notional at this ask plus 5% for \
             the ask moving); the default {METAL_MAX_NOTIONAL_DEFAULT_USD} is never raised for you",
            metal_cap_that_admits(notional)
        ));
    }
    let ticks = ((reference * 0.7) / 0.001).floor() as i64;
    let price = (ticks - i64::from(ticks % 10 == 0)) as f64 / 1000.0; // `/ 1000`, not `* 0.001`: the nearest double of the decimal
    if price >= ask * METAL_NOT_MARKETABLE_BELOW {
        return Err(format!(
            "the limit {price} (70% of DUKASCOPY_METAL_REF_PX={reference}) is not under 95% of the live ask {ask}: it \
             could FILL. Check DUKASCOPY_METAL_REF_PX against the platform"
        ));
    }
    Ok(price)
}

/// Next event off the ingest lane within `secs`, or `None`: it never panics (it runs in a `Drop` during unwinding).
fn try_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, secs: u64) -> Option<Event> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().ok()?;
    match rt.block_on(async { tokio::time::timeout(Duration::from_secs(secs), rx.recv()).await }) {
        Ok(Some(Ingest::Event(ev))) => Some(ev),
        _ => None,
    }
}

/// **The probe's order never outlives the test.** From the moment the order is submitted the guard owns the client
/// and the lane; on drop (a normal end, a failed assertion, any panic) it cancels the order if the venue may still
/// hold it, waits for the cancel's answer, says LOUDLY and by order id if it could not confirm one, and detaches.
struct MetalProbe {
    client: Option<DukascopyExecutionClient>,
    rx: tokio::sync::mpsc::Receiver<Ingest>,
    coid: String,
    /// True from the submit until the venue's answer says nothing rests (a rejection, or the cancel's).
    may_rest: bool,
}

impl MetalProbe {
    fn client(&mut self) -> &mut DukascopyExecutionClient {
        self.client.as_mut().expect("the client lives until the guard drops")
    }
}

impl Drop for MetalProbe {
    fn drop(&mut self) {
        let Some(mut client) = self.client.take() else { return };
        if self.may_rest {
            client.cancel(&self.coid);
            let mut canceled = false;
            while let Some(event) = try_event(&mut self.rx, 30) {
                if matches!(&event, Event::OrderCanceled(e) if e.client_order_id == self.coid) {
                    canceled = true;
                    break;
                }
            }
            if canceled {
                eprintln!("METAL PROBE: the order {} was canceled on the way out", self.coid);
            } else {
                eprintln!(
                    "\n!!! METAL PROBE: COULD NOT CONFIRM THE CANCEL of order {0} - IT MAY STILL REST ON THE DEMO \
                     ACCOUNT. CANCEL IT BY HAND in the Dukascopy platform (its label starts `v{0}`) !!!\n",
                    self.coid
                );
                tracing::error!(target: "vike_dukascopy", coid = %self.coid, "METAL PROBE: cancel NOT confirmed - cancel by hand");
            }
        }
        client.detach();
    }
}

/// LIVE, the METAL AMOUNT PLAN (multi-currency money, decision 0183's Part 2; the script is
/// `n080-DK2-duka-metal-unit.sh` in the owner's kit): ONE far limit BUY of the sidecar's smallest amount
/// (1000 units) on XAU/USD, the instrument the door refuses by name until a demo order proves a metal path
/// (`crates/bridges/dukascopy/src/any_symbol.rs`'s `instrument_facts`: "a metal's amount is in ounces"). The unit
/// IS the vendor's ounce (order-amounts wiki: minimum 1 oz of XAU/USD, 50 oz of XAG/USD), so 1000 units is 1000 oz
/// and the 1000 floor is the sidecar's own; what no one has seen is the order on this account. It is a smoke
/// because the door CANNOT place it: the node refuses a metal before the sidecar is asked, and that refusal is the
/// thing being protected.
///
/// It reads the instrument's LIVE quote off the sidecar first and places nothing unless (`metal_plan`) the quote is
/// two-sided, the worst-case notional (1000 units x the live ask, a unit read as one ounce) is within
/// `DUKASCOPY_METAL_MAX_NOTIONAL_USD` (default 5000: at today's gold price that REFUSES until the operator raises
/// it, knowingly: the refusal names the value to set) and the limit sits under 95% of the live ask. Then it places the order a long way under the
/// market (30% under `DUKASCOPY_METAL_REF_PX`: it must not fill), shows the venue's words (the `OrderAccepted`, or
/// the `OrderRejected` reason, verbatim), holds the order for `DUKASCOPY_METAL_HOLD_SECS` (default 90, 15..600) so
/// the owner can read its AMOUNT and its unit in the Dukascopy platform, cancels it and detaches. The sidecar's
/// own stderr (inherited here) carries the venue-stated trade terms of XAU/USD
/// (`bridge: venue terms XAU/USD ... min_amount=.. amount_increment=.. amount_per_contract=..`, a jar of the
/// release that logs them) and the amount the venue booked (`bridge: venue figures [submit-ok] ... requested_m=..`).
///
/// The order is owned by a drop guard (`MetalProbe`): a failed assertion or any panic cancels it on the way out and,
/// if the cancel is not confirmed, prints the order id loudly.
///
/// Self-skips without the token and without demo credentials; a missing `DUKASCOPY_METAL_REF_PX` (the public
/// XAU/USD price) or any refusal of `metal_plan` FAILS the test, so a run that placed nothing is never read as one
/// that did. ⚠ The login is the SMOKE ACCOUNT's, and ONE JForex login per account: run it only while no
/// gatewayhub holds that account's sidecar (`account.active` off, which the script does and restores).
#[test]
#[ignore = "live: network + demo creds + the jar + DUKASCOPY_METAL_PROBE + DUKASCOPY_METAL_REF_PX; PLACES A REAL DEMO ORDER"]
fn dukascopy_jforex_metal_unit_far_limit() {
    vike_log::test_init();
    if std::env::var("DUKASCOPY_METAL_PROBE").as_deref() != Ok(METAL_PROBE_TOKEN) {
        tracing::warn!(target: "vike_dukascopy", "metal probe: DUKASCOPY_METAL_PROBE is not the token — placing nothing");
        return;
    }
    let reference: f64 = std::env::var("DUKASCOPY_METAL_REF_PX")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|p: &f64| p.is_finite() && *p > 0.0)
        .expect("DUKASCOPY_METAL_REF_PX: the public XAU/USD price, a positive number");
    let hold: u64 = std::env::var("DUKASCOPY_METAL_HOLD_SECS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|s| (15..=600).contains(s))
        .unwrap_or(90);
    let max_notional =
        metal_max_notional(std::env::var("DUKASCOPY_METAL_MAX_NOTIONAL_USD").ok().as_deref());
    let Some(cfg) = live_config() else {
        tracing::warn!(target: "vike_dukascopy", "metal probe: no demo account creds in the store — skipping");
        return;
    };
    let bus = QuoteBus::new();
    let (events, rx) = event_channel(64);
    let client =
        DukascopyExecutionClient::spawn_quoting(cfg, &live_tools(), events, Arc::clone(&bus))
            .expect("live JForex login (ready)");
    let mut probe = MetalProbe { client: Some(client), rx, coid: String::new(), may_rest: false };
    // The live quote of the instrument, off the sidecar's own price lane: nothing is placed without one.
    let live = bus
        .fresh_tick("XAUUSD", Duration::from_secs(30))
        .unwrap_or_else(|why| panic!("METAL PROBE: no live XAUUSD quote, nothing placed: {why}"));
    tracing::info!(
        target: "vike_dukascopy",
        "METAL PROBE: live XAUUSD {}/{}, reference {reference}, {METAL_PROBE_UNITS} units = up to {:.0} USD (cap {max_notional})",
        live.bid, live.ask, METAL_PROBE_UNITS * live.ask
    );
    let price = metal_plan(reference, (live.bid, live.ask), METAL_PROBE_UNITS, max_notional)
        .unwrap_or_else(|why| panic!("METAL PROBE REFUSED, nothing placed: {why}"));
    let coid = format!("smk{}", now_ms());
    let mut req = order(&coid, 1, "limit", Some(price));
    req.symbol = "XAUUSD".into();
    req.qty = METAL_PROBE_UNITS;
    tracing::info!(target: "vike_dukascopy", "METAL PROBE: BUY {} XAUUSD @ {price} (reference {reference}, 30% under)", req.qty);
    probe.coid.clone_from(&coid);
    probe.may_rest = true; // from the submit on, the venue may hold it
    probe.client().submit(&req);
    let accepted = loop {
        match recv_event(&mut probe.rx, 60) {
            Event::OrderSubmitted(_) => continue,
            Event::OrderAccepted(e) => {
                tracing::info!(target: "vike_dukascopy", "METAL PROBE: ACCEPTED by the venue, venue_order_id={:?}", e.venue_order_id);
                break true;
            }
            Event::OrderRejected(e) => {
                tracing::warn!(target: "vike_dukascopy", "METAL PROBE: REJECTED by the venue, in its own words: {}", e.reason);
                probe.may_rest = false;
                break false;
            }
            other => panic!("expected the venue's answer to the metal order, got {other:?}"),
        }
    };
    if accepted {
        tracing::info!(
            target: "vike_dukascopy",
            "METAL PROBE: holding the order {hold} s: READ ITS AMOUNT AND UNIT in the Dukascopy platform now (label starts `v{coid}`)"
        );
        std::thread::sleep(Duration::from_secs(hold));
        probe.client().cancel(&coid);
        match recv_event(&mut probe.rx, 60) {
            Event::OrderCanceled(e) => assert_eq!(e.client_order_id, coid),
            other => panic!(
                "expected OrderCanceled, got {other:?} - the guard cancels {coid} again on the way out"
            ),
        }
        probe.may_rest = false;
        tracing::info!(target: "vike_dukascopy", "METAL PROBE: canceled");
    }
}

/// The metal probe's refusals, with no login: they are what keeps a mistyped reference or a large unit from becoming
/// a real demo order.
#[test]
fn the_metal_probe_places_nothing_unless_the_live_quote_vouches_for_it() {
    let live = (2400.0, 2400.5);
    // within a raised cap and far under the market: the limit is 70% of the reference on the 0.001 grid, off a ten-tick multiple
    let price = metal_plan(2400.0, live, 1000.0, 3_000_000.0).expect("a far, capped limit");
    assert_eq!(price, 1679.999);
    assert!((price * 1000.0).round() as i64 % 10 != 0);
    // the default cap refuses 1000 oz of gold (2,400,500 USD at this ask), naming the variable AND the value that
    // admits it: 2,400,500 plus 5% = 2,520,525
    let why =
        metal_plan(2400.0, live, 1000.0, METAL_MAX_NOTIONAL_DEFAULT_USD).expect_err("over the cap");
    assert!(why.contains("2400500 USD"), "{why}");
    assert!(why.contains("DUKASCOPY_METAL_MAX_NOTIONAL_USD=5000"), "the cap in force: {why}");
    assert!(why.contains("run again with DUKASCOPY_METAL_MAX_NOTIONAL_USD=2520525 "), "{why}");
    assert!(why.contains("never raised for you"), "{why}");
    // the value it names admits that very order, and one dollar less does not
    let named = metal_cap_that_admits(1000.0 * live.1);
    assert_eq!(named, 2_520_525.0);
    assert!(metal_plan(2400.0, live, 1000.0, named).is_ok());
    assert!(metal_plan(2400.0, live, 1000.0, 2_400_499.0).is_err());
    // a reference typed ten times too high would make the limit marketable: refused, not placed
    let why = metal_plan(24_000.0, live, 1000.0, 1e9).expect_err("marketable");
    assert!(why.contains("could FILL"), "{why}");
    // a limit just under the live ask is refused too (95% of 2400.5 = 2280.475)
    assert!(
        metal_plan(3300.0, live, 1000.0, 1e9).is_err(),
        "70% of 3300 = 2310 is above 95% of the ask"
    );
    assert!(metal_plan(3200.0, live, 1000.0, 1e9).is_ok(), "70% of 3200 = 2240 is below it");
    // no two-sided live quote, no placement
    for quote in
        [(0.0, 2400.5), (2400.0, f64::NAN), (2401.0, 2400.0), (f64::INFINITY, f64::INFINITY)]
    {
        assert!(metal_plan(2400.0, quote, 1000.0, 1e9).is_err(), "{quote:?}");
    }
}

/// The notional cap reads a positive number and falls back to the default for anything else.
#[test]
fn the_metal_probe_cap_is_a_positive_number_or_the_default() {
    assert_eq!(metal_max_notional(None), 5000.0);
    assert_eq!(metal_max_notional(Some(" 3000000 ")), 3_000_000.0);
    for bad in ["", "abc", "-5", "0", "NaN", "inf"] {
        assert_eq!(metal_max_notional(Some(bad)), 5000.0, "{bad:?}");
    }
}

// ---------------------------------------------------------------------------------------------------
// The native stop-loss (`StrategyBridge.java`'s `handleStop`), LIVE on the demo account: ONE USDJPY 1000.
// ---------------------------------------------------------------------------------------------------

/// The word the two stop-loss probes need in `DUKASCOPY_STOP_PROBE` before they place anything.
const STOP_PROBE_TOKEN: &str = "open-and-stop-one-usdjpy-1000-on-the-dukascopy-demo-account";

/// USDJPY's pip (`Instrument.USDJPY.getPipValue()`): a price sits on a tenth of it, 0.001.
const USDJPY_PIP: f64 = 0.01;

fn usdjpy_order(coid: &str, side: i32, order_type: &str) -> OrderRequest {
    let mut req = order(coid, side, order_type, None);
    req.symbol = "USDJPY".into();
    req
}

/// A reduce-only stop: the stop-loss of the open orders that face `side`.
fn usdjpy_stop(coid: &str, side: i32, trigger: f64) -> OrderRequest {
    let mut req = usdjpy_order(coid, side, "stop");
    req.trigger_price = Some(trigger);
    req.reduce_only = true;
    req
}

/// The demo account the stop probes trade, by its NUMBER in the `account` table (`DUKASCOPY_SMOKE_ACCOUNT_ID`).
/// `credentials_for` reads only an ACTIVE row, and one JForex login per account means the probe runs while the
/// row is switched off (`secrets account deactivate --id N`), so the book is read by id here, whatever the
/// row's flag. The credential values never leave the config.
fn live_config_by_id() -> Option<DukascopyConfig> {
    let id: i64 = std::env::var("DUKASCOPY_SMOKE_ACCOUNT_ID").ok()?.trim().parse().ok()?;
    let directory = vike_secrets::smoke_directory();
    let Some(Ok(book)) = directory.credentials() else { return None };
    let credentials = book.view("dukascopy", Some(id));
    let mut cfg = load_dukascopy_config_from(
        &credentials,
        &vike_secrets::venue_setting::VenueSettings::default(),
    )?;
    cfg.server = String::new();
    Some(cfg)
}

fn stop_probe_say(text: &str) {
    println!("STOP PROBE {}: {text}", now_ms());
}

/// `raw` (an environment value, `None` when unset) as a number in `range`, else `default`.
fn probe_number(raw: Option<&str>, default: f64, range: std::ops::RangeInclusive<f64>) -> f64 {
    raw.and_then(|v| v.trim().parse().ok()).filter(|v: &f64| range.contains(v)).unwrap_or(default)
}

/// `DUKASCOPY_STOP_PIPS`: how far under the bid the probe's stop-loss goes.
fn stop_pips(default: f64, range: std::ops::RangeInclusive<f64>) -> f64 {
    probe_number(std::env::var("DUKASCOPY_STOP_PIPS").ok().as_deref(), default, range)
}

/// `DUKASCOPY_STOP_WAIT_SECS`: how long the probe waits for the venue to fire its stop-loss.
fn stop_wait_secs(default: f64, range: std::ops::RangeInclusive<f64>) -> f64 {
    probe_number(std::env::var("DUKASCOPY_STOP_WAIT_SECS").ok().as_deref(), default, range)
}

/// **The probe's position never outlives the test.** From the first submit the guard owns the client and the
/// lane. `unwind` (a normal end, a failed assertion, any panic) cancels a stop that may still rest, then flattens
/// a position it may still hold, and says LOUDLY if it could not confirm either.
struct StopProbe {
    client: Option<DukascopyExecutionClient>,
    rx: tokio::sync::mpsc::Receiver<Ingest>,
    /// True from the entry's fill until a stop-loss fill or the flatten's fill says the long is gone.
    long_open: bool,
    /// The stop that may still hold the stop-loss.
    stop_coid: Option<String>,
}

impl StopProbe {
    fn client(&mut self) -> &mut DukascopyExecutionClient {
        self.client.as_mut().expect("the client lives until the guard drops")
    }

    /// The next event within `secs`, printed (the venue's own words, verbatim).
    fn next(&mut self, secs: u64) -> Option<Event> {
        let event = try_event(&mut self.rx, secs)?;
        stop_probe_say(&format!("{event:?}"));
        Some(event)
    }

    /// Submit `req` and wait for its first answer: the order's venue id when accepted, else the venue's reason.
    fn submit_and_await_accept(&mut self, req: &OrderRequest, secs: u64) -> Result<String, String> {
        self.client().submit(req);
        let until = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < until {
            match self.next(5) {
                Some(Event::OrderAccepted(e)) if e.client_order_id == req.client_order_id => {
                    return Ok(e.venue_order_id.map(|v| v.to_string()).unwrap_or_default());
                }
                Some(Event::OrderRejected(e)) if e.client_order_id == req.client_order_id => {
                    return Err(e.reason.to_string());
                }
                _ => {}
            }
        }
        Err("no answer within the wait".into())
    }

    /// Wait for `coid`'s terminal fill (`OrderFilled`): its price; a rejection or a cancel is the `Err`.
    fn await_filled(&mut self, coid: &str, secs: u64) -> Result<f64, String> {
        let until = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < until {
            match self.next(5) {
                Some(Event::OrderFilled(e)) if e.client_order_id == coid => {
                    return Ok(e.fill.last_px);
                }
                Some(Event::OrderRejected(e)) if e.client_order_id == coid => {
                    return Err(format!("rejected: {}", e.reason));
                }
                Some(Event::OrderCanceled(e)) if e.client_order_id == coid => {
                    return Err(format!("canceled: {}", e.reason));
                }
                _ => {}
            }
        }
        Err("no fill within the wait".into())
    }

    fn unwind(&mut self) {
        if self.client.is_none() {
            return;
        }
        if let Some(stop) = self.stop_coid.take() {
            self.client().cancel(&stop);
            while let Some(event) = self.next(30) {
                match event {
                    Event::OrderCanceled(e) if e.client_order_id == stop => break,
                    Event::OrderFilled(e) if e.client_order_id == stop => {
                        self.long_open = false; // the stop-loss fired first: the long is gone
                        break;
                    }
                    _ => {}
                }
            }
        }
        if self.long_open {
            let coid = format!("stp{}", now_ms());
            self.client().submit(&usdjpy_order(&coid, -1, "market"));
            match self.await_filled(&coid, 90) {
                Ok(px) => {
                    self.long_open = false;
                    stop_probe_say(&format!("flattened at {px}"));
                }
                Err(why) => eprintln!(
                    "\n!!! STOP PROBE: COULD NOT FLATTEN ({why}) - 1000 USDJPY LONG MAY REMAIN ON THE DEMO ACCOUNT: close it by hand !!!\n"
                ),
            }
        }
    }

    /// The venue's own answer to "is anything open?": a reduce-only stop on each side. Both must be REFUSED as
    /// "no open ... order" - an accepted one means a position exists (its stop-loss is taken off again).
    fn assert_flat(&mut self) {
        for (side, trigger) in [(-1, 100.0), (1, 300.0)] {
            let coid = format!("stp{}", now_ms());
            match self.submit_and_await_accept(&usdjpy_stop(&coid, side, trigger), 60) {
                Err(why) if why.contains("no open") && !why.contains("already carry") => {}
                other => {
                    if other.is_ok() {
                        self.client().cancel(&coid);
                        let _ = self.next(10);
                    }
                    panic!("STOP PROBE: NOT FLAT (side {side}): {other:?}");
                }
            }
        }
        stop_probe_say("FLAT: the venue refuses a stop on either side, there is no open order");
    }
}

impl Drop for StopProbe {
    fn drop(&mut self) {
        self.unwind();
        if let Some(mut client) = self.client.take() {
            client.detach();
        }
    }
}

/// Spawn the probe's sidecar on the smoke account and give it the guard. Self-skips (`None`) without the token or
/// the account's credentials.
fn stop_probe_start() -> Option<(StopProbe, Arc<QuoteBus>)> {
    vike_log::test_init();
    if std::env::var("DUKASCOPY_STOP_PROBE").as_deref() != Ok(STOP_PROBE_TOKEN) {
        tracing::warn!(target: "vike_dukascopy", "stop probe: DUKASCOPY_STOP_PROBE is not the token - placing nothing");
        return None;
    }
    let Some(cfg) = live_config_by_id() else {
        tracing::warn!(target: "vike_dukascopy", "stop probe: no DUKASCOPY_SMOKE_ACCOUNT_ID / no credentials - skipping");
        return None;
    };
    let bus = QuoteBus::new();
    let (events, rx) = event_channel(256);
    let client =
        DukascopyExecutionClient::spawn_quoting(cfg, &live_tools(), events, Arc::clone(&bus))
            .expect("live JForex login (ready)");
    Some((StopProbe { client: Some(client), rx, long_open: false, stop_coid: None }, bus))
}

/// Buy 1000 USDJPY at market and put a reduce-only stop `pips` under the bid. Returns the stop's coid and trigger.
fn stop_probe_open_and_stop(probe: &mut StopProbe, bus: &QuoteBus, pips: f64) -> (String, f64) {
    let entry = format!("stp{}", now_ms());
    probe.client().submit(&usdjpy_order(&entry, 1, "market"));
    let entry_px = probe.await_filled(&entry, 90).unwrap_or_else(|why| {
        panic!("STOP PROBE: the entry did not fill ({why}): is the market open?")
    });
    probe.long_open = true;
    stop_probe_say(&format!("long 1000 USDJPY at {entry_px}"));
    let tick = bus
        .fresh_tick("USDJPY", Duration::from_secs(30))
        .unwrap_or_else(|why| panic!("STOP PROBE: no live USDJPY quote: {why}"));
    let trigger = ((tick.bid - pips * USDJPY_PIP) * 1000.0).round() / 1000.0;
    stop_probe_say(&format!(
        "live bid/ask {}/{}, the stop-loss goes at {trigger} ({pips} pips under the bid)",
        tick.bid, tick.ask
    ));
    let stop = format!("stp{}", now_ms());
    probe.stop_coid = Some(stop.clone()); // from the submit on, the venue may hold the stop-loss
    match probe.submit_and_await_accept(&usdjpy_stop(&stop, -1, trigger), 60) {
        Ok(venue_id) => stop_probe_say(&format!("STOP ACCEPTED, venue_order_id={venue_id}")),
        Err(why) => {
            probe.stop_coid = None;
            panic!("STOP PROBE: the stop was REFUSED, in the venue's words: {why}");
        }
    }
    (stop, trigger)
}

/// LIVE, the native stop-loss FIRING: buy 1000 USDJPY, put a reduce-only stop a few pips under the bid
/// (`DUKASCOPY_STOP_PIPS`, default 3), wait for the venue to fire it (`DUKASCOPY_STOP_WAIT_SECS`, default 600),
/// print every event (the closing `Fill` must carry the STOP's coid and `<order id>:c:<stop coid>`), and end FLAT
/// (the venue itself refuses a stop on either side). A stop that does not fire in time is cancelled and the
/// position flattened by the guard, and the test FAILS (nothing proven).
///
/// Self-skips without `DUKASCOPY_STOP_PROBE` (the token) and credentials; PLACES REAL DEMO ORDERS. ONE JForex login
/// per account: run it only while no gatewayhub holds the account's sidecar (`account.active` off).
#[test]
#[ignore = "live: network + demo creds + the jar + DUKASCOPY_STOP_PROBE + DUKASCOPY_SMOKE_ACCOUNT_ID; PLACES REAL DEMO ORDERS"]
fn dukascopy_jforex_stop_loss_fires_and_ends_flat() {
    let Some((mut probe, bus)) = stop_probe_start() else { return };
    probe.assert_flat(); // nothing of anyone else's is open: the stop covers the oldest orders first
    let pips = stop_pips(3.0, 1.0..=50.0);
    let wait = stop_wait_secs(600.0, 30.0..=3600.0) as u64;
    let (stop, trigger) = stop_probe_open_and_stop(&mut probe, &bus, pips);
    let until = std::time::Instant::now() + Duration::from_secs(wait);
    let mut closing_fill = None;
    let mut next_quote = std::time::Instant::now();
    while std::time::Instant::now() < until {
        if std::time::Instant::now() >= next_quote {
            if let Ok(t) = bus.fresh_tick("USDJPY", Duration::from_secs(5)) {
                stop_probe_say(&format!(
                    "bid {} vs the stop {trigger} ({:.1} pips away)",
                    t.bid,
                    (t.bid - trigger) / USDJPY_PIP
                ));
            }
            next_quote = std::time::Instant::now() + Duration::from_secs(60);
        }
        match probe.next(10) {
            Some(Event::Fill(f)) if f.client_order_id == stop => closing_fill = Some(f),
            Some(Event::OrderFilled(e)) if e.client_order_id == stop => {
                probe.stop_coid = None;
                probe.long_open = false;
                break;
            }
            _ => {}
        }
    }
    let Some(fill) = closing_fill else {
        panic!(
            "STOP PROBE: the stop-loss did NOT fire within {wait} s (the guard cancels it and flattens): nothing proven"
        );
    };
    stop_probe_say(&format!(
        "STOP-LOSS FIRED: closing fill under the stop's coid, trade_id={} side={} qty={} px={} commission={}",
        fill.trade_id, fill.side, fill.last_qty, fill.last_px, fill.commission
    ));
    assert_eq!(fill.side, -1, "a SELL closes the long");
    assert!(
        fill.trade_id.to_string().contains(":c:"),
        "the closing trade id scheme: {}",
        fill.trade_id
    );
    probe.assert_flat();
}

/// LIVE, the stop-loss SURVIVING the sidecar: buy 1000 USDJPY, put a reduce-only stop `DUKASCOPY_STOP_PIPS` (default
/// 2) under the bid, then END the sidecar (the venue keeps its orders) and come back every 120 s with a NEW sidecar
/// (its books empty, the restart gap) up to `DUKASCOPY_STOP_WAIT_SECS` (default 900). The new session asks the
/// venue the one question it can: a reduce-only stop. "no open long order" with no "already carry a stop-loss"
/// means the position is GONE, closed by the venue's stop-loss while no client was attached (proven); "already
/// carry a stop-loss" means it is still open AND still protected (the stop-loss itself survived the restart, read
/// off the venue's own order). An ACCEPTED stop would mean the stop-loss was lost. Ends flat (flatten by market).
#[test]
#[ignore = "live: network + demo creds + the jar + DUKASCOPY_STOP_PROBE + DUKASCOPY_SMOKE_ACCOUNT_ID; PLACES REAL DEMO ORDERS"]
fn dukascopy_jforex_stop_loss_survives_a_sidecar_that_is_gone() {
    let Some((mut probe, bus)) = stop_probe_start() else { return };
    probe.assert_flat(); // nothing of anyone else's is open: the stop covers the oldest orders first
    let pips = stop_pips(2.0, 1.0..=50.0);
    let wait = stop_wait_secs(900.0, 120.0..=3600.0) as u64;
    let (_stop, _trigger) = stop_probe_open_and_stop(&mut probe, &bus, pips);
    // The sidecar goes away with the stop-loss on the order: the guard must NOT cancel it on the way out.
    probe.stop_coid = None;
    probe.long_open = false;
    let mut gone = probe.client.take().expect("the client");
    gone.detach();
    drop(probe);
    stop_probe_say("the sidecar is GONE; the position and its stop-loss are the venue's alone now");
    let until = std::time::Instant::now() + Duration::from_secs(wait);
    while std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_secs(120));
        let (mut again, _bus) = stop_probe_start().expect("the second session (same environment)");
        let coid = format!("stp{}", now_ms());
        let answer = again.submit_and_await_accept(&usdjpy_stop(&coid, -1, 100.0), 60);
        match answer {
            Err(why) if why.contains("no open long order") && !why.contains("already carry") => {
                stop_probe_say(
                    "PROVEN: the position is GONE - the venue's stop-loss closed it while no sidecar was attached",
                );
                again.assert_flat();
                return;
            }
            Err(why) if why.contains("already carry a stop-loss") => {
                stop_probe_say(
                    "still open, and the venue still holds the stop-loss on it (it survived the restart)",
                );
            }
            Ok(_) => {
                // an ACCEPTED stop means the order carried NO stop-loss: it was lost
                again.stop_coid = Some(coid);
                again.long_open = true;
                panic!(
                    "STOP PROBE: the stop-loss did NOT survive the sidecar: a new stop was accepted on the open order (the guard flattens)"
                );
            }
            Err(other) => {
                again.long_open = true;
                panic!(
                    "STOP PROBE: unexpected answer to the probe stop: {other} (the guard flattens)"
                );
            }
        }
        drop(again);
    }
    // The window ended with the position still open and protected: flatten it, say so, fail (the fire was not seen).
    let (mut last, _bus) = stop_probe_start().expect("the last session");
    last.long_open = true;
    drop(last); // the guard flattens
    panic!(
        "STOP PROBE: no fire within {wait} s; the stop-loss survived the restart (read off the venue) but did not fire: the flatten was sent"
    );
}

/// LIVE, MODIFY and CANCEL of a stop-loss on a real order: buy 1000 USDJPY, put a reduce-only stop
/// `DUKASCOPY_STOP_PIPS` (default 8) under the bid, MOVE it 2 pips further away, ask the venue what it holds (a second
/// stop at another price is refused, and the refusal names the stop-loss price the order carries), CANCEL it, place a new
/// stop (accepted only if the cancel really took the stop-loss off), cancel that one too, and flatten. The stop never
/// comes near the market, so it is not expected to fire; if it does the guard ends flat all the same.
#[test]
#[ignore = "live: network + demo creds + the jar + DUKASCOPY_STOP_PROBE + DUKASCOPY_SMOKE_ACCOUNT_ID; PLACES REAL DEMO ORDERS"]
fn dukascopy_jforex_stop_loss_modify_cancel_and_replace() {
    let Some((mut probe, bus)) = stop_probe_start() else { return };
    probe.assert_flat();
    let pips = stop_pips(8.0, 3.0..=50.0);
    let (stop, trigger) = stop_probe_open_and_stop(&mut probe, &bus, pips);
    // MODIFY: two pips further from the market.
    let moved = ((trigger - 2.0 * USDJPY_PIP) * 1000.0).round() / 1000.0;
    probe.client().modify(&usdjpy_stop(&stop, -1, trigger), None, Some(moved));
    let until = std::time::Instant::now() + Duration::from_secs(60);
    let mut modified = false;
    while std::time::Instant::now() < until && !modified {
        match probe.next(5) {
            Some(Event::OrderModified(e)) if e.client_order_id == stop => modified = true,
            Some(Event::OrderModifyRejected(e)) if e.client_order_id == stop => {
                panic!("STOP PROBE: the modify was REFUSED, in the venue's words: {}", e.reason)
            }
            _ => {}
        }
    }
    assert!(modified, "STOP PROBE: no answer to the modify");
    // What the venue holds now: a second stop at another price is refused, naming the stop-loss the order carries.
    let probe_coid = format!("stp{}", now_ms());
    match probe.submit_and_await_accept(&usdjpy_stop(&probe_coid, -1, trigger + 0.5), 60) {
        Err(why) if why.contains(&format!("another stop-loss at {moved}")) => {
            stop_probe_say(&format!(
                "MODIFY PROVEN: the venue's order carries the stop-loss {moved} (was {trigger})"
            ));
        }
        other => panic!("STOP PROBE: after the modify the order should carry {moved}: {other:?}"),
    }
    // CANCEL: the stop-loss comes off.
    probe.client().cancel(&stop);
    let until = std::time::Instant::now() + Duration::from_secs(60);
    let mut canceled = false;
    while std::time::Instant::now() < until && !canceled {
        if let Some(Event::OrderCanceled(e)) = probe.next(5)
            && e.client_order_id == stop
        {
            canceled = true;
        }
    }
    assert!(canceled, "STOP PROBE: no OrderCanceled for the stop");
    probe.stop_coid = None;
    // A new stop is accepted only if the cancel really took the stop-loss off the order.
    let again = format!("stp{}", now_ms());
    probe.stop_coid = Some(again.clone());
    let venue_id = probe
        .submit_and_await_accept(&usdjpy_stop(&again, -1, trigger), 60)
        .unwrap_or_else(|why| panic!("STOP PROBE: a new stop was refused after the cancel: {why}"));
    stop_probe_say(&format!(
        "CANCEL PROVEN: the order carried no stop-loss, a new stop was accepted ({venue_id})"
    ));
    probe.unwind(); // cancels that stop, then flattens the long
    probe.assert_flat();
}

/// LIVE, the way back from the RESTART GAP: buy 1000 USDJPY, put a reduce-only stop `DUKASCOPY_STOP_PIPS` (default 8)
/// under the bid, END the sidecar and start a NEW one (its stop book is empty; the venue still holds the stop-loss).
/// The new sidecar must (1) refuse a stop at another price, naming the stop-loss the order still carries, (2) take
/// the stop OVER when it is sent again at the same price (nothing changed at the venue), (3) cancel it, which takes the
/// stop-loss off the order (a new stop is then accepted). Ends flat.
#[test]
#[ignore = "live: network + demo creds + the jar + DUKASCOPY_STOP_PROBE + DUKASCOPY_SMOKE_ACCOUNT_ID; PLACES REAL DEMO ORDERS"]
fn dukascopy_jforex_stop_loss_is_taken_over_after_a_restart() {
    let Some((mut first, bus)) = stop_probe_start() else { return };
    first.assert_flat();
    let pips = stop_pips(8.0, 3.0..=50.0);
    let (_stop, trigger) = stop_probe_open_and_stop(&mut first, &bus, pips);
    // The sidecar goes away with the stop-loss on the order: the first guard must not cancel it on the way out.
    first.stop_coid = None;
    first.long_open = false;
    let mut gone = first.client.take().expect("the client");
    gone.detach();
    drop(first);
    stop_probe_say(
        "the sidecar is GONE and a new one starts: its stop book is empty, the venue still holds the stop-loss",
    );
    let (mut again, _bus) = stop_probe_start().expect("the second session (same environment)");
    again.long_open = true; // the second guard flattens whatever happens below
    // (1) another price: refused, and the refusal names what the venue holds.
    let elsewhere = format!("stp{}", now_ms());
    match again.submit_and_await_accept(&usdjpy_stop(&elsewhere, -1, trigger + 0.5), 60) {
        Err(why) if why.contains(&format!("another stop-loss at {trigger}")) => {
            stop_probe_say(&format!(
                "SURVIVAL READ OFF THE VENUE: the order still carries the stop-loss {trigger}"
            ));
        }
        other => panic!("STOP PROBE: the order should still carry {trigger}: {other:?}"),
    }
    // (2) the same price takes it over.
    let taken = format!("stp{}", now_ms());
    again.stop_coid = Some(taken.clone());
    let venue_id = again
        .submit_and_await_accept(&usdjpy_stop(&taken, -1, trigger), 60)
        .unwrap_or_else(|why| panic!("STOP PROBE: the take-over was refused: {why}"));
    stop_probe_say(&format!(
        "TAKE-OVER PROVEN: the stop at the same price was accepted ({venue_id})"
    ));
    // (3) it is this sidecar's again: cancel takes the stop-loss off the order.
    again.client().cancel(&taken);
    let until = std::time::Instant::now() + Duration::from_secs(60);
    let mut canceled = false;
    while std::time::Instant::now() < until && !canceled {
        if let Some(Event::OrderCanceled(e)) = again.next(5)
            && e.client_order_id == taken
        {
            canceled = true;
        }
    }
    assert!(canceled, "STOP PROBE: no OrderCanceled for the taken-over stop");
    again.stop_coid = None;
    let fresh = format!("stp{}", now_ms());
    again.stop_coid = Some(fresh.clone());
    again
        .submit_and_await_accept(&usdjpy_stop(&fresh, -1, trigger - 0.01), 60)
        .unwrap_or_else(|why| panic!("STOP PROBE: a new stop was refused after the cancel: {why}"));
    stop_probe_say(
        "CANCEL AFTER TAKE-OVER PROVEN: the order carried no stop-loss, a new stop was accepted",
    );
    again.unwind(); // cancels that stop, then flattens the long
    again.assert_flat();
}
