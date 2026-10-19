//! The READ verbs, rendered off the LOSSY published `CoreSnapshot` — the one part of this channel
//! where a bug means an ugly message, not a wrong order. Pure and allocation-only; it never touches
//! the vike-core hot fold.
//!
//! ⚠ **Every number these verbs print describes the WHOLE account, never one engine.** That is the
//! same scope rule `vike-tradehub`'s `summary_line` states — *a field may not be narrower than
//! `equity`'s* — and it is a correctness property rather than a preference, because
//! `vike_core::snapshot::build` binds the top-level scalars from the PRIMARY engine (`let acc =
//! &engine.account`, `positions: top_positions`, `trading_state: engine.trading_state`) while
//! `equity_total` and `orders` span every engine. On a the CI box CEX node the primary is the binance
//! PAPER engine — `vike_tradehub_core::wired_markets` lists binance first — which has traded nothing, so
//! `/status` and `/equity` answered an operator with `realized=0.00 fees=0.00 positions=0` while
//! the bybit mount took the fills that moved the equity beside them. An operator who asks a chat
//! bot "how am I doing" is exactly the reader who cannot check the source.

use vike_exec::{BalanceMode, CoreSnapshot};
use vike_tradehub_client::wire::KEY_CHANGED_WARNING;

use super::{MAX_ROWS, ReadVerb};
use crate::publish::current_venue_blocks;
use crate::summary::{
    current_equity, current_margin_used, current_missing_prices, current_position_count,
    current_total,
};

/// ` (venue1,venue2)` naming the venues behind the wallet figure, or the EMPTY string when
/// nothing has ever attested a balance — so a pure-paper node's `/equity` keeps a clean
/// `equity_wallet=0.00` with no dangling parenthesis.
///
/// ⚠ This exists because [`render_read`] used to print ONE `equity_total` — the `py_sum` of every
/// venue block's equity, which mixes the daemon's own book-keeping (`BalanceMode::Delta`) with a
/// venue's attested whole-account wallet (`Authoritative`). See `vike-tradehub`'s `summary_line`
/// doc for the 62647.10600813 that measured on the CI box: 53647 of a SHARED bybit demo wallet plus
/// nine paper mounts' 1000 seed, printed to an operator as one equity figure. An operator asking
/// a chat bot "what is my equity" is exactly the reader who cannot check the source, so the two
/// quantities are named separately here as well.
fn wallet_attribution(snap: &CoreSnapshot) -> String {
    let venues = crate::summary::current_wallet_venues(snap);
    if venues.is_empty() { String::new() } else { format!(" ({})", venues.join(",")) }
}

/// Render a read verb off a `CoreSnapshot`. Pure and allocation-only — this reads the LOSSY
/// arc-swap publication, never the core fold.
///
/// ⚠ The equity figures are SPLIT by provenance (`equity book=` / `wallet=`) — see
/// [`wallet_attribution`]. Every OTHER figure is cross-venue by construction: the
/// `realized`/`fees`/`funding`/`positions`/`margin_used`/`unpriced` numbers fold the same `py_sum` helpers
/// as `Portfolio::realized_pnl_total`/`fees_paid_total`/`funding_paid_total`/`position_count` over the same
/// `venues` in the same order the equity halves sum, never the primary-only scalar fields beside them — the
/// swap `summary_line` took in #1329, which this report had kept. See the module doc. Two quantities that
/// CANNOT honestly be totalled are named per venue instead — see [`balance_by_venue`] and [`state_line`].
///
/// ⚠ **Every read here walks the CURRENT blocks only** ([`current_venue_blocks`]: the newest of each route key),
/// as the equity halves and the published frame do: the core's list only grows, so an account that stopped and
/// started again has its stopped engine's block ahead of the new one, and a read that walks them all adds that
/// engine's PnL, fees, margin and positions to the new one's, names its stale `Halted` state and prints its
/// cash twice. With nothing superseded every figure is the core's, bit for bit.
pub fn render_read(verb: ReadVerb, snap: &CoreSnapshot) -> String {
    match verb {
        ReadVerb::Status => {
            let working = snap.orders.iter().filter(|o| !o.status.is_terminal()).count();
            format!(
                "seq={} state={}\norders={} (working {})\npositions={}\nequity book={:.2} wallet={:.2}{}\nrealized={:.2} fees={:.2}\nfault={}",
                snap.seq,
                state_line(snap),
                snap.orders.len(),
                working,
                current_position_count(snap),
                current_equity(snap, BalanceMode::Delta),
                current_equity(snap, BalanceMode::Authoritative),
                wallet_attribution(snap),
                current_total(snap, |v| v.realized_pnl),
                current_total(snap, |v| v.fees_paid),
                snap.fault.as_deref().unwrap_or("none")
            )
        }
        ReadVerb::Positions => {
            // Every venue's rows, venue-registration order then per-venue account order — NOT
            // `snap.positions`, which is the primary engine's block cloned (`top_positions`).
            let open: Vec<_> = current_venue_blocks(&snap.portfolio.venues)
                .flat_map(|v| v.positions.iter())
                .filter(|p| p.size != 0.0)
                .collect();
            let rows: Vec<String> = open
                .iter()
                .take(MAX_ROWS)
                .map(|p| {
                    format!(
                        "{}/{} size={} avg={} unrl={:.2}",
                        p.venue, p.symbol, p.size, p.avg_px, p.unrealized
                    )
                })
                .collect();
            join_rows("no open positions", rows, open.len())
        }
        ReadVerb::Orders => {
            let working: Vec<_> = snap.orders.iter().filter(|o| !o.status.is_terminal()).collect();
            let rows: Vec<String> = working
                .iter()
                .take(MAX_ROWS)
                .map(|o| {
                    format!(
                        "{} {}/{} {} {} @ {} [{:?}] filled={}",
                        o.client_order_id,
                        o.venue,
                        o.symbol,
                        if o.side >= 0 { "buy" } else { "sell" },
                        o.qty,
                        o.price.map(|p| p.to_string()).unwrap_or_else(|| "mkt".into()),
                        o.status,
                        o.filled_qty
                    )
                })
                .collect();
            join_rows("no working orders", rows, working.len())
        }
        ReadVerb::Equity => format!(
            "equity_book={:.2}\nequity_wallet={:.2}{}\nbalance={}\nrealized={:.2}\nfees={:.2}\nfunding={:.2}\nmargin_used={:.2}\nunpriced_positions={}",
            current_equity(snap, BalanceMode::Delta),
            current_equity(snap, BalanceMode::Authoritative),
            wallet_attribution(snap),
            balance_by_venue(snap),
            current_total(snap, |v| v.realized_pnl),
            current_total(snap, |v| v.fees_paid),
            current_total(snap, |v| v.funding_paid),
            current_margin_used(snap),
            current_missing_prices(snap)
        ),
    }
}

/// `venue:cash` for every venue block, in registration order — the cash figure NAMED rather than
/// totalled.
///
/// ⚠ There is deliberately no `balance_total` here, and the missing total is the point: it is the
/// exact conflation [`wallet_attribution`] exists to undo, one field over. A block's `balance`
/// answers a DIFFERENT question per `vike_exec::BalanceMode` (see `ExecutionEngine::mode_equity`) —
/// under `Delta` this daemon's own book-keeping, under `Authoritative` the venue's attested wallet
/// for the WHOLE account the credentials open — so `equity_book_total`/`equity_wallet_total` split
/// equity by that same provenance rather than summing across it. `snap.balance` — the scalar this
/// used to print — was narrower still: the PRIMARY engine's cash alone, i.e. the untraded binance
/// paper 1000.00 on that same node.
///
/// Falls back to the scalar before the first snapshot build, when `venues` is empty and there is
/// nothing to name.
fn balance_by_venue(snap: &CoreSnapshot) -> String {
    if snap.portfolio.venues.is_empty() {
        return format!("{:.2}", snap.balance);
    }
    current_venue_blocks(&snap.portfolio.venues)
        .map(|v| format!("{}:{:.2}", v.venue, v.balance))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The account's trading state: the primary engine's, plus every venue whose own state DISAGREES
/// with it, named.
///
/// `CoreSnapshot::trading_state` is `engine.trading_state` — the primary engine's and nothing
/// else — while each `VenueBlock` carries its own, and they diverge for real: a margin-call sweep
/// or a per-mount budget latch moves ONE engine to `Reducing`/`Halted`. An operator asking "am I
/// trading" must not be told `Active` by an untraded paper engine while the venue holding the risk
/// is halted. No aggregate state is invented here (there is no such law in `vike-exec`) — the
/// disagreement is simply reported, so a single-venue node renders byte-identically.
fn state_line(snap: &CoreSnapshot) -> String {
    let primary = format!("{:?}", snap.trading_state);
    let diverging: Vec<String> = current_venue_blocks(&snap.portfolio.venues)
        .filter(|v| v.trading_state != snap.trading_state)
        .map(|v| format!("{}={:?}", v.venue, v.trading_state))
        .collect();
    if diverging.is_empty() { primary } else { format!("{primary} ({})", diverging.join(",")) }
}

/// One line per RUNNING account whose key in the database is no longer the one its session started with
/// (`vike_tradehub_client::wire::WireSnapshot::key_changed`: route keys, never a key), each starting on a new
/// line so it can follow `/status`. The EMPTY string when none: a node with no changed key says nothing.
pub(super) fn key_changed_lines(route_keys: &[String]) -> String {
    route_keys.iter().map(|k| format!("\n{k}: {KEY_CHANGED_WARNING}")).collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use vike_exec::{CoreSnapshot, PositionView, VenueBlock};

    /// One venue block carrying only the fields these renders read.
    fn block(venue: &str, equity: f64, mode: vike_exec::BalanceMode) -> VenueBlock {
        VenueBlock {
            venue: venue.to_string(),
            account: None,
            route_key: venue.to_string(),
            symbol: String::new(),
            extra_symbols: Default::default(),
            tier: None,
            balance: 0.0,
            realized_pnl: 0.0,
            fees_paid: 0.0,
            funding_paid: 0.0,
            balance_mode: mode,
            equity,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_bp: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: vike_model::TradingState::Active,
            contract_values: Default::default(),
            contract_value_default: vike_model::ContractValue::default(),
            admits_symbols: false,
            quote_rates: Vec::new(),
            quote_rates_in: Default::default(),
            colliding_fills_refused: 0,
            fees_unconverted: 0,
            fees_at_stale_rate: 0,
            positions: Vec::new(),
        }
    }

    /// ⚠ The the CI box shape (2026-08-17): nine paper mounts at 1000 seed plus one bybit mount that
    /// adopted a SHARED account's 53647 wallet. `/status` and `/equity` used to answer an operator
    /// with the 62647.11 sum of the two. Both must now name each quantity, and neither may print
    /// the total.
    #[test]
    fn the_read_verbs_never_answer_with_a_wallet_added_to_paper_seed() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let mut venues = vec![block("binance", 1_000.0, vike_exec::BalanceMode::Delta)];
        for v in ["okx", "hyperliquid", "aster", "deribit", "alpaca", "ctrader", "ig", "oanda"] {
            venues.push(block(v, 1_000.0, vike_exec::BalanceMode::Delta));
        }
        venues.push(block("bybit", 53_647.10600813, vike_exec::BalanceMode::Authoritative));
        snap.portfolio.venues = venues;
        snap.portfolio.equity_total =
            vike_model::py_sum(snap.portfolio.venues.iter().map(|v| v.equity));

        for verb in [ReadVerb::Status, ReadVerb::Equity] {
            let out = render_read(verb, &snap);
            assert!(
                !out.contains("62647.11"),
                "{verb:?} must not print the conflated total: {out}"
            );
            assert!(out.contains("9000.00"), "{verb:?} must name the book-kept half: {out}");
            assert!(out.contains("53647.11"), "{verb:?} must name the wallet half: {out}");
            assert!(out.contains("(bybit)"), "{verb:?} must name WHOSE wallet it quoted: {out}");
        }
    }

    /// A running account whose key changed is one calm line (route key, the shared sentence) after
    /// `/status`; no changed key adds no text at all.
    #[test]
    fn key_changed_is_one_line_per_account_and_nothing_when_empty() {
        assert_eq!(key_changed_lines(&[]), "");
        let out = key_changed_lines(&["bybit#ALT".to_string(), "okx".to_string()]);
        assert_eq!(
            out,
            "\nbybit#ALT: key changed in the database - the old key is still in use until a restart\
             \nokx: key changed in the database - the old key is still in use until a restart"
        );
    }

    /// A pure-paper node has no wallet to name, so the attribution must vanish entirely rather
    /// than leave `equity_wallet=0.00 ()` behind.
    #[test]
    fn a_pure_paper_node_renders_a_zero_wallet_with_no_attribution() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![block("binance", 1_000.0, vike_exec::BalanceMode::Delta)];
        let out = render_read(ReadVerb::Equity, &snap);
        assert!(out.contains("equity_book=1000.00"), "{out}");
        assert!(out.contains("equity_wallet=0.00\n"), "no dangling parenthesis: {out}");
    }

    /// One open position row, carrying only the fields `/positions` prints.
    fn pos(venue: &str, symbol: &str, size: f64) -> PositionView {
        PositionView {
            venue: venue.into(),
            symbol: symbol.into(),
            position_side: vike_model::events::PositionSide::Both,
            size,
            avg_px: 100.0,
            unrealized: 3.0,
            mark_source: None,
            leverage: 0.0,
            liq_price: 0.0,
            margin_mode: vike_model::MarginMode::Cross,
            isolated_margin: None,
        }
    }

    /// The OTHER the CI box shape, the one #1329 measured: the PRIMARY engine is the untraded binance
    /// paper mount (`vike_tradehub_core::wired_markets` lists binance first) and every fill happened on the bybit mount
    /// beside it. The top-level scalars `vike_core::snapshot::build` binds from `&engine.account`
    /// therefore all read zero, and `snap.positions` is the primary block's empty vec.
    fn prod2_shaped_snapshot() -> CoreSnapshot {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let mut binance = block("binance", 1_000.0, vike_exec::BalanceMode::Delta);
        binance.balance = 1_000.0;
        let mut bybit = block("bybit", 1_042.25, vike_exec::BalanceMode::Delta);
        bybit.balance = 1_042.25;
        bybit.realized_pnl = 42.25;
        bybit.fees_paid = 1.75;
        bybit.funding_paid = 0.5;
        bybit.positions = vec![pos("bybit", "BTCUSDT", 0.5)];
        snap.portfolio.venues = vec![binance, bybit];
        snap.portfolio.equity_total =
            vike_model::py_sum(snap.portfolio.venues.iter().map(|v| v.equity));
        // Exactly what `vike_core::snapshot::build` publishes for this shape: the primary engine's own
        // numbers, which are silence.
        snap.portfolio.realized_pnl = 0.0;
        snap.portfolio.fees_paid = 0.0;
        snap.portfolio.funding_paid = 0.0;
        snap.positions = Vec::new();
        snap
    }

    /// ⚠ The defect #1329 fixed in `summary_line` and this report kept: four numbers describing the
    /// untraded primary engine while the account traded elsewhere. REDDENS on any read that goes
    /// back to `snap.portfolio.realized_pnl`/`fees_paid`/`funding_paid`/`snap.positions`.
    #[test]
    fn the_read_verbs_report_the_venue_that_traded_not_the_untraded_primary() {
        let snap = prod2_shaped_snapshot();

        let status = render_read(ReadVerb::Status, &snap);
        assert!(status.contains("realized=42.25"), "/status must fold every venue: {status}");
        assert!(status.contains("fees=1.75"), "/status must fold every venue: {status}");
        assert!(status.contains("positions=1"), "/status must count every venue: {status}");

        let equity = render_read(ReadVerb::Equity, &snap);
        assert!(equity.contains("realized=42.25"), "/equity must fold every venue: {equity}");
        assert!(equity.contains("fees=1.75"), "/equity must fold every venue: {equity}");
        assert!(equity.contains("funding=0.50"), "/equity must fold every venue: {equity}");

        let positions = render_read(ReadVerb::Positions, &snap);
        assert!(
            positions.contains("bybit/BTCUSDT size=0.5"),
            "/positions must list every venue's rows: {positions}"
        );
    }

    /// The cash figure is NAMED per venue, never summed across `BalanceMode`s and never the
    /// primary engine's alone — see [`balance_by_venue`].
    #[test]
    fn equity_names_whose_cash_each_balance_is() {
        let equity = render_read(ReadVerb::Equity, &prod2_shaped_snapshot());
        assert!(
            equity.contains("balance=binance:1000.00 bybit:1042.25"),
            "every venue's cash, attributed and untotalled: {equity}"
        );
    }

    /// A halted venue must not be hidden behind an `Active` primary — the one line an operator
    /// reads to answer "am I trading".
    #[test]
    fn status_names_a_venue_whose_trading_state_disagrees_with_the_primary() {
        let mut snap = prod2_shaped_snapshot();
        snap.portfolio.venues[1].trading_state = vike_model::TradingState::Halted;
        let status = render_read(ReadVerb::Status, &snap);
        assert!(status.contains("state=Active (bybit=Halted)"), "{status}");
    }

    /// An account that stopped and started again: the core's list keeps the stopped engine's block AHEAD of the
    /// new one's (same route key), the stopped one frozen with everything it had done, `Halted`, holding a position.
    fn restarted_account_snapshot() -> CoreSnapshot {
        let mut snap = prod2_shaped_snapshot();
        let mut stopped = block("bybit", 1_042.25, vike_exec::BalanceMode::Delta);
        stopped.balance = 1_042.25;
        stopped.realized_pnl = 40.0;
        stopped.fees_paid = 4.0;
        stopped.funding_paid = 2.0;
        stopped.margin_used = 300.0;
        stopped.missing_prices = 2;
        stopped.trading_state = vike_model::TradingState::Halted;
        stopped.positions = vec![pos("bybit", "ETHUSDT", 3.0)];
        snap.portfolio.venues.insert(1, stopped);
        snap
    }

    /// **Every Telegram read describes the NEWEST block of a route key, as the equity beside it does.** A
    /// restarted account's stopped engine used to be added to the new one's realized, fees, funding, margin,
    /// unpriced count, position rows and cash, and its stale `Halted` was named as the account's state; the
    /// equity halves had been fixed alone, so one `/equity` message mixed current-only equity with all-block sums.
    #[test]
    fn a_restarted_accounts_stopped_engine_is_in_none_of_the_reads() {
        let snap = restarted_account_snapshot();
        assert_eq!(
            snap.portfolio.venues.len(),
            3,
            "the stopped engine is still in the core's list"
        );

        let status = render_read(ReadVerb::Status, &snap);
        assert!(status.contains("realized=42.25"), "not 82.25: {status}");
        assert!(status.contains("fees=1.75"), "not 5.75: {status}");
        assert!(status.contains("positions=1"), "not 2: {status}");
        assert!(status.contains("state=Active\n"), "no phantom `bybit=Halted`: {status}");

        let equity = render_read(ReadVerb::Equity, &snap);
        assert!(equity.contains("realized=42.25"), "{equity}");
        assert!(equity.contains("fees=1.75"), "{equity}");
        assert!(equity.contains("funding=0.50"), "not 2.50: {equity}");
        assert!(equity.contains("margin_used=0.00"), "not 300.00: {equity}");
        assert!(equity.contains("unpriced_positions=0"), "not 2: {equity}");
        assert!(equity.contains("balance=binance:1000.00 bybit:1042.25\n"), "cash once: {equity}");
        assert!(equity.contains("equity_book=2042.25\n"), "{equity}");

        let positions = render_read(ReadVerb::Positions, &snap);
        assert!(positions.contains("bybit/BTCUSDT size=0.5"), "{positions}");
        assert!(!positions.contains("ETHUSDT"), "the stopped engine's row is gone: {positions}");
    }

    /// **The control: with nothing superseded every figure is the core's own, bit for bit** (the sums the core
    /// publishes beside the blocks: `Portfolio::realized_pnl_total` and its siblings, and the fields
    /// `margin_used_total` / `missing_prices_total` its snapshot build sums), so a node that never restarted an
    /// account reads the numbers it always read.
    #[test]
    fn with_nothing_superseded_the_reads_are_the_cores_sums_bit_for_bit() {
        let mut snap = prod2_shaped_snapshot();
        snap.portfolio.venues[0].margin_used = 0.1;
        snap.portfolio.venues[1].margin_used = 0.2;
        snap.portfolio.venues[1].missing_prices = 3;
        snap.portfolio.venues[0].realized_pnl = 0.1;
        snap.portfolio.venues[0].fees_paid = 0.7;
        snap.portfolio.venues[0].funding_paid = -0.3;
        // What `vike_core::snapshot::build` publishes for these blocks.
        snap.portfolio.margin_used_total =
            snap.portfolio.venues.iter().map(|v| v.margin_used).sum();
        snap.portfolio.missing_prices_total = 3;
        let bits = f64::to_bits;
        assert_eq!(
            bits(current_total(&snap, |v| v.realized_pnl)),
            bits(snap.portfolio.realized_pnl_total())
        );
        assert_eq!(
            bits(current_total(&snap, |v| v.fees_paid)),
            bits(snap.portfolio.fees_paid_total())
        );
        assert_eq!(
            bits(current_total(&snap, |v| v.funding_paid)),
            bits(snap.portfolio.funding_paid_total())
        );
        assert_eq!(bits(current_margin_used(&snap)), bits(snap.portfolio.margin_used_total));
        assert_eq!(current_missing_prices(&snap), snap.portfolio.missing_prices_total);
        assert_eq!(current_position_count(&snap), snap.portfolio.position_count());
        // …and an empty list reads the struct's `+0.0`, never an empty `sum()`'s `-0.0`.
        let empty = CoreSnapshot::empty("binance", "BTCUSDT");
        assert_eq!(bits(current_margin_used(&empty)), bits(empty.portfolio.margin_used_total));
    }

    /// A single-venue node renders exactly as before: nothing to disagree, nothing to widen.
    #[test]
    fn a_single_venue_node_renders_the_plain_state_and_its_own_numbers() {
        let mut snap = CoreSnapshot::empty("bybit", "BTCUSDT");
        let mut only = block("bybit", 1_000.0, vike_exec::BalanceMode::Delta);
        only.realized_pnl = 7.5;
        only.positions = vec![pos("bybit", "BTCUSDT", 0.5)];
        snap.portfolio.venues = vec![only];
        let status = render_read(ReadVerb::Status, &snap);
        assert!(status.contains("state=Active\n"), "nothing diverges, no parenthetical: {status}");
        assert!(status.contains("realized=7.50"), "{status}");
    }

    /// Before the first build there are no venue blocks at all: the totals are zero and the cash
    /// falls back to the scalar rather than printing an empty `balance=`.
    #[test]
    fn an_empty_snapshot_still_renders_a_balance() {
        let snap = CoreSnapshot::empty("binance", "BTCUSDT");
        let equity = render_read(ReadVerb::Equity, &snap);
        assert!(equity.contains("balance=0.00"), "{equity}");
        assert!(equity.contains("realized=0.00"), "{equity}");
        assert_eq!(render_read(ReadVerb::Positions, &snap), "no open positions");
    }
}
