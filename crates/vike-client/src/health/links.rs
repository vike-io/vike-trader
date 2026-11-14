//! The `gateway-link` and `price-link` rows of [`super::judge`]: the links the tradehub publishes, one hop each, and
//! each account's session with its venue past the gatewayhub.

use super::*;

/// The engines that hold a trade lease: every one that runs, but a `paper` account's.
pub(super) fn leased_blocks<'a>(accounts: &[AccountView<'a>]) -> Vec<&'a VenueBlockWire> {
    accounts
        .iter()
        .filter_map(AccountView::running)
        .filter(|b| b.tier.as_deref() != Some("paper"))
        .collect()
}
/// Whether the node publishes its hub links at all (`None`: its capabilities could not be read), which is what lets
/// an empty list mean "every link is up".
fn publishes_hub_links(features: Option<&[String]>) -> Option<bool> {
    features.map(|f| f.iter().any(|s| s == FEATURE_HUB_LINKS))
}
/// The row for an empty list on a node that does not say it publishes one.
fn links_not_judged(name: &'static str, what: &str, ask: &str, known: Option<bool>) -> Vec<Check> {
    match known {
        Some(false) => vec![unknown_old(name, what, ask)],
        _ => vec![Check::new(
            Status::Unknown,
            name,
            format!(
                "unknown: the node's capabilities could not be read, so an empty list cannot be told \
                 from an older node's: {what}. Ask: {ask}"
            ),
        )],
    }
}
/// `gateway-link`: the two hops past the tradehub, the trade link to the gatewayhub and the gatewayhub's session with the
/// venue. Each row names its hop and claims nothing past it.
///
/// * NO row with no account that wants a lease and nothing lost (a paper-only box has no link to claim).
/// * Every lease redialling is FAIL `GATEWAYHUB DOWN`: all of them losing their link at once is what that looks like
///   from here, and every engine still reads RUNNING. One lease down while another is up is WARN: the gatewayhub
///   answers, so it is that lease.
/// * A lease closed for good is FAIL, one row per account, however many others are up: like `Halted` it leaves the
///   account with no way to open or close risk, and unlike it nothing heals it.
/// * All up is PASS, for that hop alone. Past it, one row per account ([`venue_session_checks`]).
///
/// Every lease lost while the datahub holds a report from the gatewayhub is not `GATEWAYHUB DOWN`: the daemon answers, so
/// the row says the fault is on this tradehub's side of it. One that takes a handshake and gives no report is no such
/// proof.
pub(super) fn gateway_link_checks(
    accounts: &[AccountView<'_>],
    links: &[TradeLinkDownWire],
    features: Option<&[String]>,
    hubs: Option<&HubLeg>,
    down: &[VenueLinkDownWire],
    up: &[String],
) -> Vec<Check> {
    let leased = leased_blocks(accounts);
    if leased.is_empty() && links.is_empty() {
        return venue_session_checks(accounts, links, down, up);
    }
    let known = publishes_hub_links(features);
    if links.is_empty() && known != Some(true) {
        return links_not_judged(
            "gateway-link",
            "whether this node's trade links to the gatewayhub are up",
            &format!(
                "for bybit, deribit, hyperliquid or polymarket, vike-cli trade venue <book> balance --node <observe addr> \
                 (it rides the lease; every other venue answers `not offered`); for every venue, {}",
                box_log("-u vike-tradehub", " | grep TRADE-LINK")
            ),
            known,
        );
    }
    let total = leased.len().max(links.len());
    let (closed, redialling): (Vec<_>, Vec<_>) = links.iter().partition(|l| l.closed);
    let mut out: Vec<Check> = closed
        .iter()
        .map(|l| Check::new(Status::Fail, "gateway-link", trade_link_words(l)).scope(&l.route_key))
        .collect();
    if redialling.len() + closed.len() == total && !redialling.is_empty() {
        let longest = redialling.iter().map(|l| l.down_ms).max().unwrap_or(0);
        let lost = format!(
            "{} of {total} trade links lost for {} ({}), so no order reaches a venue",
            redialling.len(),
            span(longest),
            said(&redialling[0].why)
        );
        let on_its_box = format!("On its box: {}", box_status("vike-gatewayhub"));
        let detail = match hubs.map(|leg| &leg.answer) {
            Some(DatahubAnswer::Said(reply)) if reply.report.is_some() => format!(
                "ALL TRADE LINKS LOST: {lost}, and the engines still read RUNNING. The datahub still reaches the \
                 gatewayhub, so the fault is on this tradehub's side of it: a key, a version or the network. On its \
                 box: {}",
                box_log("-u vike-tradehub", " | grep TRADE-LINK")
            ),
            Some(DatahubAnswer::Said(reply)) if reply.reached => format!(
                "GATEWAYHUB DOWN: {lost}, and the engines still read RUNNING. It took the datahub's handshake and gave \
                 no report, so it looks wedged. {on_its_box}"
            ),
            Some(DatahubAnswer::Said(_)) => format!(
                "GATEWAYHUB DOWN: {lost}, and the engines still read RUNNING. The datahub cannot reach it either. \
                 {on_its_box}"
            ),
            _ => format!(
                "GATEWAYHUB DOWN: {lost}. The tradehub sees only its own links, and the engines still read RUNNING. \
                 {on_its_box}"
            ),
        };
        out.push(
            Check::new(Status::Fail, "gateway-link", detail).items(
                redialling
                    .iter()
                    .map(|l| {
                        format!("{}: down for {} ({})", l.route_key, span(l.down_ms), said(&l.why))
                    })
                    .collect(),
            ),
        );
    } else {
        let up = total - links.len();
        out.extend(redialling.iter().map(|l| {
            Check::new(
                Status::Warn,
                "gateway-link",
                format!(
                    "{}. {up} other trade link(s) are up, so the gatewayhub answers: this is this lease",
                    trade_link_words(l)
                ),
            )
            .scope(&l.route_key)
        }));
    }
    if links.is_empty() {
        out.push(Check::new(
            Status::Pass,
            "gateway-link",
            format!(
                "{total} trade link(s) to the gatewayhub are up: orders reach the gatewayhub, and this row \
                 says nothing past it"
            ),
        ));
    }
    out.extend(venue_session_checks(accounts, links, down, up));
    out
}
/// Whether an account's NOT RUNNING row says only that the operator's own switch is off: read off its block's KEY, which
/// no rewording moves. A row with none (an older node's, or a stop no block names) is read off the badge, as before.
pub(super) fn switched_off(n: &ArmingNotRunningViewWire) -> bool {
    match n.block_word {
        Some(block) => block == ArmingBlock::AccountInactive,
        None => n.reason.starts_with(ArmingBlock::AccountInactive.as_str()),
    }
}
/// **The gatewayhub's session with the venue, one row per account**, in what the frame says of it.
///
/// * Switched off (`account.active` = 0): PASS, no session and none wanted. Stopped for any other reason: WARN, no session,
///   in the node's words. Their `engine` rows already count; this row only says what that means for this hop.
/// * The lease has no live adapter: FAIL, the same weight as a closed lease, for the same reason.
/// * The bridge says its path is down: WARN, its words, as on the `engine` row.
/// * The bridge says it is up: PASS. It speaks for the path it reports (a private stream, a sidecar, an account session),
///   not for every socket the venue's bridge holds.
/// * Otherwise UNKNOWN, never OK: no fresh answer, or a venue whose bridge reports no such path
///   ([`VENUES_REPORTING_A_PATH`]). An account whose trade link is lost is judged by the row above and has none here.
fn venue_session_checks(
    accounts: &[AccountView<'_>],
    links: &[TradeLinkDownWire],
    down: &[VenueLinkDownWire],
    up: &[String],
) -> Vec<Check> {
    const NAME: &str = "gateway-link";
    let ask = &format!(
        "{}; for bybit, deribit, hyperliquid or polymarket also vike-cli trade venue <book> balance --node <observe addr> \
         (a read over that session; every other venue answers `not offered`)",
        box_log("-u vike-gatewayhub", " | grep TRADE-LINK")
    );
    let session = |a: &AccountView<'_>| -> Option<Check> {
        if let Some(n) = a.stopped {
            if n.tier.as_deref() == Some("paper") {
                return None;
            }
            return Some(if switched_off(n) {
                Check::new(
                    Status::Pass,
                    NAME,
                    format!(
                        "no session with the venue, and none is wanted: {}, the operator's switch",
                        not_running_words(n)
                    ),
                )
            } else {
                Check::new(
                    Status::Warn,
                    NAME,
                    format!("no session with the venue: {}", not_running_words(n)),
                )
            });
        }
        let block = a.running().filter(|b| b.tier.as_deref() != Some("paper"))?;
        if links.iter().any(|l| l.route_key == a.key) {
            return None;
        }
        Some(if let Some(link) = down.iter().find(|d| d.route_key == a.key) {
            let status = if link.no_adapter { Status::Fail } else { Status::Warn };
            Check::new(status, NAME, link.say())
        } else if up.contains(&a.key) {
            Check::new(
                Status::Pass,
                NAME,
                "the gatewayhub's adapter is up and its bridge says its path to the venue is up",
            )
        } else if VENUES_REPORTING_A_PATH.contains(&block.venue.as_str()) {
            unknown(
                NAME,
                "whether the gatewayhub's session with the venue is up: the node holds no fresh answer from the \
                 gatewayhub (it just started, it is starting the adapter again, it predates this reading, or the \
                 venue's bridge cannot vouch for its session yet)",
                ask,
            )
        } else {
            unknown(
                NAME,
                "whether the gatewayhub's session with the venue is up: this venue's bridge reports no such path",
                ask,
            )
        })
    };
    accounts.iter().filter_map(|a| session(a).map(|check| check.scope(&a.key))).collect()
}
/// `price-link`: each price feed's strategy lane to the datahub. A feed that wants no lane (it holds nothing and the
/// datahub closed its idle session) is not on the list, and a PASS is a connection, not a fresh price: the
/// `price-feeds` row stays UNKNOWN. NO row with no lease and nothing lost.
///
/// Every lane lost is FAIL `DATAHUB unreachable`: the tradehub takes every price through it, so the strategies are
/// blind. One venue's lane lost while another is up is WARN: the datahub answers.
pub(super) fn price_link_checks(
    accounts: &[AccountView<'_>],
    links: &[PriceLinkDownWire],
    features: Option<&[String]>,
    dials_up: Option<u32>,
) -> Vec<Check> {
    let Some(first) = links.first() else {
        if leased_blocks(accounts).is_empty() {
            return Vec::new();
        }
        let known = publishes_hub_links(features);
        if known != Some(true) {
            return links_not_judged(
                "price-link",
                "whether this node's price feeds are connected to the datahub",
                &box_log("-u vike-tradehub", " | grep 'not connected'"),
                known,
            );
        }
        // Every dial idle: nothing is mounted, so no session was read, and a dead datahub reads exactly like this.
        if dials_up == Some(0) {
            return vec![Check::new(
                Status::Unknown,
                "price-link",
                "unknown: no price feed holds a session with the datahub now, so none can have lost one and this says \
                 nothing of it. Ask: vike-cli backend ping --addr <datahub addr>",
            )];
        }
        return vec![Check::new(
            Status::Pass,
            "price-link",
            "no price feed has lost its lane to the datahub: a connection, not a fresh price (see `price-feeds`)",
        )];
    };
    let of = usize::try_from(first.of).unwrap_or(usize::MAX).max(links.len());
    if links.len() == of {
        let longest = links.iter().map(|l| l.down_ms).max().unwrap_or(0);
        return vec![
            Check::new(
                Status::Fail,
                "price-link",
                format!(
                    "DATAHUB unreachable from the tradehub: {} of {of} price feeds lost for {} ({}), so the \
                     strategies are blind. On its box: {}",
                    links.len(),
                    span(longest),
                    said(&first.why),
                    box_status("vike-datahub")
                ),
            )
            .items(links.iter().map(|l| format!("{}: {}", l.venue, price_link_words(l))).collect()),
        ];
    }
    links
        .iter()
        .map(|l| {
            Check::new(
                Status::Warn,
                "price-link",
                format!(
                    "{}. {} other price feed(s) are up, so the datahub answers: this is this lane",
                    price_link_words(l),
                    of - links.len()
                ),
            )
            .scope(&l.venue)
        })
        .collect()
}
