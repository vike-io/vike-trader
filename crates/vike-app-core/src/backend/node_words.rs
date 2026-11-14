//! What an observe frame says that `vike_execution::CoreSnapshot` has no field for: a side cell the bridge
//! writes and the frame reads, because that type is built in the latency-gated measured crates and
//! must not grow a display-only field. [`BridgeHandle::node_words`] is the one read; a link drop
//! clears it, so nothing a node said outlives its connection.
//!
//! [`BridgeHandle::node_words`]: crate::backend::observe_bridge::BridgeHandle::node_words

use vike_model::AccountTier;
use vike_tradehub_client::wire::{
    CoreSnapshotWire, MountViewWire, PriceLinkDownWire, TradeLinkDownWire, VenueBlockWire,
    VenueLinkDownWire,
};

use crate::backend::observe_bridge::{identity_label, map_tier};

/// The frame's words, as of the bridge's last look. Empty before the first frame, from an older node
/// (each field's own doc says what its absence means) and between a link drop and the next connection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeWords {
    /// [`running_tiers`] of the frame: what the daemon's tag says.
    pub running: String,
    /// `name [tier]`, what every status surface leads with; `None` from a node that names itself not.
    pub daemon_label: Option<String>,
    /// How many venues the node runs an account on ([`runs`]). Two accounts of a venue are one venue.
    pub venues: usize,
    /// The route keys of the RUNNING accounts whose key changed in the database under them
    /// (`CoreSnapshotWire::key_changed`).
    pub key_changed: Vec<String>,
    /// The running accounts whose way to their venue is lost right now, each with what to call it.
    pub lost: Vec<LostAccount>,
    /// The route keys of the running accounts whose bridge last said its path to the venue is up
    /// (`CoreSnapshotWire::venue_path_up`). Absent is "not known", never "down".
    pub path_up: Vec<String>,
    /// What the node runs, one row per strategy: `strategy_status`'s answer, not the frame's, which
    /// the bridge asks for beside the stream. Empty before the first answer and from an older node.
    pub mounts: Vec<MountViewWire>,
}

/// One running account whose way to its venue is lost: the frame's `trade_link_down`,
/// `venue_link_down` (`no_adapter` included) or `price_link_down`, whichever cuts it off first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LostAccount {
    /// The account's routing key, as on `VenueBlock::route_key`.
    pub route_key: String,
    /// What a badge calls it: `NO LINK`, `NO ADAPTER`, `VENUE DOWN`, `NO PRICES`.
    pub word: &'static str,
    /// What it costs, in a sentence.
    pub why: String,
}

/// How one account's way to its venue stands, as the frame says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountLink<'a> {
    /// Its bridge said, on the node's last asks, that its path to the venue is up.
    Up,
    /// Nothing says: no fresh answer, a venue whose bridge reports no path, an older node. Never "up",
    /// and never "down" either (`vike_tradehub_client::wire::VENUES_REPORTING_A_PATH`).
    NotKnown,
    /// Lost, and how.
    Lost(&'a LostAccount),
}

impl NodeWords {
    /// The words of no frame, for a caller with no connection.
    pub const NONE: &'static NodeWords = &NodeWords {
        running: String::new(),
        daemon_label: None,
        venues: 0,
        key_changed: Vec::new(),
        lost: Vec::new(),
        path_up: Vec::new(),
        mounts: Vec::new(),
    };

    /// How the account `route_key` stands.
    pub fn link_of(&self, route_key: &str) -> AccountLink<'_> {
        match self.lost.iter().find(|l| l.route_key == route_key) {
            Some(lost) => AccountLink::Lost(lost),
            None if self.path_up.iter().any(|k| k == route_key) => AccountLink::Up,
            None => AccountLink::NotKnown,
        }
    }

    /// Read one frame.
    pub fn of(w: &CoreSnapshotWire) -> Self {
        let running = running_tiers(w);
        let mut venues: Vec<&str> =
            w.venues.iter().filter(|b| runs(b, w)).map(|b| b.venue.as_str()).collect();
        venues.sort_unstable();
        venues.dedup();
        NodeWords {
            daemon_label: w.identity.as_ref().map(|id| identity_label(id, &running)),
            running,
            venues: venues.len(),
            key_changed: w.key_changed.clone(),
            lost: w
                .venues
                .iter()
                .filter(|b| runs(b, w))
                .filter_map(|b| lost_account(b, w))
                .collect(),
            path_up: w.venue_path_up.clone(),
            mounts: Vec::new(),
        }
    }
}

/// Block `b`'s account, when the frame says it is cut off: the first hop that is down wins, so a lost
/// trade link is not also blamed on the venue past it.
fn lost_account(b: &VenueBlockWire, w: &CoreSnapshotWire) -> Option<LostAccount> {
    let on = |word, why| LostAccount { route_key: b.route_key.clone(), word, why };
    if let Some(link) = w.trade_link_down.iter().find(|l| l.route_key == b.route_key) {
        return Some(on("NO LINK", trade_link_why(link)));
    }
    if let Some(path) = w.venue_link_down.iter().find(|d| d.route_key == b.route_key) {
        let word = if path.no_adapter { "NO ADAPTER" } else { "VENUE DOWN" };
        return Some(on(word, venue_path_why(path)));
    }
    let feed = w.price_link_down.iter().find(|p| p.venue == b.venue)?;
    Some(on("NO PRICES", price_link_why(feed)))
}

/// The node's words for why, or that it gave none.
fn said(why: &str) -> &str {
    if why.is_empty() { "no reason given" } else { why }
}

fn trade_link_why(link: &TradeLinkDownWire) -> String {
    let why = said(&link.why);
    if link.closed {
        format!(
            "The node's link to the gatewayhub for this account is closed for good ({why}): no order \
             of it reaches the venue, and nothing redials."
        )
    } else {
        format!(
            "The node's link to the gatewayhub for this account is down ({why}), redialling: no order \
             of it reaches the venue until it is back."
        )
    }
}

/// `VenueLinkDownWire::say`, the one wording of that outage, as a sentence of its own.
fn venue_path_why(path: &VenueLinkDownWire) -> String {
    let words = path.say();
    let mut chars = words.chars();
    let first = chars.next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    format!("{first}{}.", chars.as_str())
}

fn price_link_why(feed: &PriceLinkDownWire) -> String {
    format!(
        "This venue's price feed to the node is down ({}): its strategies hear no quote, tick or bar.",
        said(&feed.why)
    )
}

/// Whether the node runs the account behind block `b`. ⚠ A STOPPED engine keeps its published block
/// (decision 0169 item 5), so a block does not say the account runs: the node's own list does, by
/// route key alone (`CoreSnapshotWire::stopped_row`, the CLI's and the node's own rule), because a tier
/// change lists the account under its NEW tier beside the block's OLD one.
fn runs(b: &VenueBlockWire, w: &CoreSnapshotWire) -> bool {
    w.stopped_row(&b.route_key).is_none()
}

/// The tag of a daemon: the tiers of the accounts it runs NOW, `LIVE` in capitals by design so it is
/// impossible to miss, and only when a live account runs: `demo`, `LIVE + demo`, `paper`.
///
/// ⚠ It is NOT `HubIdentityWire::live`, which is the daemon's own switch (`flags.tradehub_live`). That
/// switch on, with only demo accounts running, read `tradehub [LIVE]` beside a demo-only strategy. A
/// tier the node did not say reads `tier ?`, never paper (decision 0156's rule for an unknown tier).
pub fn running_tiers(w: &CoreSnapshotWire) -> String {
    let tiers: Vec<Option<AccountTier>> =
        w.venues.iter().filter(|b| runs(b, w)).map(|b| map_tier(b.tier.as_deref())).collect();
    if tiers.is_empty() {
        return "nothing running".to_string();
    }
    let words = [
        (Some(AccountTier::Live), "LIVE"),
        (Some(AccountTier::Demo), "demo"),
        (Some(AccountTier::Paper), "paper"),
        (None, "tier ?"),
    ];
    let said: Vec<&str> =
        words.iter().filter(|(tier, _)| tiers.contains(tier)).map(|(_, word)| *word).collect();
    said.join(" + ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::TradingState;
    use vike_tradehub_client::wire::{ArmingNotRunningViewWire, HubIdentityWire};

    fn block(venue: &str, route_key: &str, tier: Option<&str>) -> VenueBlockWire {
        VenueBlockWire {
            account_code: None,
            order_client_id_shape: None,
            venue: venue.to_string(),
            balance: 0.0,
            realized_pnl: 0.0,
            fees_paid: 0.0,
            funding_paid: 0.0,
            equity: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_buying_power: 0.0,
            trading_state: TradingState::Active,
            positions: Vec::new(),
            account: None,
            route_key: route_key.to_string(),
            symbols: Vec::new(),
            tier: tier.map(str::to_string),
            contract_values: Vec::new(),
            admits_symbols: false,
            quote_rates: Vec::new(),
            quote_rates_in: String::new(),
            ledger_currency: String::new(),
            currency_assumed: false,
            balance_is_own_book: false,
        }
    }

    fn frame(venues: Vec<VenueBlockWire>) -> CoreSnapshotWire {
        CoreSnapshotWire {
            venues,
            identity: Some(HubIdentityWire {
                name: "tradehub".into(),
                strategy: "spread_maker".into(),
                params: String::new(),
                // The daemon's switch is ON in every row below, and must never be what the tag says.
                live: true,
                build: String::new(),
                advertise_addr: String::new(),
            }),
            ..CoreSnapshotWire::empty()
        }
    }

    /// ⚠ The screenshot's defect: the daemon's switch on, only demo accounts running, `[LIVE]`.
    #[test]
    fn the_tag_names_what_runs_not_the_daemons_switch() {
        let demo = frame(vec![block("bybit", "bybit", Some("demo"))]);
        assert_eq!(running_tiers(&demo), "demo");
        assert_eq!(NodeWords::of(&demo).daemon_label.as_deref(), Some("tradehub [demo]"));
        let both = frame(vec![
            block("okx", "okx", Some("paper")),
            block("bybit", "bybit#MAIN", Some("live")),
            block("bybit", "bybit", Some("demo")),
        ]);
        assert_eq!(running_tiers(&both), "LIVE + demo + paper", "the loud word first");
    }

    fn stopped(route_key: &str, tier: Option<&str>) -> ArmingNotRunningViewWire {
        ArmingNotRunningViewWire {
            venue: route_key.into(),
            account: None,
            route_key: route_key.into(),
            tier: tier.map(str::to_string),
            reason: "no credentials".into(),
            detail: None,
            block_word: None,
        }
    }

    /// A stopped engine keeps its block: a live account the node lists as not running is not what
    /// runs.
    #[test]
    fn a_stopped_account_is_not_in_the_tag_or_the_count() {
        let mut w = frame(vec![
            block("binance", "binance", Some("live")),
            block("bybit", "bybit", Some("demo")),
        ]);
        w.not_running = vec![stopped("binance", None)];
        assert_eq!(running_tiers(&w), "demo");
        assert_eq!(NodeWords::of(&w).venues, 1);
    }

    /// ⚠ A tier change lists the account under its NEW tier while the stopped engine's block keeps
    /// the OLD one, so the route key alone decides (`CoreSnapshotWire::stopped_row`): a match on the tier
    /// as well read that stopped engine as running, counted it and blamed its dead link.
    #[test]
    fn a_tier_change_reads_the_stopped_engine_as_stopped() {
        let mut w =
            frame(vec![block("bybit", "bybit", Some("demo")), block("okx", "okx", Some("paper"))]);
        w.trade_link_down = vec![trade_link("bybit", true)];
        w.not_running = vec![stopped("bybit", Some("live"))];
        assert_eq!(running_tiers(&w), "paper");
        let words = NodeWords::of(&w);
        assert_eq!(words.venues, 1);
        assert!(words.lost.is_empty(), "not one more lost account: {:?}", words.lost);
    }

    /// An unsaid or unknown tier is unknown, never paper; a node that runs nothing says that.
    #[test]
    fn an_unsaid_tier_is_unknown_and_nothing_running_says_so() {
        assert_eq!(running_tiers(&frame(vec![block("okx", "okx", None)])), "tier ?");
        assert_eq!(running_tiers(&frame(vec![block("okx", "okx", Some("sim"))])), "tier ?");
        assert_eq!(running_tiers(&frame(Vec::new())), "nothing running");
    }

    /// ⚠ `0 venues` on a node with 11 engines: the count is venues the node runs an account on, not
    /// the desktop's market-data feeds, and two accounts of a venue are one venue.
    #[test]
    fn the_count_is_the_venues_the_node_runs() {
        let w = frame(vec![
            block("bybit", "bybit", Some("demo")),
            block("bybit", "bybit#SUB", Some("demo")),
            block("binance", "binance", Some("demo")),
            block("binance", "binance#PERP", Some("demo")),
            block("okx", "okx", Some("demo")),
        ]);
        assert_eq!(NodeWords::of(&w).venues, 3);
        assert_eq!(NodeWords::of(&frame(Vec::new())).venues, 0);
    }

    fn trade_link(route_key: &str, closed: bool) -> TradeLinkDownWire {
        TradeLinkDownWire {
            route_key: route_key.into(),
            why: "connection reset".into(),
            down_ms: 60_000,
            closed,
        }
    }

    fn venue_path(route_key: &str, no_adapter: bool) -> VenueLinkDownWire {
        VenueLinkDownWire {
            route_key: route_key.into(),
            why: "bridge panicked".into(),
            failed_restarts: 0,
            retry_in_ms: 0,
            effect: String::new(),
            no_adapter,
        }
    }

    /// Each hop reads as itself, the first one down wins, and `venue_path_up` is the only thing that
    /// says "up": the same account with nothing said reads "not known".
    #[test]
    fn a_lost_or_adapterless_account_reads_as_such_and_only_the_list_says_up() {
        let mut w = frame(vec![
            block("binance", "binance", Some("demo")),
            block("bybit", "bybit", Some("demo")),
            block("okx", "okx", Some("demo")),
            block("deribit", "deribit", Some("demo")),
            block("hyperliquid", "hyperliquid", Some("demo")),
            block("polymarket", "polymarket", Some("demo")),
        ]);
        w.trade_link_down = vec![trade_link("binance", false), trade_link("bybit", true)];
        // A lost trade link is not also blamed on the venue past it.
        w.venue_link_down = vec![
            venue_path("binance", false),
            venue_path("okx", true),
            venue_path("deribit", false),
        ];
        w.price_link_down = vec![PriceLinkDownWire {
            venue: "hyperliquid".into(),
            why: String::new(),
            down_ms: 1,
            of: 2,
        }];
        w.venue_path_up = vec!["polymarket".into()];
        let words = NodeWords::of(&w);
        let lost = |key: &str| match words.link_of(key) {
            AccountLink::Lost(l) => (l.word, l.why.clone()),
            other => panic!("{key} reads {other:?}"),
        };
        assert_eq!(lost("binance").0, "NO LINK");
        assert!(lost("binance").1.contains("redialling"), "{}", lost("binance").1);
        assert!(lost("bybit").1.contains("closed for good"), "{}", lost("bybit").1);
        assert_eq!(lost("okx").0, "NO ADAPTER");
        assert!(
            lost("okx").1.starts_with("The gatewayhub has NO LIVE ADAPTER"),
            "{}",
            lost("okx").1
        );
        assert_eq!(lost("deribit").0, "VENUE DOWN");
        assert!(lost("deribit").1.starts_with("The venue path is DOWN"), "{}", lost("deribit").1);
        assert_eq!(lost("hyperliquid").0, "NO PRICES");
        assert!(lost("hyperliquid").1.contains("no reason given"), "{}", lost("hyperliquid").1);
        assert_eq!(words.link_of("polymarket"), AccountLink::Up);
        assert_eq!(words.link_of("nobody"), AccountLink::NotKnown);
        assert_eq!(words.lost.len(), 5);
    }

    /// An account the node does not run is NOT RUNNING, its own state: not one more lost account.
    #[test]
    fn a_stopped_account_is_not_counted_lost() {
        let mut w = frame(vec![block("binance", "binance", Some("demo"))]);
        w.trade_link_down = vec![trade_link("binance", true)];
        w.not_running = vec![stopped("binance", None)];
        assert!(NodeWords::of(&w).lost.is_empty());
    }
}
