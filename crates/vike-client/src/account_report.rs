//! The LIVE account report: an account's own fills out of the datahub's store, folded into [`TradingResults`] whose
//! source is `TradingRecordSource::Live { account, tier }`. Both doors answer from it, so they cannot fold an account
//! two ways: `vike-cli report account:<venue>/<LABEL>` (and `report run:<forward run>`, which is its account over the
//! run's window, kept to its mount) and the MCP tool `report` ([`read_live_side`] is the one read both call). It moved
//! here from `vike-cli` on 2026-11-24 with that second door; the tradehub's live-journal report is retired.
//!
//! # Where each fact comes from
//!
//! - **The fills: the datahub, and nothing else** (the owner's Q4, 2026-11-17). Every `kind=exec_fill` series of
//!   the venue in the window ([`HistoryStore::execution_series`]), each read whole over the window, kept to the
//!   account by the row's `account` column ([`account_fills`]). A row the execution stream restated is read once,
//!   as the stream stated it (`vike_data::ExecFillRow::superseded`).
//! - **The tier and the two fold guards: the tradehub's snapshot** ([`node_facts`], the observe key): the account's
//!   tier, and what a fold of stored fills cannot read (decisions 0209 and 0243, `vike_tradehub_client::fold_guards`),
//!   a ledger off the dollar and a contract valued off the default. ⚠ Only a RUNNING engine states those two, so an
//!   account the node does not run is refused ([`node_facts`]), never folded as dollars of a linear unit.
//! - **Each fill's quote and session: the venue's KEPT rows, read from the same datahub** ([`read_live_side`]): a
//!   fill's currency is its row's where the row states a fiat code, else its name's (`vike_catalog::kept_fiat_quote`),
//!   so an IBKR share the row quotes in euros, or a broker's CFD whose name states no currency, is refused rather than
//!   folded at par; and the row's class says whether the market trades every day (365 or 252 a year).
//!
//! # What the account's results count (the coordinator's rulings, 2026-11-18)
//!
//! - **A FOREIGN fill counts, marked** (`vike_data::exec_log::OWNERSHIP_FOREIGN`: a venue-UI order, another
//!   install's, a reconcile `EXT-` id): an account statement is what really happened on the account, as a broker
//!   statement shows hand trades. The output says how many and names them; `--without-foreign` leaves them out.
//!   A forward run's results never hold one: it keeps the fills of its own mount, and a foreign fill has none.
//! - **A fill recorded before the execution stream carries no account** and is left out, and the output says how
//!   many: nothing can say whose it was.
//! - **A window opens from the tradehub's POSITION SNAPSHOT** at or before its start (each boot takes one, the
//!   datahub keeps it: `vike_datahub_client::HistoryStoreDatahub::execution_account_positions`) plus the account's
//!   fills journaled after it, so a fill before the stream is never needed. Before the account's first snapshot the
//!   opening is folded from its fills, and refused when a fill there names no account; a snapshot that could not
//!   know the account's positions refuses in its words (`vike_analytics::trades_opening_refusal`).
//! - **The fills fold in TIME order** (`TradingResultsTableFillOrder::ByTime`'s rule, the owner's Q6).
//! - **A fee in another asset is no quote money** (`vike_catalog::fee_in_the_fills_money`): taken off its fill
//!   before the fold, so `total_fees` and the curve leave it out, and named per asset, unconverted.
//! - **The curve is the REALIZED one by default**, from `--cash` (the owner's Q7): the sampler's `TOTAL` equity is
//!   node-wide, not the account's. `--mark-to-market` reconstructs it from the fills and the store's bars, and only
//!   then are the bars read (and each trade's MAE/MFE back-filled from them).
//!
//! Its two store reads live in [`history`] (`mtm_curve_from_store`) and [`excursions`] (`backfill_excursions`);
//! both name only the venue-agnostic `HistoryStore` trait.
//!
//! Every refusal is a [`ReportError`]: the words, and the rung `vike-cli`'s exit ladder maps them to.

pub mod excursions;
pub mod history;

use std::collections::{BTreeMap, BTreeSet};

use vike_analytics::results::TradingResults;
use vike_analytics::{
    MarkToMarketRuntimeStatistics, TradingPeriod, TradingRecord, TradingRecordSource,
    TradingResultsTable,
};
use vike_data::{ExecFillRow, HistoryStore, HistoryStoreError, TimestampRange};
use vike_model::StoreKindId;
use vike_model::accounts::account_keys::{AccountLabel, AccountRef, route_key_of};
use vike_model::events::FillEvent;

use crate::attach::Rung;
use crate::hubkeys::HubKeyring;

/// The bar interval `--mark-to-market` reads when `--interval` is not given.
pub const DEFAULT_INTERVAL: &str = "1m";

/// A refusal of the live report, in the words the CLI always printed, on the rung it exits by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportError {
    /// The rung `vike-cli` exits on.
    pub rung: Rung,
    /// What went wrong.
    pub message: String,
}

impl ReportError {
    fn usage(message: impl Into<String>) -> Self {
        Self { rung: Rung::Usage, message: message.into() }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self { rung: Rung::Failed, message: message.into() }
    }

    fn connect(message: impl Into<String>) -> Self {
        Self { rung: Rung::Connect, message: message.into() }
    }

    fn refused(message: impl Into<String>) -> Self {
        Self { rung: Rung::Refused, message: message.into() }
    }
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// How many FOREIGN trade ids a note names before it says "and N more".
const FOREIGN_IDS_NAMED: usize = 10;

/// One live side: whose fills, over which window, kept to which mount.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveQuery {
    /// The account.
    pub account: AccountRef,
    /// The window the RESULTS cover, inclusive at both ends; an open end is the store's. The fills are read from
    /// the store's first up to its end: the ones before it give the position the window opens with.
    pub window: TimestampRange,
    /// A forward run's mount: only its fills are kept. `None` reads the whole account.
    pub mount_id: Option<String>,
    /// What heads the results when the operator named nothing: a forward run's strategy file.
    pub title: Option<String>,
}

/// How a live side is folded: the knobs `--cash`, `--periods-per-year`, `--mark-to-market`, `--interval` and
/// `--without-foreign` set.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveFold {
    /// The realized curve's seed.
    pub cash: f64,
    /// Where [`Self::cash`] came from (`--cash`, a stored run's first equity, the default), said beside it in
    /// every rendering: the return ratios are of that capital.
    pub cash_from: String,
    /// The annualization factor; `None` derives it from the venues (365 when every one trades every day).
    pub periods_per_year: Option<f64>,
    /// `Some(interval)`: mark the curve to market against the store's bars at that interval.
    pub mark_to_market: Option<String>,
    /// Leave the FOREIGN fills out.
    pub without_foreign: bool,
}

/// What [`account_fills`] read: the account's fills and what it says about them.
#[derive(Debug, Default)]
pub struct AccountFills {
    /// The kept fills from the store's first up to the window's end, fee corrections folded in, in TIME order:
    /// the ones before the window give its opening position.
    pub fills: Vec<FillEvent>,
    /// How many of [`Self::fills`] fall IN the window.
    pub in_window: usize,
    /// The trade ids of the account's FOREIGN fills in the window, kept or (with `--without-foreign`) left out.
    pub foreign: Vec<String>,
    /// How many fills of the venue in the window carry no account (recorded before the execution stream), left out.
    pub without_account: usize,
    /// Before a window START, per symbol: the venue's fills that carry no account. Any of them could be this
    /// account's, so an opening position on that symbol cannot be read.
    pub unattributed_before: Vec<(String, usize)>,
    /// The symbols the account's foreign fills trade beside its own (with `--without-foreign`): leaving those out
    /// would break the positions they share.
    pub shared_with_foreign: Vec<String>,
    /// The tradehub's position snapshot of the account at or before the window's start, when one is: the window
    /// opens from it, and [`Self::fills`] are then only the fills journaled after it.
    pub opening: Option<vike_analytics::TradesOpeningSnapshot>,
}

/// **The account's fills up to `query`'s window end**, out of `store`: every `kind=exec_fill` series of the venue,
/// each read whole from its first row, a row the stream restated read once, kept to the account (and to the mount,
/// for a forward run). The window's own counts are taken over its span. Pure over the trait, so the suite drives
/// it over the in-memory store.
///
/// `snapshot` is the account's latest position snapshot at or before the window's start: with one, only the rows
/// journaled AFTER it are kept (`vike_data::ExecFillRow::after_position_snapshot`, journal order: its book holds
/// every one before), and the fills before the window that name no account are not counted, since none of them is
/// needed.
pub fn account_fills(
    store: &dyn HistoryStore,
    query: &LiveQuery,
    without_foreign: bool,
    snapshot: Option<&vike_datahub_client::execution_stream::ExecutionAccountPositionsWire>,
) -> Result<AccountFills, HistoryStoreError> {
    let after_snapshot = |row: &ExecFillRow| {
        snapshot.is_none_or(|s| {
            row.after_position_snapshot((
                s.generation,
                i64::try_from(s.sequence).unwrap_or(i64::MAX),
            ))
        })
    };
    // The row's account column is the label's SCREEN spelling (`DEFAULT` for the unlabelled account).
    let word = query.account.label.to_string();
    let venue = query.account.venue;
    let start = query.window.start;
    let in_window = |ts: i64| start.is_none_or(|s| ts >= s);
    let read = TimestampRange { start: None, end: query.window.end };
    let catalog = store.execution_series(Some(venue), None, read)?;
    let mut out = AccountFills::default();
    let mut kept: Vec<ExecFillRow> = Vec::new();
    let (mut own, mut foreign) = (BTreeSet::new(), BTreeSet::new());
    let mut unattributed: BTreeMap<String, usize> = BTreeMap::new();
    for (id, _) in catalog.series.iter().filter(|(id, _)| id.kind == StoreKindId::ExecFill) {
        if id.venue != venue {
            continue;
        }
        let rows = store.scan_execution_fills_head(&id.venue, &id.symbol, read, usize::MAX)?;
        let superseded = ExecFillRow::superseded(&rows);
        for (row, gone) in rows.into_iter().zip(superseded) {
            if gone {
                continue;
            }
            // A fee correction belongs to its fill; only a fill counts in the notes.
            let a_fill = row.is_fill();
            match row.account.as_deref() {
                None if in_window(row.ts) => {
                    out.without_account += usize::from(a_fill);
                    continue;
                }
                None => {
                    if snapshot.is_none() {
                        *unattributed.entry(row.symbol.clone()).or_default() += usize::from(a_fill);
                    }
                    continue;
                }
                Some(account) if account != word => continue,
                Some(_) => {}
            }
            if !after_snapshot(&row) {
                continue; // its execution is in the snapshot's book
            }
            if query.mount_id.is_some() && row.mount_id != query.mount_id {
                continue;
            }
            if row.is_foreign() {
                foreign.insert(row.symbol.clone());
                if a_fill && in_window(row.ts) {
                    out.foreign.push(row.trade_id.clone());
                }
                if without_foreign {
                    continue;
                }
            } else {
                own.insert(row.symbol.clone());
            }
            kept.push(row);
        }
    }
    out.fills = ExecFillRow::fill_events(&kept);
    out.fills.sort_by_key(|f| f.timestamp);
    out.in_window = out.fills.iter().filter(|f| in_window(f.timestamp)).count();
    if start.is_some() {
        out.unattributed_before = unattributed.into_iter().filter(|(_, n)| *n > 0).collect();
    }
    if without_foreign {
        out.shared_with_foreign = own.intersection(&foreign).cloned().collect();
    }
    // The account's row of the datahub's position snapshots at the window's start, as the fold reads it.
    out.opening = snapshot.map(|row| {
        vike_analytics::opening_snapshot(
            row.as_of_ms,
            &row.venue,
            &row.positions,
            row.unknown.as_deref(),
        )
    });
    Ok(out)
}

/// What the tradehub says about one account: its tier, and the two fold guards of its snapshot.
pub struct NodeFacts {
    pub tier: vike_model::AccountTier,
    /// `vike_tradehub_client::fold_guards::off_dollar_ledgers` of the snapshot.
    pub ledgers: Vec<(String, String)>,
    /// `vike_tradehub_client::fold_guards::off_default_contracts` of the snapshot.
    pub contracts: Vec<(String, String, vike_model::ContractValue)>,
}

/// **Ask the tradehub** at `node`, at `Read`, for one point-in-time snapshot, and read the account's tier and
/// the snapshot's fold guards ([`facts_of`]).
pub fn node_facts(
    node: &str,
    keys: &HubKeyring,
    account: &AccountRef,
) -> Result<NodeFacts, ReportError> {
    let Some((key, _origin)) = keys.observe() else {
        return Err(ReportError::failed(keys.observe_absent_message("report", node)));
    };
    let wire = vike_tradehub_client::snapshot_once(node, key).map_err(|e| {
        ReportError::connect(format!(
            "the tradehub at {node} did not answer the snapshot read: {e}. A live account's report \
             asks it for the account's tier and its ledgers"
        ))
    })?;
    facts_of(node, &wire, account)
}

/// **What the node's snapshot `wire` says about `account`**: the tier of its RUNNING engine and the snapshot's fold
/// guards. Pure, so the refusals are decided without a node.
///
/// ⚠ **A NOT RUNNING account is refused, never folded.** The guards are read off the running engines' blocks; the
/// node's NOT RUNNING row of an account (`vike_execution::ArmingNotRunningView`) states neither its ledger's currency
/// nor its contracts' values, and no stored row does either. Folded anyway, a disarmed account that traded inverse
/// contracts would read as linear (+6,000,000 where it booked +100), and a euro ledger as dollars. An account the
/// node names nowhere is refused too: its tier is unknown, and a live record says which tier it is.
fn facts_of(
    node: &str,
    wire: &vike_tradehub_client::wire::CoreSnapshotWire,
    account: &AccountRef,
) -> Result<NodeFacts, ReportError> {
    let snapshot = vike_tradehub_client::observe_decode::wire_to_core(wire);
    let route = account.route_key();
    let book = format!("{}/{}", account.venue, account.label);
    let running = snapshot.portfolio.venues.iter().find(|b| b.route_key == route);
    if let Some(tier) = running.and_then(|b| b.tier) {
        return Ok(NodeFacts {
            tier,
            ledgers: vike_tradehub_client::fold_guards::off_dollar_ledgers(&snapshot),
            contracts: vike_tradehub_client::fold_guards::off_default_contracts(&snapshot),
        });
    }
    if snapshot.not_running.iter().any(|n| n.route_key() == route) {
        return Err(ReportError::refused(format!(
            "{book} is NOT RUNNING on the tradehub at {node}, so its ledger's currency and its contracts' \
             values are unknown (only a running engine states them): a live report would read its fills as \
             dollars of a linear contract. Report it while it runs."
        )));
    }
    Err(ReportError::refused(format!(
        "the tradehub at {node} names no account {book} (running or not), so its tier is \
         unknown and a live report would not say whose book it is. `vike-cli account \
         ls` shows the accounts this box's database holds."
    )))
}

/// One folded live side: the results (source `Live`), their table, the daily curve the table was read off and,
/// under `--mark-to-market`, the runtime exposure.
pub struct LiveSide {
    pub results: TradingResults,
    pub sheet: TradingResultsTable,
    pub equity: Vec<f64>,
    pub equity_timestamp: Vec<i64>,
    pub runtime: Option<MarkToMarketRuntimeStatistics>,
    /// What the side counted or left out, one sentence each (also said on stderr).
    pub notes: Vec<String>,
}

/// **Fold one live side.** `read` is what [`account_fills`] answered, `facts` the node's word, `kept` the venue's
/// kept instrument rows (each fill's quote and session are read off its row), `store` the history the mark-to-market
/// path reads its bars from, `now_ms` the end of an open window.
pub fn fold_live(
    query: &LiveQuery,
    fold: &LiveFold,
    read: AccountFills,
    facts: &NodeFacts,
    kept: &[vike_model::Instrument],
    store: &dyn HistoryStore,
    now_ms: i64,
) -> Result<LiveSide, ReportError> {
    // A fill's symbol is its USER name, and its row is the one that stores it (decision 0290); `None` on a venue
    // whose list predates stored names, or keeps none, and the name answers alone.
    let row_of = |f: &FillEvent| {
        vike_catalog::kept_row_of_user_name(kept, f.venue.as_str(), f.symbol.as_str()).ok()
    };
    let AccountFills {
        fills,
        in_window,
        foreign,
        without_account,
        unattributed_before,
        shared_with_foreign,
        opening: snapshot,
    } = read;
    let book = format!("{}/{}", query.account.venue, query.account.label);
    if !shared_with_foreign.is_empty() {
        return Err(ReportError::refused(format!(
            "--without-foreign cannot leave the foreign fills of {book} out: they trade {} beside this \
             install's own, so leaving them out would break the positions they share. Report without it \
             (they count, named).",
            shared_with_foreign.join(", ")
        )));
    }
    // ⚠ A window never opens flat by assumption: its opening position is the tradehub's position snapshot at or
    // before it plus the account's fills after that, else it is folded from the account's fills before it, and a
    // fill there that names no account could be this account's.
    let traded = fills.iter().map(|f| f.symbol.as_str());
    if let Some(refusal) = vike_analytics::trades_opening_refusal(
        &book,
        query.window.start,
        traded,
        &unattributed_before,
        snapshot.as_ref(),
    ) {
        return Err(ReportError::refused(refusal));
    }
    // The snapshot's positions fold first, as the fills that open them (no later than the first fill after it).
    let fills = match &snapshot {
        Some(s) => {
            let at = fills.first().map_or(s.as_of_ms, |f| f.timestamp.min(s.as_of_ms));
            s.opening_fills(at).into_iter().chain(fills).collect()
        }
        None => fills,
    };
    let mut notes = vec![format!("starting capital {} ({})", fold.cash, fold.cash_from)];
    if !foreign.is_empty() {
        let mut named =
            foreign.iter().take(FOREIGN_IDS_NAMED).cloned().collect::<Vec<_>>().join(", ");
        if foreign.len() > FOREIGN_IDS_NAMED {
            named.push_str(&format!(" and {} more", foreign.len() - FOREIGN_IDS_NAMED));
        }
        notes.push(if fold.without_foreign {
            format!(
                "{} FOREIGN fill(s) left out (--without-foreign): {named}",
                foreign.len()
            )
        } else {
            format!(
                "{} FOREIGN fill(s) counted (not placed by this install: the venue's own site, another \
                 install or a reconcile, and on the account all the same): {named}",
                foreign.len()
            )
        });
    }
    if without_account > 0 {
        notes.push(format!(
            "{without_account} fill(s) of {} recorded before the execution stream carry no account and are \
             left out",
            query.account.venue
        ));
    }
    // The node's ledgers first, then the quote refusal on what is left, then the contracts: the journal doors'
    // order, so a refused book stays refused. Each fill's quote is its KEPT row's where the row states a fiat code,
    // else its name's (`vike_catalog::kept_fiat_quote`): an IBKR share the row quotes in euros, or a broker's CFD
    // whose name states no currency, is refused, never folded at par.
    let (fills, left_ledgers) = vike_analytics::without_non_dollar_ledgers(fills, &facts.ledgers);
    let quoted = fills.iter().map(|f| {
        let quote = vike_catalog::kept_fiat_quote(f.venue.as_str(), f.symbol.as_str(), row_of(f));
        (f.symbol.as_str(), quote)
    });
    if let Some(list) = vike_catalog::off_ledger_list_of(quoted, vike_model::DEFAULT_CURRENCY) {
        return Err(ReportError::failed(
            vike_analytics::trading_results_table_foreign_quote_refusal(&list),
        ));
    }
    let (fills, left_contracts) =
        vike_analytics::without_non_default_contracts(fills, &facts.contracts);
    if let Some(label) =
        vike_analytics::trading_results_table_left_out_label(&left_ledgers, &left_contracts)
    {
        notes.push(label);
    }
    // A fee in another asset (BNB on a USDT fill) is no quote money: off the fills before they fold, so neither
    // `total_fees` nor the realized curve adds it, and said per asset, unconverted (a fill row carries no price).
    let (fills, other_fees) = vike_analytics::without_fees_in_other_assets(fills, |f| {
        vike_catalog::fee_in_the_fills_money(f.symbol.as_str(), f.commission_asset.as_str())
    });
    if !other_fees.is_empty() {
        notes.push(vike_analytics::trading_results_table_other_asset_fee_label(&other_fees));
    }
    // One close per UTC day: 365 a year when every market trades every day, 252 otherwise, by the class each
    // fill's kept row states (`vike_catalog::trades_every_day`: an alpaca coin every day, a share or an FX pair not).
    let periods_per_year = fold.periods_per_year.unwrap_or_else(|| {
        vike_analytics::results::periods_per_year_for_days(
            fills.iter().all(|f| vike_catalog::trades_every_day(f.venue.as_str(), row_of(f))),
        )
    });
    // Every fill up to the window's end is folded, so an exit inside the window is measured against the cost
    // basis of a position opened before it; only the trades CLOSED inside the window are its results.
    let from = query.window.start.unwrap_or(i64::MIN);
    let (opening, mut trades) = vike_analytics::reconstruct_trades_from(&fills, from);
    let first_fill = fills.first().map(|f| f.timestamp);
    if let Some(words) = vike_analytics::trades_opening_words(
        query.window.start,
        &opening,
        first_fill,
        snapshot.as_ref(),
    ) {
        notes.insert(0, words);
    }
    let (curve, curve_timestamp, runtime) = match &fold.mark_to_market {
        None => {
            let (e, t) = vike_analytics::equity_curve_from_trades(fold.cash, &trades);
            (e, t, None)
        }
        Some(interval) => {
            excursions::backfill_excursions(&mut trades, store, query.account.venue, interval);
            marked_curve(
                store,
                &fills,
                &trades,
                fold.cash,
                (query.account.venue, interval),
                from,
                &mut notes,
            )
        }
    };
    // One close per UTC day, as every live sheet reads its curve, so `periods_per_year` counts days.
    let (equity, equity_timestamp) =
        vike_analytics::equity::daily_equity_curve(&curve, &curve_timestamp);
    let record = TradingRecord {
        source: TradingRecordSource::Live { account: query.account.clone(), tier: facts.tier },
        n_trades: trades.len(),
        final_equity: equity.last().copied().unwrap_or(fold.cash),
        trades,
        equity_curve: equity.clone(),
        equity_timestamp: equity_timestamp.clone(),
        ..TradingRecord::default()
    };
    let mut results = TradingResults::from_record(None, &record, periods_per_year);
    // The period is the ASKED window where it was asked, else the span of what was folded.
    results.period = query
        .window
        .start
        .or(first_fill)
        .map(|from_ms| TradingPeriod { from_ms, to_ms: query.window.end.unwrap_or(now_ms) });
    // A window that measured nothing records no ratio (`TradingResults::measured_nothing`): said, with why.
    let unmeasured = "its returns and ratios are not recorded";
    match (in_window, results.measured_nothing()) {
        (0, true) => notes.push(format!("no fills in this window: {unmeasured}")),
        (0, false) => notes.push(
            "no fills in this window: its curve is the position it opened with, marked".to_string(),
        ),
        (_, true) => notes.push(format!(
            "no trade closed in this window and its curve never moved: {unmeasured}"
        )),
        _ => {}
    }
    let label = results.source_label().unwrap_or_default();
    let head = match &query.title {
        Some(title) => format!("{title} on {label}"),
        None => label,
    };
    let name = std::iter::once(head).chain(notes.iter().cloned()).collect::<Vec<_>>().join("; ");
    results.name = Some(name);
    // The table is read off the LIVE results, so what a live record cannot know (`funding_paid`:
    // `TradingResults::metric_value`) reads "not recorded" in the text and the HTML and `null` in the JSON.
    let sheet = TradingResultsTable::from_report(&results);
    Ok(LiveSide { results, sheet, equity, equity_timestamp, runtime, notes })
}

/// The `--mark-to-market` curve: the fills marked against the store's bars, with the runtime exposure; the
/// realized curve, said in `notes`, when the reconstruction gives nothing or fails. Every fill is marked, so a
/// position opened before the window carries its basis in; the samples before `from` are dropped and the rest
/// rebased on what was realized and traded before it, so the window's curve starts from `cash` plus the opening
/// position's unrealized P&L.
fn marked_curve(
    store: &dyn HistoryStore,
    fills: &[FillEvent],
    trades: &[vike_model::Trade],
    cash: f64,
    (venue, interval): (&str, &str),
    from: i64,
    notes: &mut Vec<String>,
) -> (Vec<f64>, Vec<i64>, Option<MarkToMarketRuntimeStatistics>) {
    let fallback = |why: String, notes: &mut Vec<String>| {
        notes.push(format!("--mark-to-market {why}; the curve is the realized one"));
        let (e, t) = vike_analytics::equity_curve_from_trades(cash, trades);
        (e, t, None)
    };
    match history::mtm_curve_from_store(store, fills, cash, venue, interval) {
        Ok(points) => {
            let before = points.iter().rev().find(|p| p.timestamp < from);
            let (realized, traded) = before.map_or((0.0, 0.0), |p| (p.realized, p.traded_notional));
            let points: Vec<_> = points
                .into_iter()
                .filter(|p| p.timestamp >= from)
                .map(|mut p| {
                    p.equity -= realized;
                    p.realized -= realized;
                    p.traded_notional -= traded;
                    p
                })
                .collect();
            if points.is_empty() {
                return fallback("had no fill to mark in the window".to_string(), notes);
            }
            let runtime = MarkToMarketRuntimeStatistics::from_points(&points);
            let (e, t) = vike_analytics::mtm_equity_curve(&points);
            (e, t, Some(runtime))
        }
        Err(e) => fallback(format!("could not read the {interval} bars ({e})"), notes),
    }
}

/// The account a `venue/LABEL` book names, on this build's venue roster. Refused, by name, for a venue the
/// roster does not hold: a live record names its account by a roster venue.
pub fn account_ref(venue: &str, label: AccountLabel) -> Result<AccountRef, ReportError> {
    match vike_model::venues::roster_id(venue) {
        Some(venue) => Ok(AccountRef { venue, label }),
        None => Err(ReportError::usage(format!(
            "`{venue}` is not a venue this build knows, so no account of it can be reported \
             (its routing key would be `{}`)",
            route_key_of(venue, &label)
        ))),
    }
}

/// **One live side, read and folded**: the datahub `store` at `datahub` (what it records, the venue's kept rows), the
/// tradehub at `node` (its observe key in `keys`) for the account's tier and fold guards ([`node_facts`]), the
/// datahub again for the account's fills and its opening snapshot ([`account_fills`]), then the fold
/// ([`fold_live`]). `now_ms` ends an open window. The one read `vike-cli report account:` and the MCP tool `report`
/// both answer from.
///
/// The CALLER builds the store, with its keys and its reply bound: a one-shot command line may wait out the
/// client's own bound, a single-threaded server answering inline may not (`HistoryStoreDatahub::with_reply_timeout`).
pub fn read_live_side(
    node: &str,
    keys: &HubKeyring,
    store: &vike_datahub_client::HistoryStoreDatahub,
    datahub: &str,
    query: &LiveQuery,
    fold: &LiveFold,
    now_ms: i64,
) -> Result<LiveSide, ReportError> {
    let unreadable = |e: HistoryStoreError| {
        ReportError::connect(format!("the datahub at {datahub} could not be read: {e}"))
    };
    refuse_a_datahub_that_records_nothing(
        datahub,
        &store.advertised_features().map_err(unreadable)?,
    )?;
    // The venue's kept rows, what each fill's quote and session are read off; none from a datahub that keeps no
    // lists, and every fill then answers by its name.
    let venue = query.account.venue.to_string();
    let kept = store.kept_instruments_and_splits_of(&[venue]).0.map_err(unreadable)?;
    let facts = node_facts(node, keys, &query.account)?;
    // The account's latest position snapshot at or before the window's start, the statement it opens from (none
    // asked for a window with no start: the whole recorded history opens flat).
    let snapshot = match query.window.start {
        Some(start) => store
            .execution_account_positions(start)
            .map_err(unreadable)?
            .unwrap_or_default()
            .into_iter()
            .find(|r| {
                r.venue == query.account.venue && r.account == query.account.label.to_string()
            }),
        None => None,
    };
    let read =
        account_fills(store, query, fold.without_foreign, snapshot.as_ref()).map_err(unreadable)?;
    fold_live(query, fold, read, &facts, &kept.unwrap_or_default(), store, now_ms)
}

/// **A datahub that records no live executions is REFUSED, before any fill is read** (the coordinator,
/// 2026-11-19, as the plan has it): it does not advertise
/// `vike_datahub_client::proto::FEATURE_EXECUTIONS_RECORDED`, so the newest fills of any account are
/// missing from it and its rows cannot make a correct account report. A warning beside partial or old
/// numbers would be a wrong answer delivered politely. One line, and the fix: the datahub and the
/// tradehub deploy together.
fn refuse_a_datahub_that_records_nothing(
    address: &str,
    features: &[String],
) -> Result<(), ReportError> {
    let recorded = vike_datahub_client::proto::FEATURE_EXECUTIONS_RECORDED;
    if features.iter().any(|f| f == recorded) {
        return Ok(());
    }
    Err(ReportError::refused(format!(
        "the datahub at {address} records no live executions (it does not advertise `{recorded}`): \
         update it, the datahub and the tradehub deploy together"
    )))
}

#[path = "account_report_tests.rs"]
#[cfg(test)]
mod account_report_tests;
