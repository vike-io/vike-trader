//! The READ verbs, rendered off the node's published frame (`CoreSnapshotWire`, one
//! `Request::Snapshot` per read) — the one part of this channel where a bug means an ugly message,
//! not a wrong order. Pure and allocation-only.
//!
//! The text is the text the tradehub's in-process channel printed off its own snapshot cell before
//! this channel left that daemon (decision 0256): the frame carries the newest block of each account
//! (the node's `current_venue_blocks`), each block's `ledger_currency` and `balance_is_own_book`, the
//! node's `primary_currency`, the order statuses as `OrderStatus::as_str` words and the two lists the
//! publisher stamps (`key_changed`, `balance_withheld`), which is every fact these renders read.
//!
//! ⚠ **Every number these verbs print describes the WHOLE account, never one engine.** That is the
//! same scope rule `vike-tradehub`'s `summary_line` states — *a field may not be narrower than
//! `equity`'s* — and it is a correctness property rather than a preference, because the frame's
//! top-level scalars (`trading_state`, `balance`, `positions`) are the PRIMARY engine's while the
//! blocks span every engine. On a the CI box CEX node the primary is the binance PAPER engine, which has
//! traded nothing, so `/status` and `/equity` once answered an operator with `realized=0.00
//! fees=0.00 positions=0` while the bybit mount took the fills that moved the equity beside them. An
//! operator who asks a chat bot "how am I doing" is exactly the reader who cannot check the source.

use vike_tradehub_client::wire::{
    BalanceWithheldWire, CoreSnapshotWire, KEY_CHANGED_WARNING, VenueBlockWire, quote_rate_phrase,
};
use vike_utility::math::fmt_number;

use super::{MAX_ROWS, ReadVerb};

/// One currency group's figures, folded by `vike_model::ledger_sums` (the law the node's own
/// `AccountLedgerTotals::of` folds by), over the frame's blocks of that group, in the frame's order.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ledger {
    /// The engines that keep their own book (`balance_is_own_book`, the node's `BalanceMode::Delta`).
    equity_book: f64,
    /// The engines whose balance is the venue's attested wallet (`BalanceMode::Authoritative`).
    equity_wallet: f64,
    realized_pnl: f64,
    fees_paid: f64,
    funding_paid: f64,
    margin_used: f64,
}

fn money(v: &VenueBlockWire) -> vike_model::AccountLedgerBlock {
    vike_model::AccountLedgerBlock {
        equity: v.equity,
        realized_pnl: v.realized_pnl,
        fees_paid: v.fees_paid,
        funding_paid: v.funding_paid,
        unrealized: v.unrealized,
        margin_used: v.margin_used,
    }
}

/// The group `currency` (EMPTY = the dollar group, the wire's spelling).
fn ledger(snap: &CoreSnapshotWire, currency: &str) -> Ledger {
    let group: Vec<&VenueBlockWire> =
        snap.venues.iter().filter(|v| v.ledger_currency == currency).collect();
    let equity_on = |own_book: bool| {
        let on_mode = group.iter().filter(|v| v.balance_is_own_book == own_book);
        vike_model::ledger_sums(on_mode.map(|v| money(v))).equity
    };
    let all = vike_model::ledger_sums(group.iter().map(|v| money(v)));
    Ledger {
        equity_book: equity_on(true),
        equity_wallet: equity_on(false),
        realized_pnl: all.realized_pnl,
        fees_paid: all.fees_paid,
        funding_paid: all.funding_paid,
        margin_used: all.margin_used,
    }
}

/// A group key as text: the empty dollar key reads `"USD"`, any other is itself (the node's
/// `AccountLedgerTotals::label_of`).
fn label_of(currency: &str) -> &str {
    if currency.is_empty() { "USD" } else { currency }
}

/// ` (venue1,venue2)` naming the venues behind the wallet figure of group `currency`, or the EMPTY string
/// when nothing has ever attested a balance — so a pure-paper node's `/equity` keeps a clean
/// `equity_wallet=0.00` with no dangling parenthesis.
///
/// ⚠ This exists because [`render_read`] used to print ONE `equity_total` — the `py_sum` of every
/// venue block's equity, which mixes the daemon's own book-keeping (`BalanceMode::Delta`) with a
/// venue's attested whole-account wallet (`Authoritative`). See `vike-tradehub`'s `summary_line`
/// doc for the 62647.10600813 that measured on the CI box: 53647 of a SHARED bybit demo wallet plus
/// nine paper mounts' 1000 seed, printed to an operator as one equity figure.
fn wallet_attribution_in(snap: &CoreSnapshotWire, currency: &str) -> String {
    let venues: Vec<&str> = snap
        .venues
        .iter()
        .filter(|v| v.ledger_currency == currency && !v.balance_is_own_book)
        .map(|v| v.venue.as_str())
        .collect();
    if venues.is_empty() { String::new() } else { format!(" ({})", venues.join(",")) }
}

/// The currency groups OTHER than the primary's, first-seen order: empty for a node whose engines
/// share a currency, which keeps every report below byte-identical for it (decision 0183, item 13).
fn other_ledger_currencies(snap: &CoreSnapshotWire) -> Vec<&str> {
    let mut groups: Vec<&str> = Vec::new();
    for v in &snap.venues {
        if v.ledger_currency != snap.primary_currency
            && !groups.contains(&v.ledger_currency.as_str())
        {
            groups.push(&v.ledger_currency);
        }
    }
    groups
}

/// Whether an order still works. The frame carries `OrderStatus::as_str`'s word (an older node the
/// Debug spelling); a word neither reads as is listed as working rather than hidden.
fn is_working(status: &str) -> bool {
    vike_model::OrderStatus::parse(status).is_none_or(|s| !s.is_terminal())
}

/// The status as this report always printed it (`OrderStatus`'s Debug spelling, `Accepted`), or the
/// node's own word when it is not one this build knows.
fn status_word(status: &str) -> String {
    vike_model::OrderStatus::parse(status).map_or_else(|| status.to_string(), |s| format!("{s:?}"))
}

/// Render a read verb off the node's frame. Pure and allocation-only.
///
/// ⚠ The equity figures are SPLIT by provenance (`equity book=` / `wallet=`) — see
/// [`wallet_attribution_in`]. Every OTHER figure is cross-venue by construction: the
/// `realized`/`fees`/`funding`/`positions`/`margin_used`/`unpriced` numbers fold every block of the
/// group, never the primary-only scalar fields beside them. Two quantities that CANNOT honestly be
/// totalled are named per venue instead — see [`balance_by_venue`] and [`state_line`].
pub fn render_read(verb: ReadVerb, snap: &CoreSnapshotWire) -> String {
    let primary = ledger(snap, &snap.primary_currency);
    let mut out = match verb {
        ReadVerb::Status => {
            let working = snap.orders.iter().filter(|o| is_working(&o.status)).count();
            let positions: usize = snap.venues.iter().map(|v| v.positions.len()).sum();
            format!(
                "seq={} state={}\norders={} (working {})\npositions={}\nequity book={:.2} wallet={:.2}{}\nrealized={:.2} fees={:.2}{}\nfault={}",
                snap.sequence,
                state_line(snap),
                snap.orders.len(),
                working,
                positions,
                primary.equity_book,
                primary.equity_wallet,
                wallet_attribution_in(snap, &snap.primary_currency),
                primary.realized_pnl,
                primary.fees_paid,
                other_ledgers_status(snap),
                snap.fault.as_deref().unwrap_or("none")
            )
        }
        ReadVerb::Positions => {
            // Every venue's rows, venue-registration order then per-venue account order — NOT
            // `snap.positions`, which is the primary engine's block.
            let open: Vec<_> = snap
                .venues
                .iter()
                .flat_map(|v| v.positions.iter())
                .filter(|p| p.size != 0.0)
                .collect();
            let rows: Vec<String> = open
                .iter()
                .take(MAX_ROWS)
                .map(|p| {
                    format!(
                        "{}/{} size={} avg={} unrl={:.2}",
                        p.venue,
                        p.symbol,
                        fmt_number(p.size),
                        fmt_number(p.average_price),
                        p.unrealized
                    )
                })
                .collect();
            join_rows("no open positions", rows, open.len())
        }
        ReadVerb::Orders => {
            let working: Vec<_> = snap.orders.iter().filter(|o| is_working(&o.status)).collect();
            let rows: Vec<String> = working
                .iter()
                .take(MAX_ROWS)
                .map(|o| {
                    format!(
                        "{} {}/{} {} {} @ {} [{}] filled={}",
                        o.order_client_id,
                        o.venue,
                        o.symbol,
                        if o.side >= 0 { "buy" } else { "sell" },
                        fmt_number(o.qty),
                        o.price.map(fmt_number).unwrap_or_else(|| "mkt".into()),
                        status_word(&o.status),
                        fmt_number(o.filled_qty)
                    )
                })
                .collect();
            join_rows("no working orders", rows, working.len())
        }
        ReadVerb::Equity => format!(
            "equity_book={:.2}\nequity_wallet={:.2}{}\nbalance={}\nrealized={:.2}\nfees={:.2}\nfunding={:.2}\nmargin_used={:.2}\nunpriced_positions={}{}{}",
            primary.equity_book,
            primary.equity_wallet,
            wallet_attribution_in(snap, &snap.primary_currency),
            balance_by_venue(snap),
            primary.realized_pnl,
            primary.fees_paid,
            primary.funding_paid,
            primary.margin_used,
            snap.venues.iter().map(|v| v.missing_prices).sum::<u32>(),
            other_ledgers_equity(snap),
            conversion_rate_lines(snap)
        ),
    };
    // `/status` also names a running account whose key changed in the database (a warning, not a
    // stop), and `/equity` and `/status` an account whose venue balance the node holds back from its
    // engine: both lists are the publisher's, stamped on every frame.
    if verb == ReadVerb::Status {
        out.push_str(&key_changed_lines(&snap.key_changed));
    }
    if matches!(verb, ReadVerb::Equity | ReadVerb::Status) {
        out.push_str(&balance_held_lines(&snap.balance_withheld));
    }
    out
}

/// `/equity`'s conversion-rate lines: one per account that holds a symbol quoted in another currency, in the
/// ONE wording every surface uses (`vike_tradehub_client::wire::quote_rate_phrase`), `STALE` being the
/// engine's own verdict and a stale rate that values an open position saying so (decision 0183). EMPTY for a
/// node with no such account, which is every node of one currency, so its report is unchanged.
fn conversion_rate_lines(snap: &CoreSnapshotWire) -> String {
    let mut out = String::new();
    for v in snap.venues.iter().filter(|v| !v.quote_rates.is_empty()) {
        let rates: Vec<String> = v
            .quote_rates
            .iter()
            .map(|r| quote_rate_phrase(&r.currency, r.rate, r.stale, r.held, &v.quote_rates_in))
            .collect();
        let who = if v.route_key.is_empty() { v.venue.as_str() } else { v.route_key.as_str() };
        out.push_str(&format!("\n{who}: conversion rates: {}", rates.join("; ")));
    }
    out
}

/// `/status`'s money lines for every currency group but the primary's, each under its own `[CCY]` heading
/// and led by one line saying the figures above are the primary's. EMPTY for a node whose engines share a
/// currency, so such a node's report is unchanged. A total across currencies is no money (decision 0183,
/// item 13), so the groups are listed, never added.
fn other_ledgers_status(snap: &CoreSnapshotWire) -> String {
    other_ledgers(snap, |tag, currency| {
        let totals = ledger(snap, currency);
        format!(
            "\n[{tag}] equity book={:.2} wallet={:.2}{}\n[{tag}] realized={:.2} fees={:.2}",
            totals.equity_book,
            totals.equity_wallet,
            wallet_attribution_in(snap, currency),
            totals.realized_pnl,
            totals.fees_paid,
        )
    })
}

/// `/equity`'s lines for every currency group but the primary's, as [`other_ledgers_status`] does for
/// `/status`: one `[CCY]` line per figure, EMPTY for a node of one currency.
fn other_ledgers_equity(snap: &CoreSnapshotWire) -> String {
    other_ledgers(snap, |tag, currency| {
        let totals = ledger(snap, currency);
        format!(
            "\n[{tag}] equity_book={:.2}\n[{tag}] equity_wallet={:.2}{}\n[{tag}] realized={:.2}\n[{tag}] fees={:.2}\n[{tag}] funding={:.2}\n[{tag}] margin_used={:.2}",
            totals.equity_book,
            totals.equity_wallet,
            wallet_attribution_in(snap, currency),
            totals.realized_pnl,
            totals.fees_paid,
            totals.funding_paid,
            totals.margin_used,
        )
    })
}

/// The two renders' shared skeleton: EMPTY for a node whose engines share a currency, otherwise the line
/// saying the figures above are the primary's, then `group(label, currency)` for each other currency group.
fn other_ledgers(snap: &CoreSnapshotWire, group: impl Fn(&str, &str) -> String) -> String {
    let others = other_ledger_currencies(snap);
    if others.is_empty() {
        return String::new();
    }
    let mut out =
        format!("\nledgers: the figures above are {} only", label_of(&snap.primary_currency));
    for currency in others {
        out.push_str(&group(label_of(currency), currency));
    }
    out
}

/// `venue:cash` for every venue block, in registration order — the cash figure NAMED rather than
/// totalled.
///
/// ⚠ There is deliberately no `balance_total` here, and the missing total is the point: a block's
/// `balance` answers a DIFFERENT question per provenance — on an engine that keeps its own book this
/// daemon's own book-keeping, on one that holds the venue's figure the venue's attested wallet for
/// the WHOLE account the credentials open — so the equity halves split by that same provenance rather
/// than summing across it. `snap.balance` — the scalar this used to print — was narrower still: the
/// PRIMARY engine's cash alone, i.e. the untraded binance paper 1000.00 on that same node.
///
/// Falls back to the scalar before the first snapshot build, when `venues` is empty and there is
/// nothing to name.
fn balance_by_venue(snap: &CoreSnapshotWire) -> String {
    if snap.venues.is_empty() {
        return format!("{:.2}", snap.balance);
    }
    snap.venues
        .iter()
        .map(|v| format!("{}:{:.2}", v.venue, v.balance))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The account's trading state: the primary engine's, plus every venue whose own state DISAGREES
/// with it, named.
///
/// The frame's `trading_state` is the primary engine's and nothing else, while each block carries its
/// own, and they diverge for real: a margin-call sweep or a per-mount budget latch moves ONE engine to
/// `Reducing`/`Halted`. An operator asking "am I trading" must not be told `Active` by an untraded
/// paper engine while the venue holding the risk is halted. No aggregate state is invented here — the
/// disagreement is simply reported, so a single-venue node renders byte-identically.
fn state_line(snap: &CoreSnapshotWire) -> String {
    let primary = format!("{:?}", snap.trading_state);
    let diverging: Vec<String> = snap
        .venues
        .iter()
        .filter(|v| v.trading_state != snap.trading_state)
        .map(|v| format!("{}={:?}", v.venue, v.trading_state))
        .collect();
    if diverging.is_empty() { primary } else { format!("{primary} ({})", diverging.join(",")) }
}

/// One line per RUNNING account whose key in the database is no longer the one its session started with
/// (`CoreSnapshotWire::key_changed`: route keys, never a key), each starting on a new line so it can follow
/// `/status`. The EMPTY string when none: a node with no changed key says nothing.
fn key_changed_lines(route_keys: &[String]) -> String {
    route_keys.iter().map(|k| format!("\n{k}: {KEY_CHANGED_WARNING}")).collect()
}

/// One line per RUNNING account whose venue balance the node holds back from its engine
/// ([`BalanceWithheldWire::line`]: the account, the reason in short words and what it means for the equity), each
/// starting on a new line so it can follow `/equity` and `/status`. The EMPTY string when none: a node that holds
/// nothing back says nothing.
fn balance_held_lines(held: &[BalanceWithheldWire]) -> String {
    held.iter().map(|account| format!("\n{}", account.line())).collect()
}

/// Join printed rows, stating explicitly when the list was truncated (never a silent cut).
fn join_rows(empty: &str, rows: Vec<String>, total: usize) -> String {
    if rows.is_empty() {
        return empty.to_string();
    }
    let mut out = rows.join("\n");
    if total > rows.len() {
        out.push_str(&format!("\n… {} more (showing {})", total - rows.len(), rows.len()));
    }
    out
}

#[path = "render_tests.rs"]
#[cfg(test)]
mod render_tests;
