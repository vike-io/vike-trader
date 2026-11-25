//! `vike-cli trade links [ACCOUNT]` — how long each venue session lived, how it ended and how long the next took to open, as
//! data, for every venue: what until now only `journalctl` lines said, and they rotate.
//!
//! The read and the sum are `crates/vike-client/src/links.rs`'s. This file is the verb: its command line and its table.

use std::process::ExitCode;

use vike_client::links::{LinkSummary, ask_links, document, summarize};
use vike_datahub_client::gateway_links::GatewayLinksReply;
use vike_hub_protocol::auth::HubKeys;
use vike_model::LinkSessionRow;
use vike_model::accounts::account_keys::route_key_of;
use vike_model::time::{Span, epoch_ms_to_utc_timestamp, parse_span};

use super::selector;
use crate::cmd::args;
use crate::exit::Exit;

/// The program name a refusal of this verb prints under.
const BIN: &str = "vike-cli trade links";

/// How many of a book's sessions the table lists; `--json` has every one read.
const LAST_ROWS: usize = 20;

/// `--help`'s prose (the `usage:` line and the options table are `clap`'s).
pub(super) const LONG_ABOUT: &str = "\
Every venue's sessions, summed per account: how many, how long they lived (median and longest), how
many dropped, the median wait until the next one opened, and how the last one ended. Each venue's
price feeds sum as one line. With an ACCOUNT, that account's last sessions, one per line.

A drop is a session we did not end ourselves: a stop or a restart of ours is listed and not counted.
A session with no end recorded is open, or its gatewayhub stopped without saying: `trade health` tells
which, because this table cannot. A hub killed outright has its rows closed at its next start, at the
last time it wrote and of no known length, so they count in no life figure. A session counts in --since
if it was open at any time in the span.

Read-only, under this box's device key, from the config.datahub_addr row's address. The datahub reads
the settings database it already opens, so this answers with the gatewayhub down, and it asks no venue.
Rows are kept 90 days. Not recorded: fxcm's trade session (its state stays in the vendor's library).";

// The `trade links` command line, as `clap` reads it.
#[derive(clap::Args, Debug)]
pub(super) struct LinksCommand {
    #[arg(
        value_name = "ACCOUNT",
        help = "`<venue>/<LABEL>`, one account, or `<venue>`, every book of it: their last sessions too"
    )]
    book: Option<String>,
    #[arg(
        long,
        value_name = "SPAN",
        default_value = "7d",
        help = "how far back: a session counts if it was open at any time in this span (90m, 6h, 7d, 2w)"
    )]
    since: String,
    #[arg(long, overrides_with = "json", help = "the report as one JSON document, for scripts")]
    json: bool,
}

/// The resolved `trade links` command line.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    /// `<venue>/<LABEL>` or `<venue>/DEFAULT`: the one account.
    route: Option<String>,
    /// A bare `<venue>`: every book of it. A read may show them all, and a venue can have only labelled ones.
    venue: Option<String>,
    span_ms: i64,
    json: bool,
}

impl LinksCommand {
    fn into_args(self) -> Result<Args, String> {
        let (mut route, mut venue) = (None, None);
        if let Some(raw) = &self.book {
            let book = selector::parse(raw).map_err(|e| e.message)?;
            if raw.contains('/') {
                route = Some(route_key_of(&book.venue, &book.label));
            } else {
                venue = Some(book.venue);
            }
        }
        let span_ms = match parse_span(&self.since).map_err(|e| format!("--since: {e}"))? {
            Span::Ms(ms) => ms,
            Span::Months(_) | Span::Bars(_) => {
                return Err(format!(
                    "--since: `{}` is not a fixed span here: use minutes, hours, days or weeks (90m, 6h, 7d, 2w)",
                    self.since
                ));
            }
        };
        Ok(Args { route, venue, span_ms, json: self.json })
    }
}

/// `trade links`'s argv tail through the plane's one parser, then its rules.
#[cfg(test)]
fn parse(argv: impl IntoIterator<Item = String>) -> Result<Args, args::Stop> {
    parse_trade_verb!(argv, "links", Links, BIN)
}

/// A span in the unit it reads best in: a reconnect is seconds and tenths, a session minutes.
fn seconds(s: Option<f64>) -> String {
    match s {
        None => "-".to_string(),
        Some(s) if s < 120.0 => format!("{s:.1}s"),
        // Whole milliseconds of a figure a store computed from two clocks: nothing to round wrongly here.
        Some(s) => vike_client::health::span((s * 1000.0) as u64),
    }
}

fn summary_table(summaries: &[LinkSummary]) -> String {
    let venue_w = summaries.iter().map(|s| s.venue.len()).chain(["VENUE".len()]).max().unwrap_or(0);
    let account_w =
        summaries.iter().map(|s| s.account.len()).chain(["ACCOUNT".len()]).max().unwrap_or(0);
    let mut out = format!(
        "{:<venue_w$}  {:<account_w$}  PLANE  SESSIONS  NO END YET  MEDIAN   LONGEST  DROPS  RECONNECT  LAST CLOSE",
        "VENUE", "ACCOUNT"
    );
    for s in summaries {
        out.push_str(&format!(
            "\n{:<venue_w$}  {:<account_w$}  {:<5}  {:<8}  {:<10}  {:<7}  {:<7}  {:<5}  {:<9}  {}",
            s.venue,
            s.account,
            s.plane,
            s.sessions,
            s.no_end_yet,
            seconds(s.median_life_s),
            seconds(s.longest_life_s),
            s.drops,
            seconds(s.median_reconnect_s),
            s.last_close.as_deref().unwrap_or("-"),
        ));
    }
    out
}

fn rows_table(rows: &[LinkSessionRow]) -> String {
    let route_w = rows.iter().map(|r| r.route.len()).chain(["ACCOUNT".len()]).max().unwrap_or(0);
    let mut out =
        format!("OPENED (UTC)          {:<route_w$}  LIVED    NEXT IN   HOW IT ENDED", "ACCOUNT");
    for row in rows.iter().take(LAST_ROWS) {
        let how = match (&row.how, row.ours) {
            // This table cannot tell a session that runs from one whose hub stopped without saying.
            (None, _) => "no end recorded yet: open, or the gatewayhub stopped".to_string(),
            (Some(how), true) => format!("{how} (ours)"),
            (Some(how), false) => how.clone(),
        };
        out.push_str(&format!(
            "\n{:<20}  {:<route_w$}  {:<7}  {:<8}  {how}",
            epoch_ms_to_utc_timestamp(row.opened_ms),
            row.route,
            seconds(row.lived_s),
            seconds(row.next_open_s),
        ));
    }
    out
}

/// The whole screen. A read that found nothing says what that means.
fn human(reply: &GatewayLinksReply, book: bool, since: &str) -> String {
    if reply.rows.is_empty() {
        return format!(
            "no recorded venue session was open in the last {since}. A longer --since reaches further back, and a bare \
             venue lists every book of it"
        );
    }
    let mut out = summary_table(&summarize(&reply.rows));
    if book {
        out.push_str("\n\n");
        out.push_str(&rows_table(&reply.rows));
    }
    if reply.truncated {
        out.push_str("\n\nonly the newest sessions were read: narrow it with ACCOUNT or --since");
    }
    out
}

/// Entry point [`crate::cmd::trade::run`] routes `trade links` to.
pub(super) fn run(
    cmd: LinksCommand,
    datahub_addr: Option<&str>,
    datahub_keys: &dyn Fn() -> Option<HubKeys>,
) -> ExitCode {
    let since = cmd.since.clone();
    let parsed = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse(BIN, msg),
    };
    let addr = vike_client::resolved::datahub_dial_address(None, datahub_addr);
    let since_ms = vike_model::now_ms() - parsed.span_ms;
    let book = parsed.route.is_some() || parsed.venue.is_some();
    match ask_links(&addr, datahub_keys().as_ref(), since_ms, parsed.route, parsed.venue) {
        Ok(reply) if parsed.json => {
            let doc = document(since_ms, &reply, book);
            println!(
                "{}",
                serde_json::to_string_pretty(&doc)
                    .expect("a tree of strings, numbers, arrays and nulls; serialization is total")
            );
            ExitCode::SUCCESS
        }
        Ok(reply) => {
            println!("{}", human(&reply, book, &since));
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("{BIN}: {why}");
            Exit::Failed.into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(ToString::to_string).collect()
    }

    fn row(
        route: &str,
        opened_ms: i64,
        lived: f64,
        next: Option<f64>,
        how: &str,
    ) -> LinkSessionRow {
        LinkSessionRow {
            venue: vike_model::accounts::account_keys::venue_of_route_key(route).to_string(),
            route: route.to_string(),
            plane: "trade".to_string(),
            lane: "pump".to_string(),
            opened_ms,
            closed_ms: Some(opened_ms + (lived * 1000.0) as i64),
            lived_s: Some(lived),
            how: Some(how.to_string()),
            ours: false,
            next_open_s: next,
        }
    }

    #[test]
    fn a_book_becomes_a_route_key_and_since_a_span() {
        let a = parse(argv(&["bybit/DEMO", "--since", "6h", "--json"])).expect("parses");
        assert_eq!(
            a,
            Args {
                route: Some("bybit#DEMO".to_string()),
                venue: None,
                span_ms: 6 * 3_600_000,
                json: true
            }
        );
        let none = parse(argv(&[])).expect("parses");
        assert_eq!((none.route, none.venue, none.span_ms), (None, None, 7 * 86_400_000));
        // `bybit` has only labelled books: its bare word must find them, and `<venue>/DEFAULT` is the one account.
        let bare = parse(argv(&["bybit"])).expect("parses");
        assert_eq!((bare.route, bare.venue.as_deref()), (None, Some("bybit")));
        let default = parse(argv(&["aster/DEFAULT"])).expect("parses");
        assert_eq!((default.route.as_deref(), default.venue), (Some("aster"), None));
    }

    #[test]
    fn a_span_that_is_no_fixed_time_and_a_route_key_are_refused() {
        assert!(parse(argv(&["--since", "3mo"])).is_err());
        assert!(parse(argv(&["--since", "nope"])).is_err());
        assert!(parse(argv(&["bybit#DEMO"])).is_err(), "one addressing grammar: `/`");
    }

    /// ⚠ **The sample the owner reads**, both screens, shown under `--nocapture`.
    #[test]
    fn sample_of_trade_links() {
        let t = 1_794_000_000_000;
        let reply = GatewayLinksReply {
            rows: vec![
                row("hyperliquid", t + 1_000_000, 640.0, Some(1.5), "server closed: 1000"),
                row("hyperliquid", t, 602.0, Some(31.0), "server closed: 1000"),
                row(
                    "aster",
                    t + 2_000_000,
                    906.0,
                    Some(1.5),
                    "the venue said the listenKey expired",
                ),
                row("aster", t, 240.0, Some(1.5), "no frame from the venue for 60 s"),
            ],
            truncated: false,
        };
        let all = human(&reply, false, "7d");
        println!("{all}");
        let mut lines = all.lines();
        assert!(lines.next().is_some_and(|h| h.starts_with("VENUE") && h.contains("LAST CLOSE")));
        let aster = lines.next().expect("aster");
        assert!(aster.starts_with("aster"), "{aster}");
        assert!(
            aster.contains("15m 6s") && aster.contains("the venue said the listenKey expired"),
            "{aster}"
        );
        let hyperliquid = lines.next().expect("hyperliquid");
        assert!(hyperliquid.contains("16.2s") || hyperliquid.contains("16.3s"), "{hyperliquid}");

        let one = GatewayLinksReply { rows: reply.rows[..2].to_vec(), truncated: false };
        let screen = human(&one, true, "7d");
        println!("\n{screen}");
        assert!(screen.contains("OPENED (UTC)") && screen.contains("31.0s"), "{screen}");
    }

    #[test]
    fn nothing_in_the_window_blames_no_build_and_a_row_with_no_end_claims_nothing() {
        let empty = GatewayLinksReply { rows: Vec::new(), truncated: false };
        let said = human(&empty, false, "7d");
        assert!(said.contains("no recorded venue session was open in the last 7d"), "{said}");
        assert!(said.contains("A longer --since"), "{said}");
        assert!(
            !said.contains("older") && !said.contains("gatewayhub"),
            "a cause it did not check: {said}"
        );

        let open = LinkSessionRow {
            closed_ms: None,
            lived_s: None,
            how: None,
            ..row("aster", 1, 0.0, None, "")
        };
        let ours =
            LinkSessionRow { ours: true, ..row("aster", 1, 1.0, None, "stopped by this process") };
        let table = rows_table(&[open, ours]);
        assert!(
            table.contains("no end recorded yet")
                && table.contains("stopped by this process (ours)"),
            "{table}"
        );
        assert!(!table.contains("open now"), "the hub may be down: {table}");
    }
}
