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
/// `mounts`, on a node that dials no price feed for some mount's venue (`HubIdentityWire::prices_venue`: a paper-only
/// node, `flags.tradehub_live` off, prices only the keyless venues it wired): each such mount is a WARN item, one row,
/// in the node's own words (`vike_tradehub_client::wire::no_price_feed`): no bar or quote reaches it, so it never trades.
/// No row when every mount's venue is priced, nor on a node with no identity.
pub(super) fn no_price_feed_mount_check(
    identity: Option<&vike_tradehub_client::wire::HubIdentityWire>,
    mounts: &[MountViewWire],
) -> Option<Check> {
    use vike_tradehub_client::wire::no_price_feed;
    let identity = identity?;
    let unpriced: Vec<&MountViewWire> =
        mounts.iter().filter(|m| !identity.prices_venue(&m.venue)).collect();
    if unpriced.is_empty() {
        return None;
    }
    let detail = format!(
        "{} mount(s) get no price on this node (it prices only the keyless venues it wired), so none of them ever \
         trades and every manual order on their venue is refused. A credentialed venue is priced only with \
         flags.tradehub_live on: vike-cli config set flags.tradehub_live true (then restart; it also runs the node's \
         demo and live accounts at their tiers)",
        unpriced.len()
    );
    let mut items: Vec<String> = unpriced
        .iter()
        .take(MAX_ITEMS)
        .map(|m| {
            format!(
                "{} {}/{} {}: {}",
                m.mount_id,
                m.venue,
                m.symbol,
                m.interval,
                no_price_feed(&m.venue)
            )
        })
        .collect();
    if unpriced.len() > MAX_ITEMS {
        items.push(format!("... and {} more", unpriced.len() - MAX_ITEMS));
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
                     be told from an older node's. Ask: vike-cli trade reconcile ls --node {node}"
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
            "{} held for an operator ({kinds}). List: vike-cli trade reconcile ls --node {node}. \
             Resolve one (read the venue first): vike-cli trade reconcile confirm <id> --node \
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

/// The core's health counters ([`CoreSnapshotWire::counters`]). A venue event refused for a NaN or an infinity is FAIL:
/// no venue sends one, so it has no innocent reading. Every other counter is information (each that moved is an item
/// of a PASS row): each has an innocent reading (latest-wins conflation, a reconnect replay, an order of another
/// session, a restore), and one reading of a counter cannot tell a rise from history.
pub(super) fn counters_check(snap: &CoreSnapshotWire) -> Check {
    let Some(block) = &snap.counters else {
        return unknown_old(
            "counters",
            "the core's health counters (events dropped, orders restored)",
            &box_log("-u vike-tradehub", " | grep NON-FINITE"),
        );
    };
    let moved: Vec<String> = map_counters(block)
        .named()
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(name, count)| format!("{name} {count}"))
        .collect();
    let row = match block.events_dropped_number_not_finite {
        0 => Check::new(Status::Pass, "counters", "no venue event carried a NaN or an infinity"),
        n => Check::new(
            Status::Fail,
            "counters",
            format!(
                "{n} venue event(s) carried a NaN or an infinity and were dropped: no venue sends one, so \
                 a venue fault or a compromised feed. Reconcile the venue"
            ),
        ),
    };
    row.items(moved)
}

/// An ERROR line newer than this, before the frame was framed, means the count is still rising.
const RECENT_ERROR_MS: i64 = vike_model::MS_PER_HOUR;

/// The tradehub's own ERROR and WARN lines since it started ([`CoreSnapshotWire::log_lines`], one row per target). An
/// ERROR line in the last [`RECENT_ERROR_MS`] of the frame is a WARN: the count is rising and a person may need to act.
/// Never a FAIL: the rows that judge the trading say whether it stopped. Older ERROR lines and every WARN line are
/// information, items of a PASS row; each item is a target, the loudest first.
pub(super) fn log_lines_check(snap: &CoreSnapshotWire) -> Check {
    let ask = box_log("-u vike-tradehub", " --since <boot> | grep ' ERROR '");
    let Some(rows) = &snap.log_lines else {
        return unknown_old(
            "log-lines",
            "the tradehub's ERROR and WARN lines since it started",
            &ask,
        );
    };
    let errors: u64 = rows.iter().map(|r| r.errors).sum();
    let warnings: u64 = rows.iter().map(|r| r.warnings).sum();
    let latest = rows.iter().map(|r| r.latest_error_ms).max().unwrap_or(0);
    let mut loudest: Vec<_> = rows.iter().collect();
    loudest.sort_by_key(|r| std::cmp::Reverse((r.errors, r.warnings)));
    let mut items: Vec<String> = loudest
        .iter()
        .take(MAX_ITEMS)
        .map(|r| format!("{}: {} ERROR, {} WARN", r.target, r.errors, r.warnings))
        .collect();
    if loudest.len() > MAX_ITEMS {
        items.push(format!("... and {} more", loudest.len() - MAX_ITEMS));
    }
    if errors == 0 {
        let detail = format!("no ERROR line since the node started ({warnings} WARN)");
        return Check::new(Status::Pass, "log-lines", detail).items(items);
    }
    // The node's own clock on both sides: `published_ms` and each row's time are stamped by the same process.
    let ago = (latest > 0 && snap.published_ms >= latest).then(|| snap.published_ms - latest);
    let said = format!("{errors} ERROR line(s) since the node started ({warnings} WARN)");
    match ago {
        Some(ms) if ms >= RECENT_ERROR_MS => {
            let newest = span(u64::try_from(ms).unwrap_or(0));
            let detail =
                format!("{said}, none in the last hour (the newest {newest} before this frame)");
            Check::new(Status::Pass, "log-lines", detail).items(items)
        }
        _ => {
            items.push(format!("read them: {ask}"));
            let detail = format!("{said}, the newest within the last hour: the count is rising");
            Check::new(Status::Warn, "log-lines", detail).items(items)
        }
    }
}

/// The alerts that failed every ask to their webhook target since the node started
/// ([`CoreSnapshotWire::alert_deliveries_failed`]). Any is a WARN until the node restarts: each was an alert the
/// operator was never sent, and its `ALERT:` line in the node's log is the only copy. Never a FAIL: trading goes on.
pub(super) fn alert_deliveries_check(snap: &CoreSnapshotWire) -> Check {
    let ask = box_log("-u vike-tradehub", " | grep 'alert NOT delivered'");
    match snap.alert_deliveries_failed {
        None => unknown_old("alerts-unsent", "the alerts that never reached their webhook target", &ask),
        Some(0) => Check::new(
            Status::Pass,
            "alerts-unsent",
            "no alert failed to reach its webhook target since the node started",
        ),
        Some(n) => Check::new(
            Status::Warn,
            "alerts-unsent",
            format!(
                "{n} alert(s) failed every ask to their webhook target since the node started: never sent, so read \
                 them in the node's log"
            ),
        )
        .items(vec![format!("read them: {ask}")]),
    }
}

/// The tradehub journal's size and growth at the node's last measure ([`CoreSnapshotWire::journal`]). WARN while the
/// node judges it over either alert row: every record past the last snapshot is replayed at the next start, so a flood
/// slows the restart. Never a FAIL: trading goes on. PASS otherwise.
pub(super) fn journal_size_check(snap: &CoreSnapshotWire) -> Check {
    let ask = box_log("-u vike-tradehub", " | grep 'the journal is'");
    let Some(journal) = snap.journal else {
        return unknown_old(
            "journal-size",
            "the tradehub journal's size and growth (a current node says them from its first measure, a minute at \
             most after its start, when a `config.journal_dir` row names its journal)",
            &ask,
        );
    };
    let mib = |bytes: u64| format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0));
    let rate = journal
        .bytes_per_hour
        .map_or("at a rate not measured yet".to_string(), |rate| format!("{} an hour", mib(rate)));
    let said =
        format!("{} in {} segment(s), growing {rate}", mib(journal.total_bytes), journal.segments);
    if !journal.over {
        return Check::new(Status::Pass, "journal-size", format!("{said}: under its alert rows"));
    }
    let detail = format!(
        "OVER its alert rows (`config.journal_size_alert_mib`, `config.journal_growth_alert_mib_per_hour`): {said}; \
         every record past the last snapshot is replayed at the next start"
    );
    Check::new(Status::Warn, "journal-size", detail)
        .items(vec![format!("read the watch's lines: {ask}")])
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
                vike_utility::number_text::format_number(p.size),
                vike_utility::number_text::format_number(p.average_price),
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
    // The tradehub's own lines are on its frame (`log-lines`); the other two daemons publish none to it.
    let mut out = vec![unknown(
        "journal",
        "the gatewayhub's and the datahub's ERROR lines since they started",
        &box_log("-u vike-gatewayhub -u vike-datahub", " --since <boot> | grep ' ERROR '"),
    )];
    if accounts
        .iter()
        .any(|a| vike_model::accounts::account_keys::venue_of_route_key(&a.key) == "hyperliquid")
    {
        out.push(unknown(
            "leverage",
            "the leverage an account states and whether the exchange confirmed it",
            &format!(
                "vike-cli account ls, and {}",
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
