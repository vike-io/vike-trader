//! The rows of [`super::judge`] about what the engines hold and what stands in their way: trading state, duplicates,
//! keys, balance, rates, stops, mounts, reconcile, orders, positions, prices, and what only the box can answer.

use super::*;

pub(super) fn trading_checks(accounts: &[AccountView<'_>]) -> Vec<Check> {
    let mut out = Vec::new();
    let mut active = Vec::new();
    for a in accounts {
        let Some(block) = a.running() else { continue };
        match block.trading_state {
            TradingState::Active => active.push(a.key.as_str()),
            TradingState::Reducing => out.push(
                Check::new(
                    Status::Warn,
                    "trading",
                    "REDUCING: only orders that reduce a position are admitted",
                )
                .scope(&a.key),
            ),
            TradingState::Halted => out.push(
                Check::new(
                    Status::Fail,
                    "trading",
                    "HALTED: no order that opens or adds risk is admitted (covered reduces pass)",
                )
                .scope(&a.key)
                .items(vec![
                    format!(
                        "why: {}",
                        box_log(
                            "-u vike-tradehub",
                            " | grep -E 'LINK DEAD-MAN HALTED THE VENUE|HALT'"
                        )
                    ),
                    "a link dead-man halt lifts by itself when the links are back; any other: \
                     vike-cli trade resume --node <control addr>"
                        .to_string(),
                ]),
            ),
        }
    }
    if !active.is_empty() {
        out.insert(
            0,
            Check::new(
                Status::Pass,
                "trading",
                format!("{} engine(s) Active: {}", active.len(), active.join(", ")),
            ),
        );
    }
    out
}
pub(super) fn duplicate_checks(
    snap: &CoreSnapshotWire,
    accounts: &[AccountView<'_>],
) -> Vec<Check> {
    let mut per_key: BTreeMap<&str, usize> = BTreeMap::new();
    for block in snap.venues.iter().filter(|b| !b.route_key.is_empty()) {
        *per_key.entry(block.route_key.as_str()).or_default() += 1;
    }
    let mut out: Vec<Check> = per_key
        .into_iter()
        .filter(|&(_, n)| n > 1)
        .map(|(key, n)| {
            Check::new(
                Status::Fail,
                "duplicates",
                format!(
                    "{n} blocks share this route key: a consumer that takes the first reads the \
                     STOPPED engine (a current node publishes only the newest)"
                ),
            )
            .scope(key)
        })
        .collect();
    for a in accounts.iter().filter(|a| a.stopped.is_some() && a.block.is_some()) {
        out.push(
            Check::new(
                Status::Warn,
                "duplicates",
                "a stopped engine's block is still published: its equity and positions stay \
                 valued, and no engine manages them",
            )
            .scope(&a.key),
        );
    }
    let unkeyed = snap.venues.iter().filter(|b| b.route_key.is_empty()).count();
    if unkeyed > 0 {
        out.push(Check::new(
            Status::Unknown,
            "duplicates",
            format!(
                "unknown (not on the wire): {unkeyed} block(s) carry no route key (a node that \
                 predates account routing), so a stale duplicate cannot be told from a second \
                 account. Ask: vike-cli trade position ls --node <observe addr>"
            ),
        ));
    }
    if out.is_empty() {
        out.push(Check::new(
            Status::Pass,
            "duplicates",
            "no route key is published twice and no stopped engine keeps a block",
        ));
    }
    out
}
pub(super) fn key_checks(snap: &CoreSnapshotWire) -> Vec<Check> {
    if snap.key_changed.is_empty() {
        return vec![Check::new(Status::Pass, "keys", "no running account reports a changed key")];
    }
    snap.key_changed
        .iter()
        .map(|key| Check::new(Status::Warn, "keys", KEY_CHANGED_WARNING).scope(key))
        .collect()
}
/// One WARN row per account whose venue balance the node holds back from its engine
/// ([`CoreSnapshotWire::balance_withheld`]): its equity is the engine's own book, not the venue's wallet. NO row when none
/// is held: a node that predates the field also publishes none, and a PASS row would claim a fact it cannot know.
pub(super) fn balance_checks(snap: &CoreSnapshotWire) -> Vec<Check> {
    snap.balance_withheld
        .iter()
        .map(|held| Check::new(Status::Warn, "balance", held.say()).scope(&held.route_key))
        .collect()
}
/// One WARN row per RUNNING account whose conversion-rate table has a rate that is stale while a position is
/// open in a symbol quoted in its currency (`QuoteRateViewWire::held`, which a node sets only on a stale row). The
/// position is then valued at the last rate this engine holds and an opening order on that currency is denied; the
/// margin-call and drawdown sweeps keep running on the same last rate. NO row otherwise (a stale rate nobody holds
/// anything in is `trade equity`'s line, and an older node publishes no flag, so a PASS row would claim a fact it
/// cannot know).
pub(super) fn rate_checks(accounts: &[AccountView<'_>]) -> Vec<Check> {
    accounts
        .iter()
        .filter_map(|a| {
            let block = a.running()?;
            let rates: Vec<String> = block
                .quote_rates
                .iter()
                .filter(|r| r.stale && r.held)
                .map(|r| r.say(&block.quote_rates_in))
                .collect();
            if rates.is_empty() {
                return None;
            }
            Some(
                Check::new(
                    Status::Warn,
                    "rate",
                    "a conversion rate is stale while a position is open in its currency: the \
                     position is valued at the last rate, an opening order on that currency is \
                     denied (quote-rate-stale) and a reduce is not. Look at the rates: the `equity` \
                     word of the `vike-cli trade --node <observe addr>` REPL",
                )
                .scope(&a.key)
                .items(rates),
            )
        })
        .collect()
}
/// `stops`: the operator's armed CORE-HELD reduce-only stops, the reduce-only trigger rows of the ONE emulated-order
/// list ([`CoreSnapshotWire::core_held_stops_listed`], decisions 0199's amendment and 0275; never the older
/// `core_held_stops` field, which stays for MCP). They are held by THE NODE, so `orders` (the venue's registry) never
/// counts them. PASS while all are armed and watching, WARN while any is HELD for its account's first reconcile pass
/// or was RE-ARMED after a close failed. NO row when none is listed: a node that predates the list also lists none,
/// and a PASS row would claim what it cannot know (`trade order stops` carries the empty-list sentence).
pub(super) fn stop_checks(snap: &CoreSnapshotWire, node: &str) -> Option<Check> {
    let stops = snap.core_held_stops_listed();
    if stops.is_empty() {
        return None;
    }
    let held = stops.iter().filter(|s| s.held).count();
    let rearmed = stops.iter().filter(|s| s.rearms > 0).count();
    let mut detail = format!(
        "{} core-held reduce-only stop(s) armed, each {CORE_HELD_STOP_MEANING}",
        stops.len()
    );
    if held > 0 {
        detail.push_str(&format!(
            "; {held} HELD until its account's first reconcile pass (restored from the journal, not firing yet)"
        ));
    }
    if rearmed > 0 {
        detail.push_str(&format!(
            "; {rearmed} RE-ARMED because the close it sent was refused or filled only in part"
        ));
    }
    detail.push_str(&format!(". List: vike-cli trade order stops --node {node}"));
    let mut items: Vec<String> =
        stops.iter().take(MAX_ITEMS).map(OrderEmulationStopViewWire::say).collect();
    if stops.len() > MAX_ITEMS {
        items.push(format!("... and {} more", stops.len() - MAX_ITEMS));
    }
    let status = if held > 0 || rearmed > 0 { Status::Warn } else { Status::Pass };
    Some(Check::new(status, "stops", detail).items(items))
}
/// `mounts`: a mount whose budget latch is spent (decision 0239) is a WARN, one row for all of them with one item per
/// mount: it breached its loss or notional budget, its orders were cancelled and its new orders are discarded, and it
/// stays so until an operator clears it with `trade strategy reset-budget` once it is flat. NO row when none is latched: a
/// node that predates the flag also writes none, and a PASS row would claim what it cannot know.
pub(super) fn mount_checks(mounts: &[MountViewWire]) -> Option<Check> {
    let latched: Vec<&MountViewWire> = mounts.iter().filter(|m| m.latched).collect();
    if latched.is_empty() {
        return None;
    }
    let detail = format!(
        "{} mount(s) latched liquidate-only by their budget: no new order of theirs reaches a venue until an \
         operator clears each, once it is flat. Clear one: vike-cli trade strategy reset-budget <mount> (the id is \
         the first word of the item)",
        latched.len()
    );
    let mut items: Vec<String> = latched
        .iter()
        .take(MAX_ITEMS)
        .map(|m| format!("{} {}/{} {}", m.mount_id, m.venue, m.symbol, m.interval))
        .collect();
    if latched.len() > MAX_ITEMS {
        items.push(format!("... and {} more", latched.len() - MAX_ITEMS));
    }
    Some(Check::new(Status::Warn, "mounts", detail).items(items))
}
pub(super) fn reconcile_check(
    snap: &CoreSnapshotWire,
    node: &str,
    features: Option<&[String]>,
) -> Check {
    if snap.reconcile.is_empty() {
        return match features.map(|f| f.iter().any(|s| s == FEATURE_RECONCILE_VERBS)) {
            Some(true) => Check::new(Status::Pass, "recon", "nothing is held for an operator"),
            Some(false) => Check::new(
                Status::Unknown,
                "recon",
                format!(
                    "unknown: this node does not advertise \"{FEATURE_RECONCILE_VERBS}\", so it \
                     publishes no held rows and an empty list proves nothing. Ask: {}",
                    box_log("-u vike-tradehub", " | grep 'reconcile divergence'")
                ),
            ),
            None => Check::new(
                Status::Unknown,
                "recon",
                format!(
                    "unknown: the node's capabilities could not be read, so an empty list cannot \
                     be told from an older node's. Ask: vike-cli trade recon ls --node {node}"
                ),
            ),
        };
    }
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for row in &snap.reconcile {
        *kinds.entry(row.kind.as_str()).or_default() += 1;
    }
    let kinds = kinds.iter().map(|(k, n)| format!("{k} x{n}")).collect::<Vec<_>>().join(", ");
    let items = snap
        .reconcile
        .iter()
        .flat_map(|r| {
            [
                format!(
                    "#{} {} {}: {}",
                    r.id,
                    r.account,
                    r.kind,
                    r.detail.replace(['\r', '\n'], " ")
                ),
                format!("   a confirm: {}", effect_of(&r.kind, r.proposed_event_count)),
            ]
        })
        .collect();
    Check::new(
        Status::Warn,
        "recon",
        format!(
            "{} held for an operator ({kinds}). List: vike-cli trade recon ls --node {node}. \
             Resolve one (read the venue first): vike-cli trade recon confirm <id> --node \
             <control addr>",
            snap.reconcile.len()
        ),
    )
    .items(items)
}
/// Whether an order status word (the FSM word or an older node's Debug spelling) still rests.
fn is_resting(status: &str) -> bool {
    !matches!(
        status.to_ascii_lowercase().as_str(),
        "filled" | "canceled" | "cancelled" | "rejected" | "denied" | "expired"
    )
}
pub(super) fn order_checks(snap: &CoreSnapshotWire) -> Vec<Check> {
    let resting: Vec<&OrderViewWire> =
        snap.orders.iter().filter(|o| is_resting(&o.status)).collect();
    let Some(oldest) = resting.first() else {
        return vec![Check::new(Status::Pass, "orders", "no order is resting")];
    };
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    for order in &resting {
        *by_status.entry(order.status.to_ascii_uppercase()).or_default() += 1;
    }
    let by_status =
        by_status.iter().map(|(s, n)| format!("{s} x{n}")).collect::<Vec<_>>().join(", ");
    vec![
        Check::new(
            Status::Pass,
            "orders",
            format!(
                "{} resting ({by_status}); first in registry order: {} {} {}",
                resting.len(),
                oldest.order_client_id,
                oldest.venue,
                oldest.symbol
            ),
        ),
        order_age_check(&resting),
    ]
}
/// The age of the oldest resting order, from [`OrderViewWire::age_ms`]. The node counts from the
/// moment it first SAW the order, so after a restart the figure is at most the uptime: an order
/// that rested through the restart reads as old as the process, which is what the node can know.
/// Orders carrying no age are ignored; when none carries one the node predates the field. `resting`
/// is never empty.
fn order_age_check(resting: &[&OrderViewWire]) -> Check {
    let aged = resting.iter().filter_map(|o| o.age_ms.map(|age| (age, *o)));
    let Some((age, order)) = aged.max_by_key(|(age, _)| *age) else {
        return unknown_old(
            "order-age",
            "how old the oldest resting order is",
            &format!(
                "{}, or the venue's open-orders page",
                box_log(
                    "-u vike-tradehub",
                    &format!(" | grep -F '{}'", resting[0].order_client_id)
                )
            ),
        );
    };
    let which = format!("{} {} {}", order.order_client_id, order.venue, order.symbol);
    if age > STALE_ORDER_MS {
        Check::new(
            Status::Warn,
            "order-age",
            format!(
                "the oldest resting order is {} old (over 24h): {which}. Counted from when this \
                 node first saw it, so a restart resets the clock. Look at it: vike-cli trade \
                 order ls --node <observe addr>",
                span(age)
            ),
        )
    } else {
        Check::new(
            Status::Pass,
            "order-age",
            format!("the oldest resting order is {} old: {which}", span(age)),
        )
    }
}
/// The HALT sentinel file, from [`CoreSnapshotWire::halt_sentinel`]. Present is a FAIL: while it exists
/// every adapter refuses an order that opens or adds risk, whatever the engines' state says, and
/// nothing clears it but an operator removing the file.
pub(super) fn halt_check(snap: &CoreSnapshotWire) -> Check {
    match snap.halt_sentinel {
        Some(true) => Check::new(
            Status::Fail,
            "halt-sentinel",
            "the HALT sentinel file EXISTS: every adapter refuses an order that opens or adds \
             risk, whatever the engines' state (cancels and covered reduces still pass)",
        )
        .items(vec![
            "it does not clear itself: remove the file once the cause is dealt with \
             (docs/ops/kill-switches.md)"
                .to_string(),
            format!(
                "its path: {}",
                box_log("-u vike-tradehub", " | grep 'HALT sentinel path resolved'")
            ),
        ]),
        Some(false) => Check::new(Status::Pass, "halt-sentinel", "no HALT sentinel file"),
        None => unknown_old(
            "halt-sentinel",
            "whether the HALT sentinel file exists (adapters refuse orders while it does, \
             whatever the engines' state)",
            &format!(
                "{}, then ls that path",
                box_log("-u vike-tradehub", " | grep 'HALT sentinel path resolved'")
            ),
        ),
    }
}
pub(super) fn position_checks(accounts: &[AccountView<'_>]) -> Vec<Check> {
    let mut items = Vec::new();
    let mut unmanaged = 0usize;
    for a in accounts {
        let Some(block) = a.block else { continue };
        for p in block.positions.iter().filter(|p| p.size != 0.0) {
            let leverage =
                if p.leverage > 0.0 { format!("{:.1}x", p.leverage) } else { "-".to_string() };
            let stopped = a.stopped.is_some();
            unmanaged += usize::from(stopped);
            items.push(format!(
                "{} {} {} {} @ {} (unrealized {:.2}, leverage {leverage}){}",
                a.key,
                p.symbol,
                p.position_side,
                vike_utility::math::fmt_number(p.size),
                vike_utility::math::fmt_number(p.average_price),
                p.unrealized,
                if stopped { "  <- account NOT RUNNING: nothing manages it" } else { "" }
            ));
        }
    }
    if items.is_empty() {
        return vec![Check::new(Status::Pass, "positions", "flat: no non-flat position")];
    }
    let total = items.len();
    if total > MAX_ITEMS {
        items.truncate(MAX_ITEMS);
        items.push(format!("... and {} more", total - MAX_ITEMS));
    }
    let (status, tail) = if unmanaged > 0 {
        (Status::Warn, format!(", {unmanaged} on an account that is NOT RUNNING"))
    } else {
        (Status::Pass, String::new())
    };
    vec![Check::new(status, "positions", format!("{total} non-flat{tail}")).items(items)]
}
pub(super) fn price_checks(accounts: &[AccountView<'_>]) -> Vec<Check> {
    let mut out = Vec::new();
    for a in accounts {
        let Some(block) = a.running() else { continue };
        if block.missing_prices > 0 {
            out.push(
                Check::new(
                    Status::Fail,
                    "prices",
                    format!(
                        "{} open position(s) have no priceable source: equity, margin and the \
                         risk sweeps cannot value them",
                        block.missing_prices
                    ),
                )
                .scope(&a.key),
            );
        }
    }
    if out.is_empty() {
        out.push(Check::new(Status::Pass, "prices", "every open position has a price source"));
    }
    out.push(unknown(
        "price-feeds",
        "whether each price-only feed is live now (a priced position can carry a stale quote, and \
         a venue with nothing open proves nothing)",
        &box_log("-u vike-datahub -u vike-tradehub", " | grep -i -E 'strat|runs no price feed'"),
    ));
    out
}
/// The facts that live only on the node's box or in its journal.
pub(super) fn box_only_checks(accounts: &[AccountView<'_>]) -> Vec<Check> {
    let mut out = vec![unknown(
        "journal",
        "ERROR lines since the node started",
        &box_log(
            "-u vike-tradehub -u vike-gatewayhub -u vike-datahub",
            " --since <boot> | grep ' ERROR '",
        ),
    )];
    if accounts.iter().any(|a| a.key.split('#').next() == Some("hyperliquid")) {
        out.push(unknown(
            "leverage",
            "the leverage an account states and whether the exchange confirmed it",
            &format!(
                "vike-cli secrets accounts, and {}",
                box_log("-u vike-gatewayhub", " | grep -i leverage")
            ),
        ));
        out.push(unknown(
            "hl-approval",
            "when a Hyperliquid API-wallet approval ends",
            &box_log("-u vike-gatewayhub", " | grep 'TRADE-AGENT:'"),
        ));
    }
    out
}
