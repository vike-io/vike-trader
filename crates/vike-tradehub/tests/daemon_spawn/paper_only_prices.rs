//! **A paper-only tradehub prices its keyless mounts from the live feeds** (the owner's yes of 2026-11-16,
//! `crates/vike-tradehub/src/tradehub_cli/paper_feeds.rs`'s `wire_paper_feeds`), proved against the REAL binary: the
//! live switch OFF (no `flags.tradehub_live` row), one deribit `paper` account with `buy_hold.rhai` mounted on it, and
//! a real datahub strategy lane over a scripted venue client the test pushes quotes and bars through (no venue is
//! dialled). The mount's first closed bar buys, and the paper book fills it on the feed's quote: the position comes.
//! With no strategy-lane key the same node starts, its banner says `PRICE FEEDS UNAVAILABLE`, and the mount is named
//! as never trading, in the door's own words.
//!
//! Unix only: the stop is a POSIX signal (`sigterm_stop.rs`'s reason).

#![cfg(unix)]

use crate::core_no_engine::Node;
use crate::daemon_profile_seed;
use crate::temp_project::temp_dir;
use crate::venue_feed_splice_smoke::{AcknowledgingClient, SMOKE_TRADE_KEY, scripted_datahub_with};

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_data::DataClientSink;
use vike_model::AccountTier;

const VENUE: &str = "deribit";
const SYMBOL: &str = "BTC-PERPETUAL";
const SIZE: f64 = 0.01;

/// A scripted datahub serving deribit, and the sink its venue client was handed (the test's prices go in there).
struct PriceHub {
    addr: String,
    sink: Arc<Mutex<Option<Arc<dyn DataClientSink>>>>,
}

impl PriceHub {
    fn start() -> Self {
        let sink: Arc<Mutex<Option<Arc<dyn DataClientSink>>>> = Arc::default();
        let kept = Arc::clone(&sink);
        let addr = scripted_datahub_with(
            Box::new(move |_venue, given| {
                *kept.lock().expect("sink") = Some(given);
                Ok(Box::new(AcknowledgingClient(0)))
            }),
            &[VENUE],
        );
        PriceHub { addr, sink }
    }
}

/// One quote (bid 49 990, ask 50 010) and one closed 1m bar of the symbol, through the datahub's kept sink.
fn push_prices(kept: &Mutex<Option<Arc<dyn DataClientSink>>>) {
    let Some(sink) = kept.lock().expect("sink").clone() else { return };
    let now = vike_model::now_ms();
    sink.quote(
        VENUE,
        SYMBOL,
        vike_marketdata::TickQuote {
            timestamp: now,
            local_timestamp: now,
            bid: 49_990.0,
            ask: 50_010.0,
            bid_size: 1_000.0,
            ask_size: 1_000.0,
            symbol: SYMBOL.into(),
        },
    );
    let bar = vike_marketdata::Bar::ohlcv(
        (now / 60_000 - 1) * 60_000,
        50_000.0,
        50_000.0,
        50_000.0,
        50_000.0,
        1.0,
    );
    sink.close_bar(VENUE, SYMBOL, "1m", bar);
}

/// A paper-only project: deribit's default `paper` account mounted with `buy_hold.rhai`, its prices from the datahub
/// at `datahub` under the strategy-lane key when `keyed`. No `flags.tradehub_live` row: the switch is off.
fn project(dir: &Path, datahub: &str, keyed: bool) {
    let settings = dir.join("settings");
    vike_secrets::plant_settings_rows(
        &settings,
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "datahub_addr".to_string(),
                value: format!("\"{datahub}\""),
            }],
            ..Default::default()
        },
    )
    .expect("plant the rows");
    daemon_profile_seed::seed_active_daemon_profile(
        &settings,
        "paper-only-prices",
        &format!(
            "venue = \"{VENUE}\"\n\
             asset_class = \"CryptoPerp\"\n\
             symbol = \"{SYMBOL}\"\n\
             interval = \"1m\"\n\
             interval_ms = 60000\n\
             tick_size = 0.5\n\
             \n\
             [strategy]\n\
             file = \"buy_hold.rhai\"\n\
             \n\
             [strategy.params]\n\
             size = {SIZE}\n\
             \n\
             [daemon]\n\
             summary_ms = 1000\n\
             shutdown_deadline_ms = 5000\n"
        ),
    );
    let created = vike_secrets::edit_account_in_unjournalled(
        &settings,
        vike_secrets::AccountEdit::Create { venue: VENUE, tier: AccountTier::Paper, label: None },
    )
    .expect("a paper row");
    let id = created.after.expect("a created row").id;
    vike_secrets::edit_account_in_unjournalled(
        &settings,
        vike_secrets::AccountEdit::SetSymbol { id, symbol: Some(SYMBOL) },
    )
    .expect("state the symbol");
    if keyed {
        vike_secrets::save_credentials_to_store_unjournalled(
            &settings,
            vike_secrets::StoreTable::HubKey,
            &[(
                vike_hub_protocol::auth::DATAHUB_TRADE_KEY_ENV.to_string(),
                SMOKE_TRADE_KEY.to_string(),
            )],
            vike_secrets::CredentialUpsertIntent::Add,
        )
        .expect("file the strategy-lane key");
    }
}

/// The node's ready line.
fn ready(node: &mut Node) -> String {
    node.wait_line("the ready line", |line| line.contains("\"kind\":\"ready\""))
}

#[test]
fn a_paper_only_node_fills_its_keyless_mount_on_the_live_feed() {
    let hub = PriceHub::start();
    let root = temp_dir("paper-only-prices");
    project(root.path(), &hub.addr, true);
    let mut node = Node::start(root.path());

    let line = ready(&mut node);
    assert!(line.contains("\"mode\":\"PAPER\""), "a priced paper-only node is plain PAPER: {line}");
    node.wait_log("the paper-only node prices deribit from its live feed");

    // The mount's first closed bar buys; the paper book fills the market buy on the feed's next quote. The prices
    // go in on their own thread while the summary lines are read.
    let pushing = Arc::new(AtomicBool::new(true));
    let pusher = {
        let (pushing, sink) = (Arc::clone(&pushing), Arc::clone(&hub.sink));
        std::thread::spawn(move || {
            while pushing.load(Ordering::SeqCst) {
                push_prices(&sink);
                std::thread::sleep(Duration::from_millis(500));
            }
        })
    };
    let held = node.net_position("the filled position", |net| net != 0.0);
    pushing.store(false, Ordering::SeqCst);
    pusher.join().expect("the price pusher ends");
    assert!((held - SIZE).abs() < 1e-9, "the mount's buy is the position: {held}");
    node.stop();
}

#[test]
fn a_paper_only_node_with_no_strategy_lane_key_says_its_mount_gets_no_price() {
    let hub = PriceHub::start();
    let root = temp_dir("paper-only-prices-no-key");
    project(root.path(), &hub.addr, false);
    let mut node = Node::start(root.path());

    let line = ready(&mut node);
    assert!(line.contains("PRICE FEEDS UNAVAILABLE"), "the banner says it: {line}");
    node.wait_log("PRICE FEEDS UNAVAILABLE");
    node.wait_log(&vike_tradehub_client::wire::no_price_feed(VENUE));
    node.stop();
}
