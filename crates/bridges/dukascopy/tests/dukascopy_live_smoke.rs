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
//! `crates/bridges/dukascopy/src/adapter.rs`'s `VenueAdapterDukascopy` makes over the directories its
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

use vike_dukascopy::feed::QuoteBus;
use vike_dukascopy::{
    DukascopyConfig, DukascopyTools, ExecutionClientDukascopy, load_dukascopy_config_from,
    resolve_dukascopy_tools,
};
use vike_execution::testing::recv_event_within;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_model::AccountTier;
use vike_model::OrderRequest;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;

/// The smoke account's login from the credential store (`smoke_account`, resolved through
/// `vike_secrets::smoke_directory` at the demo tier); `None` (skip) when absent — the live gate.
/// `server` is the daemon's JNLP: the box's `dukascopy demo login rest jnlp` address row
/// ([`dukascopy_settings`]), so the smoke dials the JNLP the node dials.
fn live_config() -> Option<DukascopyConfig> {
    // DUKASCOPY_SMOKE_ACCOUNT=<LABEL> switches to that demo account (differential testing: a
    // second account has its own login counter and server backend).
    let directory = vike_secrets::smoke_directory();
    let credentials = directory.credentials_for("dukascopy", &smoke_account(), AccountTier::Demo);
    with_jnlp(load_dukascopy_config_from(&credentials, &dukascopy_settings()))
}

/// The login, or `None` (skip); a whole login with no usable JNLP row is LOUD, never a skip.
fn with_jnlp(config: Option<Result<DukascopyConfig, String>>) -> Option<DukascopyConfig> {
    config.map(|c| c.unwrap_or_else(|why| panic!("{why}")))
}

/// The venue's settings and `venue_url` address rows from the settings database of the settings directory
/// `VIKE_SETTINGS_DIR` names (else the project walk from the working directory): the same read as
/// `crates/bridges/vike-ibkr/tests/ibkr_smokes/common.rs`'s `ibkr_settings`. A box with no database or no
/// `venue_url` table reads the release's addresses (`vike_secrets::venue_setting::load_venue_settings`); no
/// settings directory at all reads them too.
///
/// ⚠ LOUD, never a quiet default: a database that will not read would otherwise hand this smoke the
/// release's JNLP instead of the row the node dials.
fn dukascopy_settings() -> vike_secrets::venue_setting::VenueSettings {
    let settings_dir_override = std::env::var("VIKE_SETTINGS_DIR").ok();
    vike_paths::state_path::project_settings_dir_from(
        settings_dir_override.as_deref(),
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| {
        vike_secrets::venue_setting::load_venue_settings(&dir)
            .unwrap_or_else(|e| panic!("the settings database could not be read: {e}"))
    })
    .and_then(|mut all| all.remove("dukascopy"))
    .unwrap_or_else(|| {
        vike_secrets::venue_setting::VenueSettings::default()
            .with_urls(vike_venues::venue_urls::VenueUrls::seed("dukascopy"))
    })
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
/// `crates/bridges/dukascopy/src/adapter.rs`'s `VenueAdapterDukascopy` makes the same call over what
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

fn order(order_client_id: &str, side: i32, order_type: &str, price: Option<f64>) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        display_quantity: None,
        release: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        account: None,
        combo_legs: Vec::new(),
        order_client_id: order_client_id.into(),
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
        timestamp: now_ms(),
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

/// LIVE: real JForex login on the smoke's demo account — spawn returns Ok only after the
/// sidecar's `ready` handshake (account id + balance logged on stderr). Proves jar, java, JNLP
/// and creds.
#[test]
#[ignore = "live: network + demo creds + <project>/bin/jforex/jforex-bridge.jar"]
fn dukascopy_jforex_login() {
    vike_log::test_init();
    let Some(config) = live_config() else {
        tracing::warn!(target: "vike_dukascopy", "dukascopy live smoke: no demo account creds in the store — skipping");
        return;
    };
    let (events, _rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn(config, &live_tools(), events)
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
    let Some(config) = live_config() else {
        tracing::warn!(target: "vike_dukascopy", "dukascopy live smoke: no demo account creds in the store — skipping");
        return;
    };
    let (events, mut rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn(config, &live_tools(), events)
        .expect("live JForex login (ready)");
    tracing::info!(target: "vike_dukascopy", "LOGIN OK — starting ladder");

    // Leg 1: market BUY 1000 EURUSD.
    let order_client_id1 = format!("smk{}", now_ms());
    client.submit(&order(&order_client_id1, 1, "market", None));
    match recv_event_within(&mut rx, Duration::from_secs(30)) {
        Event::OrderSubmitted(e) => assert_eq!(e.order_client_id, order_client_id1),
        other => panic!("expected OrderSubmitted, got {other:?}"),
    }
    match recv_event_within(&mut rx, Duration::from_secs(60)) {
        Event::OrderRejected(e) => {
            // Weekend / closed market: the venue answered — full round trip proven.
            tracing::warn!(target: "vike_dukascopy", "VENUE REJECTED (acceptable when market closed): {}", e.reason);
            client.detach();
            return;
        }
        Event::OrderAccepted(e) => {
            assert_eq!(e.order_client_id, order_client_id1);
            tracing::info!(target: "vike_dukascopy", "ACCEPTED venue_order_id={:?}", e.venue_order_id);
        }
        other => panic!("expected OrderAccepted or OrderRejected, got {other:?}"),
    }
    // Dual-publish contract (Proto.orderFilled, proven by the dukascopy_exec.rs fake): a
    // fill emits BOTH the bare FillEvent — which the core Account folds into position/PnL —
    // FIRST, then the wrapping OrderFilled the order FSM applies, both carrying the same
    // fill. The bare Fill is therefore the event that arrives here, before the wrap.
    let fill = match recv_event_within(&mut rx, Duration::from_secs(60)) {
        Event::Fill(fill) => {
            assert_eq!(fill.order_client_id, order_client_id1);
            assert_eq!(fill.venue, "dukascopy");
            assert_eq!(fill.symbol, "EURUSD");
            fill
        }
        other => panic!("expected bare Fill, got {other:?}"),
    };
    match recv_event_within(&mut rx, Duration::from_secs(60)) {
        Event::OrderFilled(e) => assert_eq!(e.order_client_id, order_client_id1),
        other => panic!("expected OrderFilled wrap, got {other:?}"),
    }
    tracing::info!(target: "vike_dukascopy", "FILLED {} @ {}", fill.last_qty, fill.last_price);

    // Leg 2: opposite market SELL nets the position flat (bridge close emulation).
    // Contract: synthetic OrderAccepted(venue_order_id="net:<order>") first, then
    // OrderPartiallyFilled per non-final close leg, and exactly ONE terminal
    // OrderFilled. NOTE the netting may close a STALE opposing position left by an
    // earlier crashed run before this leg's — that's correct netting behavior.
    let order_client_id2 = format!("smk{}", now_ms());
    client.submit(&order(&order_client_id2, -1, "market", None));
    loop {
        match recv_event_within(&mut rx, Duration::from_secs(60)) {
            Event::OrderSubmitted(_) => continue,
            Event::OrderAccepted(e) => {
                assert_eq!(
                    e.order_client_id, order_client_id2,
                    "net-accept under submitting order client id"
                );
                tracing::info!(target: "vike_dukascopy", "NET-ACCEPTED {:?}", e.venue_order_id);
                continue;
            }
            Event::Fill(f) => {
                // Dual-publish: the bare FillEvent precedes each partial/terminal wrap.
                assert_eq!(
                    f.order_client_id, order_client_id2,
                    "net-close fill under submitting order client id"
                );
                tracing::info!(target: "vike_dukascopy", "NET-CLOSE FILL {} @ {}", f.last_qty, f.last_price);
                continue;
            }
            Event::OrderPartiallyFilled(e) => {
                assert_eq!(e.order_client_id, order_client_id2);
                tracing::info!(target: "vike_dukascopy", "NET-CLOSE LEG {} @ {}", e.fill.last_qty, e.fill.last_price);
                continue;
            }
            Event::OrderFilled(e) => {
                assert_eq!(
                    e.order_client_id, order_client_id2,
                    "close fill under submitting order client id"
                );
                tracing::info!(target: "vike_dukascopy", "NET-CLOSED {} @ {}", e.fill.last_qty, e.fill.last_price);
                break;
            }
            other => panic!("expected net-close lifecycle event, got {other:?}"),
        }
    }

    // Leg 3: resting limit far below market, then cancel.
    let order_client_id3 = format!("smk{}", now_ms());
    client.submit(&order(&order_client_id3, 1, "limit", Some(0.9000)));
    loop {
        match recv_event_within(&mut rx, Duration::from_secs(60)) {
            Event::OrderSubmitted(_) => continue,
            Event::OrderAccepted(e) => {
                assert_eq!(e.order_client_id, order_client_id3);
                tracing::info!(target: "vike_dukascopy", "LIMIT RESTING — canceling");
                break;
            }
            other => panic!("expected limit OrderAccepted, got {other:?}"),
        }
    }
    client.cancel(&order_client_id3);
    match recv_event_within(&mut rx, Duration::from_secs(60)) {
        Event::OrderCanceled(e) => assert_eq!(e.order_client_id, order_client_id3),
        other => panic!("expected OrderCanceled, got {other:?}"),
    }
    tracing::info!(target: "vike_dukascopy", "LADDER GREEN: login -> market fill -> net close -> limit rest -> cancel");
    client.detach();
}

/// The word `dukascopy_jforex_metal_unit_far_limit` needs in `DUKASCOPY_METAL_PROBE` before it places anything.
const METAL_PROBE_TOKEN: &str = "place-one-far-xauusd-limit-on-the-dukascopy-demo-account";

/// The amount the metal probe falls back to when the venue states no minimum and `DUKASCOPY_METAL_UNITS` names none: ONE
/// ounce, the vendor's documented minimum of XAU/USD (order-amounts wiki), which a jar of the release that sizes a metal
/// from the venue's own terms (`OrderMap.java`'s `minUnits`) takes. It used to be the 1000-unit floor of a jar before the
/// per-instrument terms, which is 1000 oz of gold and millions of dollars.
const METAL_PROBE_UNITS: f64 = 1.0;

/// The amount to place: `DUKASCOPY_METAL_UNITS` when it is a positive finite number (`raw`, `None`: unset), else the
/// minimum the VENUE states for the instrument on the sidecar's `terms` line (`stated_min`: the probe places what the
/// venue says it takes, not what this file assumes), else [`METAL_PROBE_UNITS`].
fn metal_units(raw: Option<&str>, stated_min: Option<f64>) -> f64 {
    let positive = |v: f64| v.is_finite() && v > 0.0;
    raw.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| positive(*v))
        .or(stated_min.filter(|v| positive(*v)))
        .unwrap_or(METAL_PROBE_UNITS)
}

/// The most notional (USD) the probe will place an order for unless `DUKASCOPY_METAL_MAX_NOTIONAL_USD` says more. Raised
/// from 5000 on the owner's default (a DEMO account only): the proof order is ONE ounce, and 5000 USD refused it once gold
/// passed that price. 25,000 USD admits one ounce at up to five times the price of 2026 and still refuses ten ounces
/// and, by two orders of magnitude, the 1000 oz of a jar before the venue's terms.
const METAL_MAX_NOTIONAL_DEFAULT_USD: f64 = 25_000.0;

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
        Ok(Some(Ingest::Event(event))) => Some(event),
        _ => None,
    }
}

/// **The probe's order never outlives the test.** From the moment the order is submitted the guard owns the client
/// and the lane; on drop (a normal end, a failed assertion, any panic) it cancels the order if the venue may still
/// hold it, waits for the cancel's answer, says LOUDLY and by order id if it could not confirm one, and detaches.
struct MetalProbe {
    client: Option<ExecutionClientDukascopy>,
    rx: tokio::sync::mpsc::Receiver<Ingest>,
    order_client_id: String,
    /// True from the submit until the venue's answer says nothing rests (a rejection, or the cancel's).
    may_rest: bool,
}

impl MetalProbe {
    fn client(&mut self) -> &mut ExecutionClientDukascopy {
        self.client.as_mut().expect("the client lives until the guard drops")
    }
}

impl Drop for MetalProbe {
    fn drop(&mut self) {
        let Some(mut client) = self.client.take() else { return };
        if self.may_rest {
            client.cancel(&self.order_client_id);
            let mut canceled = false;
            while let Some(event) = try_event(&mut self.rx, 30) {
                if matches!(&event, Event::OrderCanceled(e) if e.order_client_id == self.order_client_id)
                {
                    canceled = true;
                    break;
                }
            }
            if canceled {
                eprintln!(
                    "METAL PROBE: the order {} was canceled on the way out",
                    self.order_client_id
                );
            } else {
                eprintln!(
                    "\n!!! METAL PROBE: COULD NOT CONFIRM THE CANCEL of order {0} - IT MAY STILL REST ON THE DEMO \
                     ACCOUNT. CANCEL IT BY HAND in the Dukascopy platform (its label starts `v{0}`) !!!\n",
                    self.order_client_id
                );
                tracing::error!(target: "vike_dukascopy", order_client_id = %self.order_client_id, "METAL PROBE: cancel NOT confirmed - cancel by hand");
            }
        }
        client.detach();
    }
}

/// LIVE, the METAL AMOUNT PLAN (multi-currency money, decision 0183's Part 2; the script is
/// `n080-DK2-duka-metal-unit.sh` in the owner's kit, and, through the node, the kit's metals-and-CFDs round trip):
/// ONE far limit BUY on XAU/USD of the amount the VENUE states it takes. The probe
/// reads the sidecar's `terms` line for XAU/USD (the venue's own minimum, increment, maximum and amount per contract),
/// logs it with the grid the node would admit the metal on (`crates/bridges/dukascopy/src/instrument/any_symbol.rs`'s
/// `facts_from_terms`), and places that minimum unless `DUKASCOPY_METAL_UNITS` names another amount (a jar without the
/// terms line falls back to one ounce, the vendor's documented minimum). The unit IS the vendor's ounce (order-amounts
/// wiki: minimum 1 oz of XAU/USD, 50 oz of XAG/USD); what no one has seen is the order on this account. It stays a smoke
/// because it places a REAL order on a DEMO account, which the node's door never does on its own.
///
/// It reads the instrument's LIVE quote off the sidecar first and places nothing unless (`metal_plan`) the quote is
/// two-sided, the worst-case notional (the units x the live ask, a unit read as one ounce) is within
/// `DUKASCOPY_METAL_MAX_NOTIONAL_USD` (default 25000, raised on the owner's default for a demo account: one ounce fits at
/// any plausible gold price, ten do not, and the refusal names the value to set) and the limit sits under 95% of the live ask. Then it places the order a long way under the
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
    let Some(config) = live_config() else {
        tracing::warn!(target: "vike_dukascopy", "metal probe: no demo account creds in the store — skipping");
        return;
    };
    let bus = QuoteBus::new();
    let (events, rx) = event_channel(64);
    let client =
        ExecutionClientDukascopy::spawn_quoting(config, &live_tools(), events, Arc::clone(&bus))
            .expect("live JForex login (ready)");
    let mut probe =
        MetalProbe { client: Some(client), rx, order_client_id: String::new(), may_rest: false };
    // The live quote of the instrument, off the sidecar's own price lane: nothing is placed without one.
    let live = bus
        .fresh_tick("XAUUSD", Duration::from_secs(30))
        .unwrap_or_else(|why| panic!("METAL PROBE: no live XAUUSD quote, nothing placed: {why}"));
    // What the VENUE states for the instrument (the sidecar's `terms` line, written at its first tick - the live quote
    // above has just brought one): the amount placed is its stated minimum unless the operator names another, and the
    // grid the node would admit it on is logged, so the run records the real minimum and step.
    let stated = bus.venue_terms("XAUUSD", Duration::from_secs(30));
    match &stated {
        Ok(terms) => {
            let facts = bus
                .instrument_list()
                .and_then(|rows| vike_dukascopy::instrument::any_symbol::facts_basis(&rows, "XAUUSD"))
                .and_then(|basis| match basis {
                    vike_dukascopy::instrument::any_symbol::DukascopyPropertiesBasis::VenueTerms(row) => {
                        vike_dukascopy::instrument::any_symbol::facts_from_terms(&row, terms)
                    }
                    vike_dukascopy::instrument::any_symbol::DukascopyPropertiesBasis::Static => {
                        Err("not a metal".to_string())
                    }
                });
            tracing::info!(
                target: "vike_dukascopy",
                "METAL PROBE: the venue states {terms:?}; the node would admit XAUUSD on {facts:?}"
            );
        }
        Err(why) => tracing::warn!(
            target: "vike_dukascopy",
            "METAL PROBE: the venue's terms could not be read ({why}); falling back to {METAL_PROBE_UNITS} unit"
        ),
    }
    let units = metal_units(
        std::env::var("DUKASCOPY_METAL_UNITS").ok().as_deref(),
        stated.as_ref().ok().and_then(|terms| terms.min),
    );
    tracing::info!(
        target: "vike_dukascopy",
        "METAL PROBE: live XAUUSD {}/{}, reference {reference}, {units} units = up to {:.0} USD (cap {max_notional})",
        live.bid, live.ask, units * live.ask
    );
    let price = metal_plan(reference, (live.bid, live.ask), units, max_notional)
        .unwrap_or_else(|why| panic!("METAL PROBE REFUSED, nothing placed: {why}"));
    let order_client_id = format!("smk{}", now_ms());
    let mut req = order(&order_client_id, 1, "limit", Some(price));
    req.symbol = "XAUUSD".into();
    req.qty = units;
    tracing::info!(target: "vike_dukascopy", "METAL PROBE: BUY {} XAUUSD @ {price} (reference {reference}, 30% under)", req.qty);
    probe.order_client_id.clone_from(&order_client_id);
    probe.may_rest = true; // from the submit on, the venue may hold it
    probe.client().submit(&req);
    let accepted = loop {
        match recv_event_within(&mut probe.rx, Duration::from_secs(60)) {
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
            "METAL PROBE: holding the order {hold} s: READ ITS AMOUNT AND UNIT in the Dukascopy platform now (label starts `v{order_client_id}`)"
        );
        std::thread::sleep(Duration::from_secs(hold));
        probe.client().cancel(&order_client_id);
        match recv_event_within(&mut probe.rx, Duration::from_secs(60)) {
            Event::OrderCanceled(e) => assert_eq!(e.order_client_id, order_client_id),
            other => panic!(
                "expected OrderCanceled, got {other:?} - the guard cancels {order_client_id} again on the way out"
            ),
        }
        probe.may_rest = false;
        tracing::info!(target: "vike_dukascopy", "METAL PROBE: canceled");
    }
}

/// `DUKASCOPY_METAL_UNITS` is a positive finite number or nothing: anything else leaves the amount where it was (the
/// venue's stated minimum, else one ounce), so a typo can only keep the probe as small as it always was.
#[test]
fn the_metal_probes_amount_is_the_venues_minimum_unless_a_positive_number_is_named() {
    assert_eq!(metal_units(None, None), METAL_PROBE_UNITS);
    assert_eq!(metal_units(Some("1"), None), 1.0);
    assert_eq!(metal_units(Some(" 50 "), None), 50.0);
    // the venue's stated minimum is the amount when nothing is named: silver's is fifty ounces
    assert_eq!(metal_units(None, Some(50.0)), 50.0);
    assert_eq!(metal_units(Some("2"), Some(50.0)), 2.0, "an operator's number wins");
    for bad in ["", "0", "-1", "nan", "inf", "one"] {
        assert_eq!(metal_units(Some(bad), None), METAL_PROBE_UNITS, "{bad:?}");
        assert_eq!(metal_units(Some(bad), Some(50.0)), 50.0, "{bad:?}");
    }
    // a figure the venue states that is no amount is not placed
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(metal_units(None, Some(bad)), METAL_PROBE_UNITS, "{bad}");
    }
    // one ounce of gold is inside the default cap at today's ask and at five times it: the proof order needs nothing raised
    assert!(metal_plan(2400.0, (2400.0, 2400.5), 1.0, METAL_MAX_NOTIONAL_DEFAULT_USD).is_ok());
    assert!(
        metal_plan(12_000.0, (12_000.0, 12_000.5), 1.0, METAL_MAX_NOTIONAL_DEFAULT_USD).is_ok()
    );
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
    assert!(why.contains("DUKASCOPY_METAL_MAX_NOTIONAL_USD=25000"), "the cap in force: {why}");
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
    assert_eq!(metal_max_notional(None), METAL_MAX_NOTIONAL_DEFAULT_USD);
    assert_eq!(metal_max_notional(Some(" 3000000 ")), 3_000_000.0);
    for bad in ["", "abc", "-5", "0", "NaN", "inf"] {
        assert_eq!(metal_max_notional(Some(bad)), METAL_MAX_NOTIONAL_DEFAULT_USD, "{bad:?}");
    }
}

// ---------------------------------------------------------------------------------------------------
// The native stop-loss (`StrategyBridge.java`'s `handleStop`), LIVE on the demo account: ONE USDJPY 1000.
// ---------------------------------------------------------------------------------------------------

/// The word the two stop-loss probes need in `DUKASCOPY_STOP_PROBE` before they place anything.
const STOP_PROBE_TOKEN: &str = "open-and-stop-one-usdjpy-1000-on-the-dukascopy-demo-account";

/// USDJPY's pip (`Instrument.USDJPY.getPipValue()`): a price sits on a tenth of it, 0.001.
const USDJPY_PIP: f64 = 0.01;

fn usdjpy_order(order_client_id: &str, side: i32, order_type: &str) -> OrderRequest {
    let mut req = order(order_client_id, side, order_type, None);
    req.symbol = "USDJPY".into();
    req
}

/// A reduce-only stop: the stop-loss of the open orders that face `side`.
fn usdjpy_stop(order_client_id: &str, side: i32, trigger: f64) -> OrderRequest {
    let mut req = usdjpy_order(order_client_id, side, "stop");
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
    with_jnlp(load_dukascopy_config_from(&credentials, &dukascopy_settings()))
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
    client: Option<ExecutionClientDukascopy>,
    rx: tokio::sync::mpsc::Receiver<Ingest>,
    /// True from the entry's fill until a stop-loss fill or the flatten's fill says the long is gone.
    long_open: bool,
    /// The stop that may still hold the stop-loss.
    stop_order_client_id: Option<String>,
}

impl StopProbe {
    fn client(&mut self) -> &mut ExecutionClientDukascopy {
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
                Some(Event::OrderAccepted(e)) if e.order_client_id == req.order_client_id => {
                    return Ok(e.venue_order_id.map(|v| v.to_string()).unwrap_or_default());
                }
                Some(Event::OrderRejected(e)) if e.order_client_id == req.order_client_id => {
                    return Err(e.reason.to_string());
                }
                _ => {}
            }
        }
        Err("no answer within the wait".into())
    }

    /// Wait for `order_client_id`'s terminal fill (`OrderFilled`): its price; a rejection or a cancel is the `Err`.
    fn await_filled(&mut self, order_client_id: &str, secs: u64) -> Result<f64, String> {
        let until = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < until {
            match self.next(5) {
                Some(Event::OrderFilled(e)) if e.order_client_id == order_client_id => {
                    return Ok(e.fill.last_price);
                }
                Some(Event::OrderRejected(e)) if e.order_client_id == order_client_id => {
                    return Err(format!("rejected: {}", e.reason));
                }
                Some(Event::OrderCanceled(e)) if e.order_client_id == order_client_id => {
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
        if let Some(stop) = self.stop_order_client_id.take() {
            self.client().cancel(&stop);
            while let Some(event) = self.next(30) {
                match event {
                    Event::OrderCanceled(e) if e.order_client_id == stop => break,
                    Event::OrderFilled(e) if e.order_client_id == stop => {
                        self.long_open = false; // the stop-loss fired first: the long is gone
                        break;
                    }
                    _ => {}
                }
            }
        }
        if self.long_open {
            let order_client_id = format!("stp{}", now_ms());
            self.client().submit(&usdjpy_order(&order_client_id, -1, "market"));
            match self.await_filled(&order_client_id, 90) {
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
            let order_client_id = format!("stp{}", now_ms());
            match self.submit_and_await_accept(&usdjpy_stop(&order_client_id, side, trigger), 60) {
                Err(why) if why.contains("no open") && !why.contains("already carry") => {}
                other => {
                    if other.is_ok() {
                        self.client().cancel(&order_client_id);
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
    let Some(config) = live_config_by_id() else {
        tracing::warn!(target: "vike_dukascopy", "stop probe: no DUKASCOPY_SMOKE_ACCOUNT_ID / no credentials - skipping");
        return None;
    };
    let bus = QuoteBus::new();
    let (events, rx) = event_channel(256);
    let client =
        ExecutionClientDukascopy::spawn_quoting(config, &live_tools(), events, Arc::clone(&bus))
            .expect("live JForex login (ready)");
    Some((
        StopProbe { client: Some(client), rx, long_open: false, stop_order_client_id: None },
        bus,
    ))
}

/// Buy 1000 USDJPY at market and put a reduce-only stop `pips` under the bid. Returns the stop's order client id and trigger.
fn stop_probe_open_and_stop(probe: &mut StopProbe, bus: &QuoteBus, pips: f64) -> (String, f64) {
    let entry = format!("stp{}", now_ms());
    probe.client().submit(&usdjpy_order(&entry, 1, "market"));
    let entry_price = probe.await_filled(&entry, 90).unwrap_or_else(|why| {
        panic!("STOP PROBE: the entry did not fill ({why}): is the market open?")
    });
    probe.long_open = true;
    stop_probe_say(&format!("long 1000 USDJPY at {entry_price}"));
    let tick = bus
        .fresh_tick("USDJPY", Duration::from_secs(30))
        .unwrap_or_else(|why| panic!("STOP PROBE: no live USDJPY quote: {why}"));
    let trigger = ((tick.bid - pips * USDJPY_PIP) * 1000.0).round() / 1000.0;
    stop_probe_say(&format!(
        "live bid/ask {}/{}, the stop-loss goes at {trigger} ({pips} pips under the bid)",
        tick.bid, tick.ask
    ));
    let stop = format!("stp{}", now_ms());
    probe.stop_order_client_id = Some(stop.clone()); // from the submit on, the venue may hold the stop-loss
    match probe.submit_and_await_accept(&usdjpy_stop(&stop, -1, trigger), 60) {
        Ok(venue_id) => stop_probe_say(&format!("STOP ACCEPTED, venue_order_id={venue_id}")),
        Err(why) => {
            probe.stop_order_client_id = None;
            panic!("STOP PROBE: the stop was REFUSED, in the venue's words: {why}");
        }
    }
    (stop, trigger)
}

/// LIVE, the native stop-loss FIRING: buy 1000 USDJPY, put a reduce-only stop a few pips under the bid
/// (`DUKASCOPY_STOP_PIPS`, default 3), wait for the venue to fire it (`DUKASCOPY_STOP_WAIT_SECS`, default 600),
/// print every event (the closing `Fill` must carry the STOP's order client id and `<order id>:c:<stop order client id>`), and end FLAT
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
            Some(Event::Fill(f)) if f.order_client_id == stop => closing_fill = Some(f),
            Some(Event::OrderFilled(e)) if e.order_client_id == stop => {
                probe.stop_order_client_id = None;
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
        "STOP-LOSS FIRED: closing fill under the stop's order client id, trade_id={} side={} qty={} px={} commission={}",
        fill.trade_id, fill.side, fill.last_qty, fill.last_price, fill.commission
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
    probe.stop_order_client_id = None;
    probe.long_open = false;
    let mut gone = probe.client.take().expect("the client");
    gone.detach();
    drop(probe);
    stop_probe_say("the sidecar is GONE; the position and its stop-loss are the venue's alone now");
    let until = std::time::Instant::now() + Duration::from_secs(wait);
    while std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_secs(120));
        let (mut again, _bus) = stop_probe_start().expect("the second session (same environment)");
        let order_client_id = format!("stp{}", now_ms());
        let answer = again.submit_and_await_accept(&usdjpy_stop(&order_client_id, -1, 100.0), 60);
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
                again.stop_order_client_id = Some(order_client_id);
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
            Some(Event::OrderModified(e)) if e.order_client_id == stop => modified = true,
            Some(Event::OrderModifyRejected(e)) if e.order_client_id == stop => {
                panic!("STOP PROBE: the modify was REFUSED, in the venue's words: {}", e.reason)
            }
            _ => {}
        }
    }
    assert!(modified, "STOP PROBE: no answer to the modify");
    // What the venue holds now: a second stop at another price is refused, naming the stop-loss the order carries.
    let probe_order_client_id = format!("stp{}", now_ms());
    match probe.submit_and_await_accept(&usdjpy_stop(&probe_order_client_id, -1, trigger + 0.5), 60)
    {
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
            && e.order_client_id == stop
        {
            canceled = true;
        }
    }
    assert!(canceled, "STOP PROBE: no OrderCanceled for the stop");
    probe.stop_order_client_id = None;
    // A new stop is accepted only if the cancel really took the stop-loss off the order.
    let again = format!("stp{}", now_ms());
    probe.stop_order_client_id = Some(again.clone());
    let venue_id = probe
        .submit_and_await_accept(&usdjpy_stop(&again, -1, trigger), 60)
        .unwrap_or_else(|why| panic!("STOP PROBE: a new stop was refused after the cancel: {why}"));
    stop_probe_say(&format!(
        "CANCEL PROVEN: the order carried no stop-loss, a new stop was accepted ({venue_id})"
    ));
    probe.unwind(); // cancels that stop, then flattens the long
    probe.assert_flat();
}

/// LIVE, a MEASUREMENT of an exit's SIZE amend: buy 1000 USDJPY, put a reduce-only stop `DUKASCOPY_STOP_PIPS`
/// (default 8) under the bid, send a modify that names a new SIZE (500) and no price, and print the answer:
/// `OrderModified` (the size moved in place) or `OrderModifyRejected` in its words. A JForex stop-loss is a price
/// on a whole order with no amount of its own (`jforex-bridge`'s `StopPlan.java`), and
/// `crates/vike-venues/src/venue_amend_mode.rs`'s dukascopy `stop` cell reads `Native` with no size/price split:
/// this run says which one the installed jar does. It asserts no answer, only that one came. Then the guard
/// cancels the stop and flattens.
#[test]
#[ignore = "live: network + demo creds + the jar + DUKASCOPY_STOP_PROBE + DUKASCOPY_SMOKE_ACCOUNT_ID; PLACES REAL DEMO ORDERS"]
fn dukascopy_jforex_stop_loss_size_amend_reads_the_answer() {
    let Some((mut probe, bus)) = stop_probe_start() else { return };
    probe.assert_flat();
    let pips = stop_pips(8.0, 3.0..=50.0);
    let (stop, trigger) = stop_probe_open_and_stop(&mut probe, &bus, pips);
    probe.client().modify(&usdjpy_stop(&stop, -1, trigger), Some(500.0), None);
    let until = std::time::Instant::now() + Duration::from_secs(60);
    let mut answer = None;
    while std::time::Instant::now() < until && answer.is_none() {
        match probe.next(5) {
            Some(Event::OrderModified(e)) if e.order_client_id == stop => {
                answer =
                    Some(format!("MOVED IN PLACE: qty {:?}, price {:?}", e.new_qty, e.new_price));
            }
            Some(Event::OrderModifyRejected(e)) if e.order_client_id == stop => {
                answer = Some(format!("REFUSED: {}", e.reason));
            }
            _ => {}
        }
    }
    let answer = answer.expect("STOP PROBE: no answer to the size amend within 60 s");
    stop_probe_say(&format!("EXIT SIZE AMEND (1000 -> 500, trigger {trigger} kept): {answer}"));
    probe.unwind(); // cancels the stop, then flattens the long
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
    first.stop_order_client_id = None;
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
    again.stop_order_client_id = Some(taken.clone());
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
            && e.order_client_id == taken
        {
            canceled = true;
        }
    }
    assert!(canceled, "STOP PROBE: no OrderCanceled for the taken-over stop");
    again.stop_order_client_id = None;
    let fresh = format!("stp{}", now_ms());
    again.stop_order_client_id = Some(fresh.clone());
    again
        .submit_and_await_accept(&usdjpy_stop(&fresh, -1, trigger - 0.01), 60)
        .unwrap_or_else(|why| panic!("STOP PROBE: a new stop was refused after the cancel: {why}"));
    stop_probe_say(
        "CANCEL AFTER TAKE-OVER PROVEN: the order carried no stop-loss, a new stop was accepted",
    );
    again.unwind(); // cancels that stop, then flattens the long
    again.assert_flat();
}

// ---------------------------------------------------------------------------------------------------
// The venue's own stop-limit and if-touched orders (`TriggerEntry.java`), LIVE on the demo account: far EURUSD 1000.
// ---------------------------------------------------------------------------------------------------

// shortcut: no token of their own, unlike the stop and metal probes (a new variable is a row of
// `crates/vike-config/src/settings/rows/bridges.rs`): these place far orders only and cancel them, so `#[ignore]`,
// the account's NUMBER and its credentials gate them, as they gate the ladder; add a token row if they ever fill.
fn trigger_probe_say(text: &str) {
    println!("TRIGGER PROBE {}: {text}", now_ms());
}

/// `px` on EURUSD's grid, a tenth of a pip (0.00001).
fn eurusd_grid(px: f64) -> f64 {
    (px * 100_000.0).round() / 100_000.0
}

/// A `kind` (`stop_limit`, `take_profit`) of 1000 EURUSD triggered at `trigger`, with `limit` as its price.
fn eurusd_entry(
    order_client_id: &str,
    side: i32,
    kind: &str,
    trigger: f64,
    limit: Option<f64>,
) -> OrderRequest {
    let mut req = order(order_client_id, side, kind, limit);
    req.trigger_price = Some(trigger);
    req
}

/// **The probe's resting orders never outlive the test.** Every accepted order client id is in `resting` until its cancel is
/// answered; on drop (a normal end, a failed assertion, any panic) the guard cancels what is left with the client it
/// holds, says LOUDLY which it could not confirm, and detaches.
struct TriggerProbe {
    client: Option<ExecutionClientDukascopy>,
    rx: tokio::sync::mpsc::Receiver<Ingest>,
    resting: Vec<String>,
}

impl TriggerProbe {
    fn client(&mut self) -> &mut ExecutionClientDukascopy {
        self.client.as_mut().expect("the client lives until the guard drops")
    }

    fn next(&mut self, secs: u64) -> Option<Event> {
        let event = try_event(&mut self.rx, secs)?;
        trigger_probe_say(&format!("{event:?}"));
        Some(event)
    }

    /// Submit `req`; its venue id when accepted (the order client id is then the guard's), else the refusal's words.
    fn place(&mut self, req: &OrderRequest) -> Result<String, String> {
        self.client().submit(req);
        let until = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < until {
            match self.next(5) {
                Some(Event::OrderAccepted(e)) if e.order_client_id == req.order_client_id => {
                    self.resting.push(req.order_client_id.clone());
                    return Ok(e.venue_order_id.map(|v| v.to_string()).unwrap_or_default());
                }
                Some(Event::OrderRejected(e)) if e.order_client_id == req.order_client_id => {
                    return Err(e.reason.to_string());
                }
                Some(Event::OrderFilled(e)) if e.order_client_id == req.order_client_id => {
                    panic!(
                        "TRIGGER PROBE: {} FILLED at {}: it was meant to rest far from the market",
                        e.order_client_id, e.fill.last_price
                    )
                }
                _ => {}
            }
        }
        Err("no answer within the wait".into())
    }

    /// Amend `req`'s limit to `new_price`: the limit the order holds now, else the refusal's words.
    fn modify(&mut self, req: &OrderRequest, new_price: f64) -> Result<f64, String> {
        self.client().modify(req, None, Some(new_price));
        let until = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < until {
            match self.next(5) {
                Some(Event::OrderModified(e)) if e.order_client_id == req.order_client_id => {
                    return e.new_price.ok_or_else(|| "OrderModified named no price".to_string());
                }
                Some(Event::OrderModifyRejected(e)) if e.order_client_id == req.order_client_id => {
                    return Err(e.reason.to_string());
                }
                _ => {}
            }
        }
        Err("no answer within the wait".into())
    }

    /// Move `req`'s trigger to `trigger`, keeping `keep` as its limit (`None` for a market-if-touched order): the trigger
    /// the order holds now, else the refusal's words (the client's own, or the venue's).
    fn modify_trigger(
        &mut self,
        req: &OrderRequest,
        keep: Option<f64>,
        trigger: f64,
    ) -> Result<f64, String> {
        self.client().modify_trigger(req, None, keep, trigger).map_err(str::to_string)?;
        let until = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < until {
            match self.next(5) {
                Some(Event::OrderModified(e)) if e.order_client_id == req.order_client_id => {
                    return e
                        .new_trigger_price
                        .ok_or_else(|| "OrderModified named no trigger".to_string());
                }
                Some(Event::OrderModifyRejected(e)) if e.order_client_id == req.order_client_id => {
                    return Err(e.reason.to_string());
                }
                _ => {}
            }
        }
        Err("no answer within the wait".into())
    }

    /// What the venue lists now, through the reconcile query: each of our resting order client ids and the kind it is reported as.
    fn kinds(&mut self) -> HashMap<String, vike_model::OrderType> {
        use vike_execution::recon::ReconcileClient;
        let reports = self
            .client()
            .recon_client()
            .fetch_order_status_reports(0)
            .unwrap_or_else(|why| panic!("TRIGGER PROBE: the recon query failed: {why}"));
        let mut kinds = HashMap::new();
        for r in reports {
            trigger_probe_say(&format!(
                "venue lists {:?} as {:?} ({})",
                r.order_client_id,
                r.order_type,
                r.venue_order_id.as_str()
            ));
            if let Some(order_client_id) = r.order_client_id {
                kinds.insert(order_client_id, r.order_type);
            }
        }
        kinds
    }

    /// Cancel every resting order client id and wait for each answer; LOUD for any the venue may still hold.
    fn cancel_all(&mut self) {
        if self.client.is_none() {
            return;
        }
        for order_client_id in std::mem::take(&mut self.resting) {
            self.client().cancel(&order_client_id);
            let until = std::time::Instant::now() + Duration::from_secs(60);
            let mut done = false;
            while std::time::Instant::now() < until && !done {
                match self.next(5) {
                    Some(Event::OrderCanceled(e)) if e.order_client_id == order_client_id => {
                        done = true
                    }
                    Some(Event::OrderCancelRejected(e)) if e.order_client_id == order_client_id => {
                        break;
                    }
                    _ => {}
                }
            }
            if !done {
                eprintln!(
                    "\n!!! TRIGGER PROBE: the cancel of {order_client_id} was NOT confirmed - a far EURUSD order MAY REMAIN on the demo account: cancel it by hand !!!\n"
                );
            }
        }
    }
}

impl Drop for TriggerProbe {
    fn drop(&mut self) {
        self.cancel_all();
        if let Some(mut client) = self.client.take() {
            client.detach();
        }
    }
}

/// Spawn a probe's sidecar on the smoke account (by NUMBER, `DUKASCOPY_SMOKE_ACCOUNT_ID`). Self-skips (`None`) without
/// the number or the credentials.
fn trigger_probe_start() -> Option<(TriggerProbe, Arc<QuoteBus>)> {
    vike_log::test_init();
    let Some(config) = live_config_by_id() else {
        tracing::warn!(target: "vike_dukascopy", "trigger probe: no DUKASCOPY_SMOKE_ACCOUNT_ID / no credentials - skipping");
        return None;
    };
    let bus = QuoteBus::new();
    let (events, rx) = event_channel(256);
    let client =
        ExecutionClientDukascopy::spawn_quoting(config, &live_tools(), events, Arc::clone(&bus))
            .expect("live JForex login (ready)");
    Some((TriggerProbe { client: Some(client), rx, resting: Vec::new() }, bus))
}

/// LIVE, the venue's own STOP-LIMIT and IF-TOUCHED orders, far from the market so none fills: a buy stop-limit 20% over
/// the ask (limit two pips worse), a buy limit-if-touched 20% under the bid (limit one pip worse) and a buy
/// market-if-touched beside it. Proves, in this order:
/// 1. the refusals by name, nothing placed: the client's of a reduce-only stop-limit, the sidecar's of a limit on the
///    better side of its trigger;
/// 2. all three are ACCEPTED by the venue (the `bridge: <kind> ... sent: BUYSTOP at T, slippage S pips` and the
///    `bridge: venue message ...` lines on stderr carry what the venue made of each: read them, a stop-limit should read
///    `STOP LIMIT BUY ... @ LIMIT <T + S pips> IF ASK => T`, an if-touched order `MIT BUY ... IF ASK <= T`);
/// 3. the recon query reports them as `StopLimit`, `TakeProfit`, `TakeProfit`;
/// 4. an amend moves the stop-limit's LIMIT and the limit-if-touched order's (`setOpenPrice(trigger, slippage)`) and a
///    market-if-touched order's is refused (it has none); a trigger amend (`modify_trigger`) moves the stop-limit's trigger
///    with its limit kept and the market-if-touched order's trigger alone. Every amend is named by an `AMEND <whose>
///    <what> <order client id>` line, and the sidecar's `bridge: amend of order ... confirmed by <path>` line says which confirmation
///    answered it (`ORDER_CHANGED_OK`, the venue's replacement notice, or a read-back): the own-session limit-if-touched
///    amend here against the adopted one in 5 tells "adopted" from "limit-if-touched" (2026-11-09: the adopted one was
///    answered by no `ORDER_CHANGED_OK`);
/// 5. after the sidecar is ENDED and a new one started (the venue keeps the orders; the new sidecar adopts them by label
///    and reads their kind back off the order's comment), the recon query still reports the three kinds, an amend of
///    the limit-if-touched order's limit is served as a slippage move, and every cancel is answered.
///
/// Self-skips without `DUKASCOPY_SMOKE_ACCOUNT_ID` and its credentials; PLACES REAL DEMO ORDERS (cancelled by the guard
/// whatever happens). ONE JForex login per account: run it only while no gatewayhub holds the account's sidecar
/// (`account.active` off). Needs a jar that announces `trigger-entry`, and an open market (the far prices come from a
/// live quote).
#[test]
#[ignore = "live: network + demo creds + a jar announcing trigger-entry + DUKASCOPY_SMOKE_ACCOUNT_ID; PLACES REAL DEMO ORDERS"]
fn dukascopy_jforex_stop_limit_and_if_touched_rest_amend_survive_a_restart_and_cancel() {
    let Some((mut probe, bus)) = trigger_probe_start() else {
        return;
    };
    let tick = bus.fresh_tick("EURUSD", Duration::from_secs(30)).unwrap_or_else(|why| {
        panic!("TRIGGER PROBE: no live EURUSD quote (is the market open?): {why}")
    });
    let stop_trigger = eurusd_grid(tick.ask * 1.2);
    let touch_trigger = eurusd_grid(tick.bid * 0.8);
    trigger_probe_say(&format!(
        "bid/ask {}/{}: stop-limit at {stop_trigger}, if-touched orders at {touch_trigger}",
        tick.bid, tick.ask
    ));

    // 1. refused by the sidecar, by name, nothing placed
    let ro = format!("trg{}", now_ms());
    let mut reduce_only =
        eurusd_entry(&ro, 1, "stop_limit", stop_trigger, Some(stop_trigger + 0.0002));
    reduce_only.reduce_only = true;
    match probe.place(&reduce_only) {
        Err(why) if why.contains("ENTRY order") => {
            trigger_probe_say("REFUSED BY NAME: a reduce-only stop-limit")
        }
        other => {
            panic!("TRIGGER PROBE: a reduce-only stop-limit must be refused by name: {other:?}")
        }
    }
    let better = format!("trg{}", now_ms());
    match probe.place(&eurusd_entry(
        &better,
        1,
        "take_profit",
        touch_trigger,
        Some(touch_trigger - 0.0001),
    )) {
        Err(why) if why.contains("better side") => {
            trigger_probe_say("REFUSED BY NAME: a limit on the better side")
        }
        other => {
            panic!("TRIGGER PROBE: a limit on the better side must be refused by name: {other:?}")
        }
    }

    // 2. the three rest
    let stop_limit = eurusd_entry(
        &format!("trs{}", now_ms()),
        1,
        "stop_limit",
        stop_trigger,
        Some(eurusd_grid(stop_trigger + 0.0002)),
    );
    let lit = eurusd_entry(
        &format!("trl{}", now_ms()),
        1,
        "take_profit",
        touch_trigger,
        Some(eurusd_grid(touch_trigger + 0.0001)),
    );
    let mit = eurusd_entry(&format!("trm{}", now_ms()), 1, "take_profit", touch_trigger, None);
    for req in [&stop_limit, &lit, &mit] {
        let venue_id = probe.place(req).unwrap_or_else(|why| {
            panic!("TRIGGER PROBE: {:?} was REFUSED, in the venue's words: {why}", req.order_type)
        });
        trigger_probe_say(&format!(
            "ACCEPTED {:?} {} venue_order_id={venue_id}",
            req.order_type, req.order_client_id
        ));
    }

    // 3. the venue's list names their kinds
    let kinds = probe.kinds();
    assert_eq!(
        kinds.get(&stop_limit.order_client_id),
        Some(&vike_model::OrderType::StopLimit),
        "{kinds:?}"
    );
    assert_eq!(
        kinds.get(&lit.order_client_id),
        Some(&vike_model::OrderType::TakeProfit),
        "{kinds:?}"
    );
    assert_eq!(
        kinds.get(&mit.order_client_id),
        Some(&vike_model::OrderType::TakeProfit),
        "{kinds:?}"
    );

    // 4. an amend moves the stop-limit's limit; a market-if-touched order has none. Each amend is named first, and the
    // sidecar's `bridge: amend of order <id> label=v<order client id>_<n> confirmed by <path>` line says which confirmation answered it
    trigger_probe_say(&format!(
        "AMEND own-session stop-limit LIMIT {}",
        stop_limit.order_client_id
    ));
    let wanted = eurusd_grid(stop_trigger + 0.0005);
    let held = probe
        .modify(&stop_limit, wanted)
        .unwrap_or_else(|why| panic!("TRIGGER PROBE: the stop-limit's amend was refused: {why}"));
    assert!(
        (held - wanted).abs() < 1e-9,
        "the limit the stop-limit holds now: {held}, wanted {wanted}"
    );
    // ...and the limit-if-touched order's limit, in THIS session. On 2026-11-09 the ADOPTED one's amend (5) was
    // answered by no ORDER_CHANGED_OK: this one's path, beside that one's, says whether "adopted" or "limit-if-touched"
    // is what the SDK leaves unanswered.
    trigger_probe_say(&format!("AMEND own-session limit-if-touched LIMIT {}", lit.order_client_id));
    let wanted_lit = eurusd_grid(touch_trigger + 0.0003);
    let held = probe.modify(&lit, wanted_lit).unwrap_or_else(|why| {
        panic!("TRIGGER PROBE: the own-session limit-if-touched order's amend was refused: {why}")
    });
    assert!(
        (held - wanted_lit).abs() < 1e-9,
        "the limit the limit-if-touched order holds now: {held}, wanted {wanted_lit}"
    );
    match probe.modify(&mit, touch_trigger + 0.0003) {
        Err(why) if why.contains("no limit") => {
            trigger_probe_say("REFUSED BY NAME: the market-if-touched order's amend")
        }
        other => panic!(
            "TRIGGER PROBE: a market-if-touched order's limit amend must be refused: {other:?}"
        ),
    }
    // ...and a trigger amend moves the stop-limit's trigger with its limit kept, and a market-if-touched order's alone
    trigger_probe_say(&format!(
        "AMEND own-session stop-limit TRIGGER {}",
        stop_limit.order_client_id
    ));
    let lower = eurusd_grid(stop_trigger - 0.0003);
    let moved = probe.modify_trigger(&stop_limit, Some(wanted), lower).unwrap_or_else(|why| {
        panic!("TRIGGER PROBE: the stop-limit's trigger amend was refused: {why}")
    });
    assert!((moved - lower).abs() < 1e-9, "the stop-limit's trigger now: {moved}, wanted {lower}");
    trigger_probe_say(&format!(
        "AMEND own-session market-if-touched TRIGGER {}",
        mit.order_client_id
    ));
    let mit_trigger = eurusd_grid(touch_trigger - 0.0005);
    let moved = probe.modify_trigger(&mit, None, mit_trigger).unwrap_or_else(|why| {
        panic!("TRIGGER PROBE: the market-if-touched order's trigger amend was refused: {why}")
    });
    assert!((moved - mit_trigger).abs() < 1e-9, "the market-if-touched trigger now: {moved}");

    // 5. a NEW sidecar reads them back off the venue
    let resting = std::mem::take(&mut probe.resting); // the venue keeps them: the first guard must not cancel them
    drop(probe);
    trigger_probe_say(
        "the sidecar is GONE and a new one starts: the venue still holds the three orders",
    );
    let (mut again, _bus) = trigger_probe_start().expect("the second session (same environment)");
    again.resting = resting; // from here the second guard cancels them
    let kinds = again.kinds();
    assert_eq!(
        kinds.get(&stop_limit.order_client_id),
        Some(&vike_model::OrderType::StopLimit),
        "after the restart: {kinds:?}"
    );
    assert_eq!(
        kinds.get(&lit.order_client_id),
        Some(&vike_model::OrderType::TakeProfit),
        "after the restart: {kinds:?}"
    );
    assert_eq!(
        kinds.get(&mit.order_client_id),
        Some(&vike_model::OrderType::TakeProfit),
        "after the restart: {kinds:?}"
    );
    trigger_probe_say(&format!("AMEND adopted limit-if-touched LIMIT {}", lit.order_client_id));
    let wanted = eurusd_grid(touch_trigger + 0.0002);
    let held = again.modify(&lit, wanted).unwrap_or_else(|why| {
        panic!("TRIGGER PROBE: the adopted limit-if-touched order's amend was refused: {why}")
    });
    assert!((held - wanted).abs() < 1e-9, "after the restart the limit is {held}, wanted {wanted}");
    again.cancel_all();
    assert!(again.resting.is_empty());
    trigger_probe_say(
        "GREEN: refused by name, rested, read back by kind, amended, read back after a restart, cancelled",
    );
}

/// LIVE, how the venue takes a PLAIN limit (the sidecar's five-argument `submitOrder`, whose Javadoc says "default value
/// of 5 pips slippage is used", while the wiki says a limit WITH a slippage becomes a MIT order): one buy limit of 1000
/// EURUSD 20% under the bid, accepted, held five seconds, then cancelled. ⚠ The answer is on the SIDECAR's stderr, not in
/// an event: run it with `--nocapture` and read the `bridge: venue message NOTIFICATION ...` (or `ORDER_SUBMIT_OK`) line.
/// `LIMIT BUY 0.001 mil. EUR/USD @ LIMIT P IF ASK <= P` = a plain limit (the default does not apply to it);
/// `MIT BUY 0.001 mil. EUR/USD @ LIMIT P+0.0005 IF ASK <= P` = the 5-pip default applies and every resting limit can fill
/// up to five pips WORSE than its price (keep the plain-limit slippage-0 commit). Self-skips without
/// `DUKASCOPY_SMOKE_ACCOUNT_ID` and its credentials; PLACES ONE REAL DEMO ORDER (cancelled by the guard). Run it on the
/// jar WITHOUT the droppable slippage-0 commit, or it measures the fix instead of the question.
#[test]
#[ignore = "live: network + demo creds + the jar + DUKASCOPY_SMOKE_ACCOUNT_ID; PLACES ONE REAL DEMO ORDER"]
fn dukascopy_jforex_plain_limit_reads_how_the_venue_takes_it() {
    let Some((mut probe, bus)) = trigger_probe_start() else {
        return;
    };
    let tick = bus.fresh_tick("EURUSD", Duration::from_secs(30)).unwrap_or_else(|why| {
        panic!("LIMIT PROBE: no live EURUSD quote (is the market open?): {why}")
    });
    let price = eurusd_grid(tick.bid * 0.8);
    let order_client_id = format!("lim{}", now_ms());
    let venue_id = probe
        .place(&order(&order_client_id, 1, "limit", Some(price)))
        .unwrap_or_else(|why| panic!("LIMIT PROBE: the far limit was refused: {why}"));
    trigger_probe_say(&format!(
        "ACCEPTED a buy limit at {price} (bid {}), venue_order_id={venue_id}",
        tick.bid
    ));
    std::thread::sleep(Duration::from_secs(5)); // the venue's notification follows the acceptance
    let kinds = probe.kinds();
    assert_eq!(
        kinds.get(&order_client_id),
        Some(&vike_model::OrderType::Limit),
        "a plain limit names no other kind: {kinds:?}"
    );
    probe.cancel_all();
    trigger_probe_say(&format!(
        "READ the `bridge: venue message` line above: `LIMIT BUY ... @ LIMIT {price} IF ASK <= {price}` = plain limit; \
         `MIT BUY ... @ LIMIT {} IF ASK <= {price}` = the 5-pip default applies",
        eurusd_grid(price + 0.0005)
    ));
}
