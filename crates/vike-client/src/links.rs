//! What `vike-cli trade links` reads and how it sums it: the venue sessions the gatewayhub recorded
//! (`vike_secrets::link_sessions`), asked of the datahub, which answers from the settings store it already opens. It links no
//! gatewayhub client (decision 0253), dials no venue, and answers with the gatewayhub down.
//!
//! The sum is pure, so a table and a `--json` document cannot disagree.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::gateway_links::GatewayLinksReply;
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_model::LinkSessionRow;

/// How long the datahub gets: one bounded read of one table, behind at most the store's own lock wait.
const REPLY_WAIT: Duration = Duration::from_secs(10);

/// What the price plane's many symbols sum up as, in the ACCOUNT column: a feed is no account.
pub const PRICE_FEEDS: &str = "(price feeds)";

/// Ask the datahub at `addr`. `Err` is a datahub that could not be asked, in words.
pub fn ask_links(
    addr: &str,
    keys: Option<&HubKeys>,
    since_ms: i64,
    route: Option<String>,
    venue: Option<String>,
) -> Result<GatewayLinksReply, String> {
    let mut client = DatahubClient::connect_with_keys(addr, keys, Scope::Read)
        .map_err(|e| format!("datahub at {addr}: {e}"))?;
    client.set_reply_timeout(Some(REPLY_WAIT));
    client.gateway_links(since_ms, route, venue)
}

/// One account's sessions, summed. On the price plane, one venue's.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkSummary {
    /// The venue.
    pub venue: String,
    /// The account's route key, or [`PRICE_FEEDS`].
    pub account: String,
    /// `trade` or `price`.
    pub plane: String,
    /// Every session read.
    pub sessions: usize,
    /// Those with no end recorded: open, or their gatewayhub stopped without saying. This table cannot tell which.
    pub no_end_yet: usize,
    /// The median life of a closed session, seconds.
    pub median_life_s: Option<f64>,
    /// The longest life of a closed session, seconds.
    pub longest_life_s: Option<f64>,
    /// Closed sessions we did not end ourselves.
    pub drops: usize,
    /// The median wait from a drop to the next session, seconds.
    pub median_reconnect_s: Option<f64>,
    /// How the newest closed session ended.
    pub last_close: Option<String>,
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    match values.len() {
        0 => None,
        n if n % 2 == 1 => Some(values[mid]),
        _ => Some((values[mid - 1] + values[mid]) / 2.0),
    }
}

/// `rows` as they arrive, newest first, summed per account (per venue on the price plane), sorted.
#[must_use]
pub fn summarize(rows: &[LinkSessionRow]) -> Vec<LinkSummary> {
    let mut groups: BTreeMap<(&str, &str, &str), Vec<&LinkSessionRow>> = BTreeMap::new();
    for row in rows {
        let account = if row.plane == "price" { PRICE_FEEDS } else { row.route.as_str() };
        groups.entry((row.venue.as_str(), account, row.plane.as_str())).or_default().push(row);
    }
    groups
        .into_iter()
        .map(|((venue, account, plane), rows)| {
            let closed = || rows.iter().filter(|r| r.closed_ms.is_some());
            let lives = || closed().filter_map(|r| r.lived_s);
            LinkSummary {
                venue: venue.to_string(),
                account: account.to_string(),
                plane: plane.to_string(),
                sessions: rows.len(),
                no_end_yet: rows.len() - closed().count(),
                median_life_s: median(lives().collect()),
                longest_life_s: lives().max_by(f64::total_cmp),
                drops: closed().filter(|r| !r.ours).count(),
                median_reconnect_s: median(
                    closed().filter(|r| !r.ours).filter_map(|r| r.next_open_s).collect(),
                ),
                last_close: closed().next().and_then(|r| r.how.clone()),
            }
        })
        .collect()
}

/// The `--json` document: `since_ms`, `truncated`, `accounts[]`, and with a book its `sessions[]`.
#[must_use]
pub fn document(since_ms: i64, reply: &GatewayLinksReply, book: bool) -> Value {
    let accounts: Vec<Value> = summarize(&reply.rows)
        .iter()
        .map(|s| {
            json!({
                "venue": s.venue,
                "account": s.account,
                "plane": s.plane,
                "sessions": s.sessions,
                "no_end_yet": s.no_end_yet,
                "median_life_s": s.median_life_s,
                "longest_life_s": s.longest_life_s,
                "drops": s.drops,
                "median_reconnect_s": s.median_reconnect_s,
                "last_close": s.last_close,
            })
        })
        .collect();
    let mut doc = json!({"since_ms": since_ms, "truncated": reply.truncated, "accounts": accounts});
    if book {
        doc["sessions"] = serde_json::to_value(&reply.rows).unwrap_or(Value::Null);
    }
    doc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(
        route: &str,
        lived: f64,
        next: Option<f64>,
        how: &str,
        ours: bool,
    ) -> LinkSessionRow {
        LinkSessionRow {
            venue: "hyperliquid".to_string(),
            route: route.to_string(),
            plane: "trade".to_string(),
            lane: "pump".to_string(),
            opened_ms: 1_000,
            closed_ms: Some(2_000),
            lived_s: Some(lived),
            how: Some(how.to_string()),
            ours,
            next_open_s: next,
        }
    }

    /// The owner's own numbers: reconnects of 31 s then 1.5 s, sessions of 240 s then 906 s.
    #[test]
    fn a_summary_says_median_and_longest_life_drops_reconnect_and_the_last_close() {
        let open = LinkSessionRow {
            closed_ms: None,
            lived_s: None,
            how: None,
            ..session("hyperliquid", 0.0, None, "", false)
        };
        let rows = vec![
            open,
            session("hyperliquid", 906.0, Some(1.5), "server closed: 1000 newest", false),
            session("hyperliquid", 240.0, Some(31.0), "server closed: 1006", false),
            session("hyperliquid", 10.0, Some(3.0), "stopped by this process", true),
        ];
        let got = summarize(&rows);
        assert_eq!(got.len(), 1);
        let s = &got[0];
        assert_eq!((s.sessions, s.no_end_yet, s.drops), (4, 1, 2));
        assert_eq!((s.median_life_s, s.longest_life_s), (Some(240.0), Some(906.0)));
        assert_eq!(s.median_reconnect_s, Some(16.25), "our own stop is no reconnect");
        assert_eq!(s.last_close.as_deref(), Some("server closed: 1000 newest"));
    }

    /// Two accounts stay two lines; a price symbol is no account, so a venue's feeds are one line.
    #[test]
    fn accounts_stay_apart_and_a_venues_price_feeds_sum_as_one() {
        let price = |symbol: &str| LinkSessionRow {
            plane: "price".to_string(),
            ..session(symbol, 5.0, None, "gap", false)
        };
        let rows = vec![
            session("hyperliquid", 1.0, None, "a", false),
            session("hyperliquid#LIVE", 2.0, None, "b", false),
            price("BTC"),
            price("ETH"),
        ];
        let got = summarize(&rows);
        let lines: Vec<(&str, &str, usize)> =
            got.iter().map(|s| (s.account.as_str(), s.plane.as_str(), s.sessions)).collect();
        assert_eq!(
            lines,
            vec![
                (PRICE_FEEDS, "price", 2),
                ("hyperliquid", "trade", 1),
                ("hyperliquid#LIVE", "trade", 1)
            ]
        );
    }

    #[test]
    fn the_document_carries_the_rows_only_for_a_book() {
        let reply = GatewayLinksReply {
            rows: vec![session("aster", 1.0, None, "a", false)],
            truncated: true,
        };
        let all = document(5, &reply, false);
        assert_eq!((&all["since_ms"], &all["truncated"]), (&json!(5), &json!(true)));
        assert_eq!(all["accounts"][0]["sessions"], 1);
        assert!(all.get("sessions").is_none());
        assert_eq!(document(5, &reply, true)["sessions"][0]["route"], "aster");
    }
}
