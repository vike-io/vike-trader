//! The History tab's Summary: the performance summary of ONE ACCOUNT over its loaded fills (the owner,
//! 2026-11-18: the History summary is per account, as a live record names one). It shows the account's
//! label, the seed, then every catalog row of their fold (`vike_analytics::trading_results_table_from_fills`),
//! or ONE row that says why there is none. The rows are kept in the view until the fills, the account,
//! the seed or the node's off-dollar ledgers or off-default contracts under them change.
//!
//! - The account is picked among the `(venue, account)` pairs the loaded fills name ([`summary_accounts`]).
//!   Its label is `vike_analytics::TradingRecordSource::label` of `Live { account, tier }`, the tier from
//!   the node's account directory, so it reads as every other view labels a set of results.
//! - A fill recorded BEFORE the execution stream carries no account (the retired materializer's, an
//!   import's). It is no account's for certain, so it is left out and its count is said ([`account_fills`]).
//! - A FOREIGN fill (not provably this install's order: a trade on the venue's own site, another
//!   install's) IS the account's: the venue's balance holds it, as a broker statement shows manual trades
//!   (the coordinator, 2026-11-18). So it is folded and its count is said; the Fills tab marks it.
//! - The loaded fills are a WINDOW (the time filter, a page), so the fold OPENS with the position the account's
//!   fills before it leave, and an "Opening" row says it: the loader reads them once per load
//!   (`ExecutionHistoryPage::before`), and the fold and its refusal are the CLI's (`vike-cli report account:`):
//!   `vike_analytics::reconstruct_trades_from`, `vike_analytics::trades_opening_refusal`. Folded from flat, an
//!   exit of an older long read as a new short and booked P&L against nothing.
//! - With the tradehub's POSITION SNAPSHOT of the account at or before the oldest loaded fill (each boot takes one;
//!   the loader reads them, `ExecutionHistoryPage::account_positions`), the fold opens from the snapshot and only the
//!   fills journaled after it, so a fill recorded before the stream (no account) is never needed; a snapshot that
//!   could not know the account's positions refuses in its words (`vike_analytics::opening_snapshot`, the CLI's
//!   reading of the row).

use super::page::{ExecutionHistoryLoad, ExecutionHistoryPage};
use super::{AccountWindowHistoryInputs, AccountWindowHistoryView};
use vike_data::ExecFillRow;
use vike_ui_theme::components::state::{self, ThemeEmptyPane};
use vike_ui_theme::components::{ThemeTokens, role_px};
use vike_ui_theme::metrics::space;
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::type_scale::ThemeTextRole;

/// The Summary of one account, kept until the fills, the account, the seed or the node's off-dollar
/// ledgers or off-default contracts under them change.
#[derive(Clone)]
pub(super) struct AccountWindowHistorySummaryCache {
    generation: u64,
    fills: usize,
    account: (String, String),
    seed: f64,
    ledgers: Vec<(String, String)>,
    contracts: Vec<(String, String, vike_model::ContractValue)>,
    /// The account's label ([`account_label`]).
    label: String,
    /// The lines under the label: the foreign fills folded, the fills without an account left out.
    notes: Vec<String>,
    /// The account's fills, fee corrections not counted.
    fill_count: usize,
    rows: Vec<(String, String)>,
}

/// One account's share of the loaded fills ([`account_fills`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct AccountWindowHistoryAccountFills {
    /// The account's rows, fee corrections included, in the page's order.
    pub(super) fills: Vec<ExecFillRow>,
    /// Fills of the account's venue that carry no account, left out.
    pub(super) without_account: usize,
    /// The account's fills that are not this install's orders, folded.
    pub(super) foreign: usize,
}

/// The performance summary of the picked account: its label, the seed, then every catalog row.
pub(super) fn summary(
    ui: &mut egui::Ui,
    view: &mut AccountWindowHistoryView,
    load: &ExecutionHistoryLoad,
    page: &ExecutionHistoryPage,
    inputs: &AccountWindowHistoryInputs<'_>,
    t: &ThemeTokens,
) {
    use egui::RichText;
    let (ledgers, contracts) = (inputs.off_dollar_ledgers, inputs.off_default_contracts);
    let (positive, negative) = ThemeSidePair::PositiveNegative.texts(t);
    let accounts = summary_accounts(&page.fills);
    if view.summary_account.as_ref().is_none_or(|a| !accounts.contains(a)) {
        view.summary_account = accounts.first().cloned();
    }
    let Some(account) = view.summary_account.clone() else {
        let without = ExecFillRow::not_superseded(&page.fills)
            .iter()
            .filter(|f| f.is_fill() && f.account.is_none())
            .count();
        state::view(ui, ThemeEmptyPane::Empty(&no_account_words(without)));
        return;
    };
    let caption = |ui: &egui::Ui, text: &str| {
        RichText::new(text)
            .size(role_px(ui.ctx(), ThemeTextRole::Caption))
            .color(t.theme.text_tertiary)
    };
    ui.horizontal(|ui| {
        // No visible label sits beside the picker, so it is named in the tree alone.
        egui::ComboBox::from_id_salt("history_summary_account")
            .selected_text(account_words(&account))
            .show_ui(ui, |ui| {
                for a in &accounts {
                    ui.selectable_value(
                        &mut view.summary_account,
                        Some(a.clone()),
                        account_words(a),
                    );
                }
            })
            .response
            .accessible_name("Account");
    });
    let fresh = view.summary.as_ref().is_some_and(|c| {
        c.generation == load.query.generation
            && c.fills == page.fills.len()
            && c.account == account
            && c.seed == view.seed
            && c.ledgers == ledgers
            && c.contracts == contracts
    });
    if !fresh {
        let mine = account_fills(&page.fills, &account);
        let rows = opening_summary_rows(page, &account, &mine, view.seed, ledgers, contracts);
        view.summary = Some(AccountWindowHistorySummaryCache {
            generation: load.query.generation,
            fills: page.fills.len(),
            account: account.clone(),
            seed: view.seed,
            ledgers: ledgers.to_vec(),
            contracts: contracts.to_vec(),
            label: account_label(&account, &mine.fills, inputs.directory),
            notes: account_notes(&account, &mine),
            fill_count: mine.fills.iter().filter(|f| f.is_fill()).count(),
            rows,
        });
    }
    let Some(cache) = &view.summary else { return };
    ui.label(
        RichText::new(&cache.label)
            .size(role_px(ui.ctx(), ThemeTextRole::Strong))
            .color(t.theme.text)
            .strong(),
    );
    for note in &cache.notes {
        ui.label(
            RichText::new(note)
                .size(role_px(ui.ctx(), ThemeTextRole::Caption))
                .color(t.theme.text_secondary),
        );
    }
    let fill_count = cache.fill_count;
    ui.horizontal(|ui| {
        let name = ui.label(caption(ui, "Seed")).id;
        ui.add(egui::DragValue::new(&mut view.seed).speed(100.0).prefix("$")).labelled_by(name);
        ui.label(caption(ui, &format!("over this account's {fill_count} fills loaded above")));
    });
    ui.add_space(space::SM);
    let Some(cache) = &view.summary else { return };
    egui::Grid::new("tearsheet_rows")
        .num_columns(2)
        .spacing([space::XL3, space::SM])
        .striped(true)
        .show(ui, |ui| {
            for (label, value) in &cache.rows {
                ui.label(RichText::new(label).color(t.theme.text_secondary));
                // Sign-colour the value cell on the RENDERED string's first byte: '-' is a loss,
                // '+' a gain, else neutral (see [`tearsheet_rows`] for why only losses colour).
                let col = match value.as_bytes().first() {
                    Some(b'+') => positive,
                    Some(b'-') => negative,
                    _ => t.theme.text,
                };
                ui.label(RichText::new(value).monospace().color(col));
                ui.end_row();
            }
        });
}

/// `venue/account`, as the picker lists an account.
fn account_words((venue, account): &(String, String)) -> String {
    format!("{venue}/{account}")
}

/// **The accounts the Summary can fold**: every `(venue, account)` a loaded fill names, each once, in the
/// page's order (newest first). A fill that names no account names none of them.
pub(super) fn summary_accounts(fills: &[ExecFillRow]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for f in fills {
        if let Some(account) = &f.account {
            let pair = (f.venue.clone(), account.clone());
            if !out.contains(&pair) {
                out.push(pair);
            }
        }
    }
    out
}

/// The rows of `fills` no stream row restates (`ExecFillRow::not_superseded`, the CLI's rule too): a fill the
/// execution stream re-stored with its account is ONE fill, never also a fill "without an account". Grouped by
/// `(venue, symbol)`, in page order inside a series: the order the Summary has always folded in, so its refusal
/// names symbols in it and two symbols' fills at one millisecond keep it through the fold's stable time sort.
fn kept_by_series(fills: &[ExecFillRow]) -> Vec<&ExecFillRow> {
    let mut kept = ExecFillRow::not_superseded(fills);
    // Stable, so page order survives inside a series.
    kept.sort_by(|a, b| a.venue.cmp(&b.venue).then_with(|| a.symbol.cmp(&b.symbol)));
    kept
}

/// **One account's fills**: the rows of `account` (its fee corrections included, which carry its account
/// too), with the counts its notes say: the fills of its venue that carry no account (left out) and its
/// foreign fills (folded). A row the stream restated is counted once ([`kept_by_series`]); a count counts
/// FILLS (`ExecFillRow::is_fill`, the rows the fold folds as fills).
pub(super) fn account_fills(
    fills: &[ExecFillRow],
    (venue, account): &(String, String),
) -> AccountWindowHistoryAccountFills {
    let fills = kept_by_series(fills);
    let mine: Vec<ExecFillRow> = fills
        .iter()
        .filter(|f| f.venue == *venue && f.account.as_deref() == Some(account.as_str()))
        .map(|f| (*f).clone())
        .collect();
    let foreign = mine.iter().filter(|f| f.is_fill() && f.is_foreign()).count();
    let without_account =
        fills.iter().filter(|f| f.is_fill() && f.venue == *venue && f.account.is_none()).count();
    AccountWindowHistoryAccountFills { fills: mine, without_account, foreign }
}

/// **The account's label**, in the one wording (`vike_analytics::TradingRecordSource::label` of
/// `Live { account, tier }`) over the span of its fills. The tier is the node directory's
/// (`vike_tradehub_client::wire::AccountDirectoryAccountWire::tier`): an account it does not list (no
/// directory yet, a deactivated row) or a venue this build does not know is named without one, because a
/// tier is never guessed.
pub(super) fn account_label(
    (venue, account): &(String, String),
    fills: &[ExecFillRow],
    directory: Option<&vike_tradehub_client::wire::AccountDirectoryWire>,
) -> String {
    use vike_model::accounts::account_keys::{
        AccountNamed, RESERVED_DEFAULT_LABEL, parse_wire_account,
    };
    let period = fills.iter().map(|f| f.ts).min().zip(fills.iter().map(|f| f.ts).max());
    let period = period.map(|(from_ms, to_ms)| vike_analytics::TradingPeriod { from_ms, to_ms });
    let tier = directory
        .and_then(|d| {
            d.accounts.iter().find(|a| {
                a.venue == *venue
                    && a.label.as_deref().unwrap_or(RESERVED_DEFAULT_LABEL) == account.as_str()
            })
        })
        .and_then(|a| a.tier.parse::<vike_model::AccountTier>().ok());
    let named = parse_wire_account(account).ok().map(|label| AccountNamed::of(venue, label));
    match (named, tier) {
        (Some(AccountNamed::Known(account)), Some(tier)) => {
            vike_analytics::TradingRecordSource::Live { account, tier }.label(None, period)
        }
        _ => format!("{venue}/{account} (the node did not report its tier)"),
    }
}

/// What the fills BEFORE the loaded ones say of an account's opening ([`account_opening`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct AccountWindowHistoryOpening {
    /// The oldest loaded fill: every fill older is "before". `None` with no loaded fill.
    pub(super) start: Option<i64>,
    /// The account's own fills before `start`: the position the loaded ones open with.
    pub(super) fills: Vec<ExecFillRow>,
    /// Per symbol, the venue's fills before `start` that carry no account: any could be the account's. Not counted
    /// with a [`Self::snapshot`], which stands for them.
    pub(super) unattributed: Vec<(String, usize)>,
    /// The tradehub's position snapshot of the account at or before `start` ([`ExecutionHistoryPage::account_positions`]),
    /// when one is: the Summary opens from it, and [`Self::fills`] are then only the ones journaled after it.
    pub(super) snapshot:
        Option<vike_datahub_client::execution_stream::ExecutionAccountPositionsWire>,
}

/// Whether `f` was journaled after `snapshot` (`vike_data::ExecFillRow::after_position_snapshot`); every fill is,
/// with none.
fn after_snapshot(
    snapshot: Option<&vike_datahub_client::execution_stream::ExecutionAccountPositionsWire>,
    f: &ExecFillRow,
) -> bool {
    snapshot.is_none_or(|s| {
        f.after_position_snapshot((s.generation, i64::try_from(s.sequence).unwrap_or(i64::MAX)))
    })
}

/// **What the fills before the loaded ones say of `account`'s opening**, out of
/// [`ExecutionHistoryPage::before`] (only those older than every loaded fill, as "Older" moves the edge), a row
/// the stream restated read once ([`kept_by_series`]).
pub(super) fn account_opening(
    page: &ExecutionHistoryPage,
    (venue, account): &(String, String),
) -> AccountWindowHistoryOpening {
    let start = page.fills.iter().map(|f| f.ts).min();
    let before: Vec<ExecFillRow> = page
        .before
        .values()
        .flatten()
        .filter(|f| start.is_some_and(|s| f.ts < s))
        .cloned()
        .collect();
    let snapshot = start.and_then(|_| {
        page.account_positions.iter().find(|s| s.venue == *venue && s.account == *account).cloned()
    });
    let mut opening = AccountWindowHistoryOpening { start, ..Default::default() };
    let mut unattributed: std::collections::BTreeMap<String, usize> = Default::default();
    for f in kept_by_series(&before).into_iter().filter(|f| f.venue == *venue) {
        match f.account.as_deref() {
            None if f.is_fill() && snapshot.is_none() => {
                *unattributed.entry(f.symbol.clone()).or_default() += 1
            }
            Some(a) if a == account && after_snapshot(snapshot.as_ref(), f) => {
                opening.fills.push(f.clone())
            }
            _ => {}
        }
    }
    opening.unattributed = unattributed.into_iter().collect();
    opening.snapshot = snapshot;
    opening
}

/// **The Summary's rows, folded from the position the account opens with**: refused in ONE row when that
/// position cannot be read — the fills before could not be read, or one there names no account on a symbol it
/// trades (`vike_analytics::trades_opening_refusal`, the CLI's rule) — else [`summary_rows`] over its fills
/// before and loaded, from the oldest loaded one.
fn opening_summary_rows(
    page: &ExecutionHistoryPage,
    account: &(String, String),
    mine: &AccountWindowHistoryAccountFills,
    seed: f64,
    ledgers: &[(String, String)],
    contracts: &[(String, String, vike_model::ContractValue)],
) -> Vec<(String, String)> {
    let book = account_words(account);
    let opening = account_opening(page, account);
    let snapshot = opening.snapshot.as_ref().map(|row| {
        vike_analytics::opening_snapshot(
            row.as_of_ms,
            &row.venue,
            &row.positions,
            row.unknown.as_deref(),
        )
    });
    let traded = mine.fills.iter().chain(&opening.fills).map(|f| f.symbol.as_str());
    let refusal = page
        .before_unread
        .as_ref()
        .map(|e| {
            format!(
                "the opening position of {book} cannot be read: the fills before the loaded ones did \
                 not load ({e})"
            )
        })
        .or_else(|| {
            page.positions_unread.as_ref().map(|e| {
                format!(
                    "the opening position of {book} cannot be read: the tradehub's position snapshots \
                     did not load ({e})"
                )
            })
        })
        .or_else(|| {
            vike_analytics::trades_opening_refusal(
                &book,
                opening.start,
                traded,
                &opening.unattributed,
                snapshot.as_ref(),
            )
        });
    if let Some(why) = refusal {
        return vec![("Not shown".to_string(), why)];
    }
    // With a snapshot only the fills journaled after it fold on top of it: its book holds every one before.
    let loaded = mine.fills.iter().filter(|f| after_snapshot(opening.snapshot.as_ref(), f));
    let fills: Vec<ExecFillRow> = opening.fills.iter().chain(loaded).cloned().collect();
    summary_rows(&fills, opening.start, snapshot.as_ref(), seed, ledgers, contracts)
}

/// The lines under the label: how many of the account's fills are foreign (folded), and how many fills of
/// its venue carry no account (left out). Nothing for an account with neither.
fn account_notes(
    (venue, _): &(String, String),
    fills: &AccountWindowHistoryAccountFills,
) -> Vec<String> {
    let mut notes = Vec::new();
    if fills.foreign > 0 {
        notes.push(format!(
            "{} of these fills are foreign (not this install's orders, such as a trade on the \
             venue's own site): they are the account's, so they are in this summary, and the Fills \
             tab marks them.",
            fills.foreign
        ));
    }
    if fills.without_account > 0 {
        notes.push(format!(
            "{} fills of {venue} were recorded before the execution stream and carry no account: \
             they are not in this summary.",
            fills.without_account
        ));
    }
    notes
}

/// The Summary when no loaded fill names its account: there is no account to summarize.
fn no_account_words(without_account: usize) -> String {
    format!(
        "No loaded fill names its account: {without_account} fills were recorded before the \
         execution stream and carry no account, so there is no account to summarize."
    )
}

/// The rows the Summary tab shows for the loaded `fills`: the tearsheet of their fold, or ONE row that
/// says why there is none when a fill is on a symbol whose name says it is quoted in a fiat currency
/// other than the dollar (decision 0183, item 13). The fold reads every fill as one currency and
/// holds no rate, so it would show yen as dollars; the rows are never half-right.
///
/// `ledgers` is [`vike_tradehub_client::fold_guards::off_dollar_ledgers`]: the fills of a venue whose engine books in another
/// currency are LEFT OUT (`vike_analytics::without_non_dollar_ledgers`, the journal doors' own rule, by VENUE
/// because a stored fill names no currency) and a first row says how many (decision 0243). A euro
/// account's EURUSD fills carry no foreign NAME, so the name check alone read them as dollars.
///
/// `contracts` is [`vike_tradehub_client::fold_guards::off_default_contracts`]: the fills of a `(venue, symbol)` the node values
/// off the default (inverse, or a multiplier off 1.0) are left out too, after the refusal as the journal doors
/// do (`vike_analytics::without_non_default_contracts`, decision 0243 item 6), and the first row is
/// their whole label (`vike_analytics::trading_results_table_left_out_label`).
///
/// `fills` holds the account's fills BEFORE the loaded ones too ([`account_opening`]) when `from` (the oldest
/// loaded fill) is given: every one is folded (`vike_analytics::reconstruct_trades_from`, the CLI's fold), so an
/// exit of a position opened before `from` is measured against its cost basis, only the trades closed from
/// `from` on are the rows', and an "Opening" row says the position it opened with.
fn summary_rows(
    fills: &[vike_data::ExecFillRow],
    from: Option<i64>,
    snapshot: Option<&vike_analytics::TradesOpeningSnapshot>,
    seed: f64,
    ledgers: &[(String, String)],
    contracts: &[(String, String, vike_model::ContractValue)],
) -> Vec<(String, String)> {
    let on_dollar = |f: &&vike_data::ExecFillRow| !ledgers.iter().any(|(v, _)| *v == f.venue);
    let events = vike_data::ExecFillRow::fill_events(fills);
    // The snapshot's positions fold first, as the fills that open them (no later than the first fill after it).
    let opening = snapshot.map_or_else(Vec::new, |s| {
        let at = events.iter().map(|f| f.timestamp).min().map_or(s.as_of_ms, |t| t.min(s.as_of_ms));
        s.opening_fills(at)
    });
    let named = vike_catalog::foreign_fiat_quoted_list(
        fills.iter().filter(on_dollar).map(|f| f.symbol.as_str()).chain(
            opening
                .iter()
                .filter(|f| !ledgers.iter().any(|(v, _)| *v == f.venue.as_str()))
                .map(|f| f.symbol.as_str()),
        ),
    );
    let events: Vec<_> = opening.into_iter().chain(events).collect();
    let (events, left_out) = vike_analytics::without_non_dollar_ledgers(events, ledgers);
    let (events, left_contracts) = vike_analytics::without_non_default_contracts(events, contracts);
    // A fee in another asset (BNB on a USDT fill) is no quote money: off the fills before the fold, so the fee
    // total and the curve leave it out, and a row says it per asset, unconverted.
    let (events, other_fees) = vike_analytics::without_fees_in_other_assets(events, |f| {
        vike_catalog::fee_in_the_fills_money(f.symbol.as_str(), f.commission_asset.as_str())
    });
    let label = vike_analytics::trading_results_table_left_out_label(&left_out, &left_contracts);
    let mut rows: Vec<(String, String)> = Vec::new();
    if let Some(list) = named {
        rows.push((
            "Not shown".to_string(),
            vike_analytics::trading_results_table_foreign_quote_refusal(&list),
        ));
    } else if events.is_empty()
        && let Some(label) = &label
    {
        rows.push(("Not shown".to_string(), label.clone()));
        return rows;
    } else {
        // One sample per UTC day: 365 a year when every market trades every day, 252 otherwise. No kept row is
        // read: alpaca's coins are the one class a kept row could turn to every-day (`vike_catalog::trades_every_day`),
        // and alpaca has no kept list (decision 0062's decision 3) — shortcut: hand the row in here the day it has one.
        let periods_per_year = vike_analytics::results::periods_per_year_for_days(
            events.iter().all(|f| vike_catalog::trades_every_day(f.venue.as_str(), None)),
        );
        let mut events = events;
        events.sort_by_key(|f| f.timestamp);
        let (opening, trades) =
            vike_analytics::reconstruct_trades_from(&events, from.unwrap_or(i64::MIN));
        let (equity, ts) = vike_analytics::equity_curve_from_trades(seed, &trades);
        let sheet = vike_analytics::trading_results_table_from_curve(
            trades,
            &equity,
            &ts,
            periods_per_year,
        )
        .0;
        rows = tearsheet_rows(&sheet);
        if let Some(words) =
            from.and_then(|_| vike_analytics::trades_opening_words(from, &opening, None, snapshot))
        {
            rows.insert(0, ("Opening".to_string(), words));
        }
        if !other_fees.is_empty() {
            let said = vike_analytics::trading_results_table_other_asset_fee_label(&other_fees);
            rows.insert(0, ("Other assets".to_string(), said));
        }
    }
    if let Some(label) = label {
        rows.insert(0, ("Left out".to_string(), label));
    }
    rows
}

/// Flatten a `TradingResultsTable` into ordered `(label, value)` rows for the grid — **every
/// `vike_analytics::metric_catalog::METRICS` entry, in that table's own declaration order,
/// rendered by that entry's own `MetricUnit`.**
///
/// # It is now an adapter, and that is the whole change
///
/// This function used to be a twenty-row `vec![]` of `(label, format!(…))` pairs against a catalog
/// of thirty-eight, with its own precision per row and its own `* 100.0` on the percent ones; then
/// it became a catalog walk over a local `live_metric_value` accessor, which its own doc called the
/// residual and asked to have DELETED once the accessor moved onto the document type. That move
/// landed — `vike_analytics::TradingResultsTable::rendered_rows` is the one rendering of that document's
/// numbers, shared with the `Display` table and the HTML tearsheet — so the accessor and its pin
/// test are gone and what remains here is the `&'static str` → `String` conversion `WindowView`'s
/// row type needs. Nothing about the roster, the order or the formatting is decided in this file.
///
/// # A row this document does not carry now says so, where it used to VANISH
///
/// The old accessor answered `None` for the thirteen catalog ids the flat struct had no field for,
/// and those rows were OMITTED — defensible then, because `None` was a fact about the TYPE rather
/// than about the session, identical on every run. It is not a fact about the type any more:
/// `TradingResultsTable` carries the whole catalog, so `None` means THIS reconstruction recorded no
/// number, which is exactly the case `vike_analytics::results::TradingResults::render_metrics`
/// prints `vike_analytics::NOT_RECORDED` for. So the panel prints it too. The rows that were
/// missing because the struct dropped them (`long_ratio`, the two ulcer figures, the four
/// return-distribution figures and the rest) now carry real numbers over the same fills, which is
/// the "honest fix" the deleted accessor's doc named; `funding_paid` keeps saying *not recorded* on
/// the store door too, deliberately, because a fill stream folds no funding cashflow and a `0.00`
/// there would tell an operator of a live PERP account that funding was free.
///
/// `NOT_RECORDED` begins with `n`, so the grid's sign-coloring reads it as neutral rather than as a
/// loss — worth stating because that colouring keys on the rendered string's first byte.
fn tearsheet_rows(s: &vike_analytics::TradingResultsTable) -> Vec<(String, String)> {
    s.rendered_rows().into_iter().map(|(id, text)| (id.to_string(), text)).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        ExecutionHistoryPage, account_fills, account_label, account_notes, opening_summary_rows,
        summary_accounts, summary_rows, tearsheet_rows,
    };
    use vike_analytics::metric_catalog::METRICS;
    use vike_analytics::{
        NOT_RECORDED, TRADING_RESULTS_TABLE_SCHEMA, TradingResultsTable,
        TradingResultsTableMetricValues,
    };
    use vike_tradehub_client::fold_guards::off_default_contracts;

    /// A sheet with distinctive values on the rows the formatting assertions read, and NOTHING
    /// recorded elsewhere — so both halves of `rendered_rows` are exercised: a rendered number and
    /// [`NOT_RECORDED`].
    ///
    /// Built through `TradingResultsTableMetricValues::from_fn` rather than as a struct literal because the document
    /// has no per-metric fields left to name; the ids below are the same strings an operator types
    /// at `--metrics`.
    fn sheet() -> TradingResultsTable {
        const VALUES: &[(&str, f64)] = &[
            ("n_trades", 3.0),
            ("final_equity", 10_500.5),
            ("total_return", 0.1234),
            ("net_profit", 500.5),
            ("gross_profit", 700.0),
            ("gross_loss", -199.5),
            ("total_fees", 1.2345),
            ("win_rate", 0.6667),
            ("profit_factor", f64::INFINITY),
            ("sharpe", 1.5),
            ("sortino", 2.25),
            ("calmar", 0.75),
            ("max_drawdown", -0.25),
            ("cagr", -0.05),
            ("sqn", 1.1),
            ("average_win", 350.0),
            ("average_loss", -199.5),
            ("payoff_ratio", 1.75),
            ("expected_payoff", 166.83),
            ("largest_win", 400.0),
            ("largest_loss", -199.5),
            ("consecutive_wins", 2.0),
            ("consecutive_losses", 1.0),
            ("value_at_risk_95", -0.02),
            ("expected_shortfall_95", -0.03),
        ];
        TradingResultsTable {
            schema: TRADING_RESULTS_TABLE_SCHEMA,
            name: None,
            invalid: None,
            metrics: TradingResultsTableMetricValues::from_fn(|id| {
                VALUES.iter().find(|(k, _)| *k == id).map(|(_, v)| *v)
            }),
        }
    }

    /// The grid shows the WHOLE catalog, in the catalog's own declaration order — pinned against
    /// `METRICS` rather than against a literal, because a literal here would be the hand copy this
    /// file has already deleted twice.
    ///
    /// ⚠ This replaces `the_rows_a_live_tearsheet_cannot_answer_are_pinned`, whose whole subject —
    /// a pinned SET of catalog rows the panel silently skipped — no longer exists: nothing is
    /// filtered, so there is no omission to keep honest. That makes this the stronger claim of the
    /// two, and it is also why the old `UNANSWERABLE` list is not carried forward as data.
    #[test]
    fn the_panel_renders_every_catalog_row_in_the_catalogs_own_order() {
        let rows = tearsheet_rows(&sheet());
        let rendered: Vec<&str> = rows.iter().map(|(l, _)| l.as_str()).collect();
        // A closed-trade row appears only where a run armed that block, which no live sheet does.
        let expected: Vec<&str> = METRICS
            .iter()
            .filter(|m| m.home != vike_analytics::metric_catalog::MetricHome::ClosedTrades)
            .map(|m| m.id)
            .collect();
        assert_eq!(rendered, expected);
        // …and it is far WIDER than the twenty-row hand copy that started this, which is the
        // defect that motivated every step of it.
        assert!(rendered.len() > 20, "{} rows", rendered.len());
    }

    /// The formatting is `MetricUnit::render`'s, per the row's OWN unit — so the scaling, the
    /// precision and the non-finite spelling are all decided in one place for this panel, the
    /// CLI, the HTML tearsheet and the stored report alike.
    #[test]
    fn values_are_rendered_by_the_rows_own_unit() {
        let rows = tearsheet_rows(&sheet());
        let get = |id: &str| rows.iter().find(|(l, _)| l == id).map(|(_, v)| v.as_str());
        // Count: no decimals.
        assert_eq!(get("n_trades"), Some("3"));
        assert_eq!(get("consecutive_wins"), Some("2"));
        // Money: two decimals, no scaling.
        assert_eq!(get("final_equity"), Some("10500.50"));
        assert_eq!(get("average_loss"), Some("-199.50"));
        // Percent: the `* 100.0` a hand-rolled renderer forgets, four decimals.
        assert_eq!(get("total_return"), Some("12.3400%"));
        assert_eq!(get("cagr"), Some("-5.0000%"));
        assert_eq!(get("max_drawdown"), Some("-25.0000%"));
        assert_eq!(get("win_rate"), Some("66.6700%"));
        // Ratio: four decimals, and the house inf sentinel reaches the operator as Rust's own
        // `inf` — the same bytes every other door prints, rather than this panel's old `∞` glyph.
        assert_eq!(get("payoff_ratio"), Some("1.7500"));
        assert_eq!(get("profit_factor"), Some("inf"));
        // A row this reconstruction recorded no number for says so — it is PRESENT and reads
        // `not recorded`, where the deleted accessor made it vanish from the panel entirely.
        assert_eq!(get("omega"), Some(NOT_RECORDED));
    }

    /// A stored fill: 100,000 units of `symbol` at `price`, on oanda.
    fn fill(symbol: &str, side: i32, price: f64, timestamp: i64) -> vike_data::ExecFillRow {
        fill_on("oanda", symbol, side, price, timestamp)
    }

    /// [`fill`] on `venue`.
    fn fill_on(
        venue: &str,
        symbol: &str,
        side: i32,
        price: f64,
        timestamp: i64,
    ) -> vike_data::ExecFillRow {
        vike_data::ExecFillRow {
            ts: timestamp,
            trade_id: format!("t{timestamp}"),
            order_client_id: format!("c{timestamp}"),
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            side,
            qty: 100_000.0,
            px: price,
            commission: 0.0,
            mark_price: None,
            liquidity_side: "taker".to_string(),
            commission_asset: "USD".to_string(),
            venue_order_id: None,
            account: None,
            position_side: None,
            corrects_trade_id: None,
            correction_seq: None,
            mount_id: None,
            received_ms: None,
            ownership: None,
            journal_sequence: None,
            journal_generation: None,
        }
    }

    /// **The Summary never shows yen as dollars** (decision 0183, item 13): loaded fills on a symbol
    /// whose name says another fiat quote give ONE row that says why, naming the symbol and its
    /// currency; dollar-quoted and crypto fills still give the whole catalog.
    #[test]
    fn the_summary_refuses_foreign_quoted_fills_in_one_row_and_folds_the_rest() {
        let yen = [fill("USD_JPY", 1, 150.0, 1), fill("USD_JPY", -1, 150.1, 2)];
        let rows = summary_rows(&yen, None, None, 10_000.0, &[], &[]);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].1.contains("USD_JPY (JPY)"), "{rows:?}");

        for symbol in ["EUR_USD", "BTCUSDT"] {
            let fine = [fill(symbol, 1, 100.0, 1), fill(symbol, -1, 110.0, 2)];
            let rows = summary_rows(&fine, None, None, 10_000.0, &[], &[]);
            // No live sheet arms closed-trade accounting, so its block is never a row here.
            let expected: Vec<&str> = METRICS
                .iter()
                .filter(|m| m.home != vike_analytics::metric_catalog::MetricHome::ClosedTrades)
                .map(|m| m.id)
                .collect();
            let got: Vec<&str> = rows.iter().map(|(l, _)| l.as_str()).collect();
            assert_eq!(got, expected, "{symbol}: the whole catalog");
        }
    }

    /// **The Summary never adds a fee in another asset to its quote fees**: 0.0123 BNB on a USDT fill is left out
    /// of `total_fees` and of the curve, and a first row says it per asset, unconverted.
    #[test]
    fn the_summary_leaves_a_fee_in_another_asset_out_of_its_fees_and_says_so() {
        let paid =
            |side: i32, price: f64, timestamp: i64, fee: f64, asset: &str| vike_data::ExecFillRow {
                commission: fee,
                commission_asset: asset.to_string(),
                ..fill_on("binance", "BTCUSDT", side, price, timestamp)
            };
        let fills = [paid(1, 100.0, 1, 0.0123, "BNB"), paid(-1, 110.0, 2, 0.5, "USDT")];
        let rows = summary_rows(&fills, None, None, 10_000.0, &[], &[]);
        assert_eq!(rows[0].0, "Other assets", "{rows:?}");
        assert!(
            rows[0].1.starts_with("fees in other assets: 0.0123 BNB (not converted"),
            "{rows:?}"
        );
        let get = |id: &str| rows.iter().find(|(l, _)| l == id).map(|(_, v)| v.as_str());
        assert_eq!(get("total_fees"), Some("0.50"), "the USDT fee alone");
        // 100,000 units 100 -> 110, less the USDT fee only.
        assert_eq!(get("final_equity"), Some("1009999.50"));
    }

    /// **The Summary never shows a euro book as dollars** (decision 0243): `EUR_USD` names no foreign
    /// quote, so on a node whose fxcm engine books in EUR the fills of that VENUE are left out and a
    /// first row says so; a dollar venue's fills beside them are still folded, and with no off-dollar
    /// ledger the rows are the whole catalog as before.
    #[test]
    fn the_summary_leaves_out_a_non_dollar_ledgers_fills_and_says_so() {
        let euro_ledger = [("fxcm".to_string(), "EUR".to_string())];
        let fxcm =
            [fill_on("fxcm", "EUR_USD", 1, 1.10, 1), fill_on("fxcm", "EUR_USD", -1, 1.11, 2)];

        let rows = summary_rows(&fxcm, None, None, 10_000.0, &euro_ledger, &[]);
        assert_eq!(rows.len(), 1, "every fill is on the euro venue: nothing to fold: {rows:?}");
        assert_eq!(rows[0].0, "Not shown");
        assert!(rows[0].1.contains("2 EUR-ledger fills of fxcm"), "{rows:?}");

        let both = [
            fill_on("fxcm", "EUR_USD", 1, 1.10, 1),
            fill_on("fxcm", "EUR_USD", -1, 1.11, 2),
            fill_on("binance", "BTCUSDT", 1, 100.0, 3),
            fill_on("binance", "BTCUSDT", -1, 110.0, 4),
        ];
        let rows = summary_rows(&both, None, None, 10_000.0, &euro_ledger, &[]);
        assert_eq!(rows[0].0, "Left out", "{rows:?}");
        assert!(rows[0].1.contains("2 EUR-ledger fills of fxcm"), "{rows:?}");
        let expected: Vec<&str> = METRICS
            .iter()
            .filter(|m| m.home != vike_analytics::metric_catalog::MetricHome::ClosedTrades)
            .map(|m| m.id)
            .collect();
        let got: Vec<&str> = rows[1..].iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(got, expected, "the dollar venue's fills still fold, the whole catalog");
        let net = rows.iter().find(|(l, _)| l == "net_profit").map(|(_, v)| v.as_str());
        // 100,000 units of BTCUSDT 100 -> 110 only: the euro fills add nothing
        assert_eq!(net, Some("1000000.00"));

        let rows = summary_rows(&both, None, None, 10_000.0, &[], &[]);
        assert_eq!(rows.len(), expected.len(), "no off-dollar ledger: nothing is left out");
    }

    /// **The Summary never reads an inverse contract as a linear one** (decision 0243, item 6): the
    /// fills of a `(venue, symbol)` the live snapshot values off the default are left out and named in
    /// the journal doors' own words (`vike_analytics::trading_results_table_left_out_label`); the rest still fold.
    #[test]
    fn the_summary_leaves_out_a_non_default_contracts_fills_and_says_so() {
        use vike_execution::{CoreSnapshot, VenueBlock};
        use vike_model::ContractValue;
        let mut snapshot = CoreSnapshot::empty("deribit", "BTC-PERPETUAL");
        let values = [
            ("BTC-PERPETUAL".to_string(), ContractValue::inverse(10.0)),
            ("ETH-PERPETUAL".to_string(), ContractValue::linear(1.0)),
        ];
        let deribit = VenueBlock {
            venue: "deribit".to_string(),
            contract_values: std::sync::Arc::new(values.into_iter().collect()),
            ..VenueBlock::default()
        };
        snapshot.portfolio.venues = vec![deribit.clone(), deribit, VenueBlock::default()];
        let contracts = off_default_contracts(&snapshot);
        let inverse = ContractValue::inverse(10.0);
        assert_eq!(contracts, vec![("deribit".into(), "BTC-PERPETUAL".into(), inverse)]);

        let fills = [
            fill_on("deribit", "BTC-PERPETUAL", 1, 60_000.0, 1),
            fill_on("deribit", "BTC-PERPETUAL", -1, 66_000.0, 2),
            fill_on("binance", "BTCUSDT", 1, 100.0, 3),
            fill_on("binance", "BTCUSDT", -1, 110.0, 4),
        ];
        let rows = summary_rows(&fills, None, None, 10_000.0, &[], &contracts);
        assert_eq!(rows[0].0, "Left out", "{rows:?}");
        let words = "leaves out 2 fills of deribit BTC-PERPETUAL (inverse, 10 a unit)";
        assert!(rows[0].1.starts_with(words), "{rows:?}");
        let net = rows.iter().find(|(l, _)| l == "net_profit").map(|(_, v)| v.as_str());
        // 100,000 units of BTCUSDT 100 -> 110 only: the inverse fills add nothing
        assert_eq!(net, Some("1000000.00"));

        let rows = summary_rows(&fills[..2], None, None, 10_000.0, &[], &contracts);
        assert_eq!(rows.len(), 1, "every fill is on the inverse contract: {rows:?}");
        assert_eq!(rows[0].0, "Not shown");
        assert!(rows[0].1.starts_with(words), "{rows:?}");
    }

    /// [`fill_on`] booked on `account`, with `ownership` as the execution stream stated it.
    fn booked(
        row: vike_data::ExecFillRow,
        account: Option<&str>,
        ownership: Option<&str>,
    ) -> vike_data::ExecFillRow {
        vike_data::ExecFillRow {
            account: account.map(str::to_string),
            ownership: ownership.map(str::to_string),
            ..row
        }
    }

    fn pair(venue: &str, account: &str) -> (String, String) {
        (venue.to_string(), account.to_string())
    }

    /// The page the per-account tests read, newest first: binance MAIN (one of ours, one foreign),
    /// binance's default account, one binance fill recorded before the stream, and bybit's ALT.
    fn page() -> Vec<vike_data::ExecFillRow> {
        vec![
            booked(fill_on("binance", "BTCUSDT", -1, 110.0, 4), Some("MAIN"), Some("ours")),
            booked(fill_on("bybit", "BTCUSDT", 1, 100.0, 3), Some("ALT"), Some("ours")),
            booked(fill_on("binance", "BTCUSDT", 1, 100.0, 2), Some("MAIN"), Some("foreign")),
            booked(fill_on("binance", "ETHUSDT", 1, 10.0, 2), Some("DEFAULT"), Some("ours")),
            booked(fill_on("binance", "BTCUSDT", 1, 100.0, 1), None, None),
        ]
    }

    /// The picker offers every account a loaded fill names, once, newest first; a fill recorded before
    /// the execution stream names none.
    #[test]
    fn the_summary_offers_every_account_the_fills_name_once() {
        assert_eq!(
            summary_accounts(&page()),
            [pair("binance", "MAIN"), pair("bybit", "ALT"), pair("binance", "DEFAULT")]
        );
        assert!(summary_accounts(&[fill("BTCUSDT", 1, 100.0, 1)]).is_empty());
    }

    /// **The Summary is ONE account's** (the owner, 2026-11-18): its fills only, its foreign fills
    /// folded and counted, and the fills of its venue that carry no account left out and counted.
    #[test]
    fn an_accounts_summary_folds_its_fills_and_counts_what_it_left_out() {
        let main = account_fills(&page(), &pair("binance", "MAIN"));
        assert_eq!(main.fills.iter().map(|f| f.ts).collect::<Vec<_>>(), [4, 2]);
        assert_eq!((main.foreign, main.without_account), (1, 1));
        let notes = account_notes(&pair("binance", "MAIN"), &main);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].starts_with("1 of these fills are foreign"), "{notes:?}");
        assert!(
            notes[1].starts_with("1 fills of binance were recorded before the execution stream")
        );

        // The foreign fill is IN the fold: bought at 100, sold at 110, 100,000 units.
        let rows = summary_rows(&main.fills, None, None, 10_000.0, &[], &[]);
        let net = rows.iter().find(|(l, _)| l == "net_profit").map(|(_, v)| v.as_str());
        assert_eq!(net, Some("1000000.00"));

        let alt = account_fills(&page(), &pair("bybit", "ALT"));
        assert_eq!((alt.fills.len(), alt.foreign, alt.without_account), (1, 0, 0));
        assert!(account_notes(&pair("bybit", "ALT"), &alt).is_empty());
    }

    /// **A fill the stream re-stored with its account is counted once**: its old row, which carries no account,
    /// is superseded (`ExecFillRow::superseded`), so it is neither "without an account" nor a second fill; a row
    /// from before the stream with no twin still is.
    #[test]
    fn a_fill_the_stream_restated_is_not_counted_as_left_out() {
        let restated = fill_on("binance", "BTCUSDT", 1, 100.0, 1);
        let page = vec![
            booked(fill_on("binance", "BTCUSDT", -1, 110.0, 4), Some("MAIN"), Some("ours")),
            booked(restated.clone(), Some("MAIN"), Some("ours")),
            booked(restated, None, None),
            booked(fill_on("binance", "BTCUSDT", 1, 90.0, 0), None, None),
        ];
        let main = account_fills(&page, &pair("binance", "MAIN"));
        assert_eq!(main.fills.len(), 2, "the stream's two fills");
        assert_eq!(main.without_account, 1, "only the untwinned old row is left out");
        let notes = account_notes(&pair("binance", "MAIN"), &main);
        assert!(notes[0].starts_with("1 fills of binance were recorded before"), "{notes:?}");
    }

    /// **A row naming no execution is no fill** (E21): it restates nothing but carries no trade id, so the fold
    /// skips it (`ExecFillRow::fill_events`), and the notes' counts skip it too: it is neither one of the
    /// account's foreign fills nor a fill "without an account".
    #[test]
    fn a_row_naming_no_execution_is_not_counted_as_a_fill() {
        let nameless = |account: Option<&str>, ownership: Option<&str>| vike_data::ExecFillRow {
            trade_id: String::new(),
            ..booked(fill_on("binance", "BTCUSDT", 1, 100.0, 1), account, ownership)
        };
        let page = vec![
            booked(fill_on("binance", "BTCUSDT", -1, 110.0, 4), Some("MAIN"), Some("ours")),
            nameless(Some("MAIN"), Some("foreign")),
            nameless(None, None),
        ];
        let main = account_fills(&page, &pair("binance", "MAIN"));
        assert_eq!(main.fills.len(), 2, "the account's rows, the nameless one included");
        assert_eq!((main.foreign, main.without_account), (0, 0));
    }

    /// **An account's fills keep the Summary's series order** (the coordinator, 2026-11-23): grouped by
    /// `(venue, symbol)` in that order, in page order inside a series, however the page interleaves them. So the
    /// refusal names its symbols in that order, and two symbols' fills at ONE millisecond reach the fold's
    /// stable time sort in it.
    #[test]
    fn an_accounts_fills_keep_the_series_order_whatever_the_page_interleaves() {
        let main = pair("oanda", "MAIN");
        let mine = |symbol: &str, side: i32, timestamp: i64| {
            booked(fill(symbol, side, 150.0, timestamp), Some("MAIN"), Some("ours"))
        };
        let page = [
            mine("USD_JPY", -1, 4),
            mine("EUR_JPY", -1, 3),
            mine("USD_JPY", 1, 2),
            mine("EUR_JPY", 1, 1),
        ];
        let fills = account_fills(&page, &main).fills;
        let order: Vec<(&str, i64)> = fills.iter().map(|f| (f.symbol.as_str(), f.ts)).collect();
        assert_eq!(order, [("EUR_JPY", 3), ("EUR_JPY", 1), ("USD_JPY", 4), ("USD_JPY", 2)]);
        let rows = summary_rows(&fills, None, None, 10_000.0, &[], &[]);
        assert!(
            rows[0].1.contains("the fills include EUR_JPY (JPY), USD_JPY (JPY), quoted"),
            "{rows:?}"
        );

        let one_millisecond = [mine("USD_JPY", 1, 7), mine("EUR_JPY", 1, 7)];
        let fills = account_fills(&one_millisecond, &main).fills;
        let symbols: Vec<&str> = fills.iter().map(|f| f.symbol.as_str()).collect();
        assert_eq!(symbols, ["EUR_JPY", "USD_JPY"], "the series order, not the page's");
    }

    /// **The reviewer's scenario in the Summary**: a long of 1 bought at 100 BEFORE the loaded window and sold at
    /// 110 inside it is ONE trade of +10, measured against the position the fills before it left, which the
    /// "Opening" row says; folded from flat (the defect) the sell opened a phantom short. A fill before the window
    /// that names no account on the symbol, or fills before that did not load, refuse the rows in words.
    #[test]
    fn the_summary_measures_an_exit_against_the_position_opened_before_the_window() {
        let main = pair("binance", "MAIN");
        let one = |side: i32, price: f64, timestamp: i64| vike_data::ExecFillRow {
            qty: 1.0,
            ..booked(
                fill_on("binance", "BTCUSDT", side, price, timestamp),
                Some("MAIN"),
                Some("ours"),
            )
        };
        let series = ("binance".to_string(), "BTCUSDT".to_string());
        let page = ExecutionHistoryPage {
            fills: vec![one(-1, 110.0, 40)],
            before: [(series, vec![one(1, 100.0, 10)])].into(),
            ..ExecutionHistoryPage::default()
        };
        let mine = account_fills(&page.fills, &main);
        let rows = opening_summary_rows(&page, &main, &mine, 1_000.0, &[], &[]);
        let get = |rows: &[(String, String)], id: &str| {
            rows.iter().find(|(l, _)| l == id).map(|(_, v)| v.clone())
        };
        assert_eq!(get(&rows, "n_trades").as_deref(), Some("1"), "one round trip: {rows:?}");
        assert_eq!(get(&rows, "net_profit").as_deref(), Some("10.00"));
        let opened = "opening position at 1970-01-01T00:00:00Z: long 1 BTCUSDT at 100 (folded from \
                      the account's fills before it)";
        assert_eq!(get(&rows, "Opening").as_deref(), Some(opened));
        let flat = summary_rows(&mine.fills, None, None, 1_000.0, &[], &[]);
        assert_eq!(get(&flat, "n_trades").as_deref(), Some("0"), "from flat: a phantom short");

        let mut blind = page.clone();
        for rows in blind.before.values_mut() {
            rows.push(fill_on("binance", "BTCUSDT", 1, 95.0, 5));
        }
        let rows = opening_summary_rows(&blind, &main, &mine, 1_000.0, &[], &[]);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, "Not shown");
        assert!(rows[0].1.contains("the opening position of binance/MAIN"), "{rows:?}");
        assert!(rows[0].1.contains("1 of BTCUSDT"), "{rows:?}");

        let unread = ExecutionHistoryPage { before_unread: Some("timed out".into()), ..page };
        let rows = opening_summary_rows(&unread, &main, &mine, 1_000.0, &[], &[]);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].1.contains("did not load (timed out)"), "{rows:?}");
    }

    /// **The Summary opens from the tradehub's position snapshot**: the long of 1 at 100 was bought before the
    /// stream (its row names no account, so without a snapshot the rows are refused), and the boot's snapshot holds
    /// MAIN long 1 at 100. The sell at 110 journaled after it is ONE trade of +10, a row journaled before it is in
    /// its book and folds no second time, and the "Opening" row names the snapshot. A snapshot that could not know
    /// MAIN's positions refuses MAIN in its words, and so does a read of the snapshots that failed.
    #[test]
    fn the_summary_opens_from_the_tradehubs_position_snapshot() {
        use vike_datahub_client::execution_stream::ExecutionAccountPositionsWire;
        let main = pair("binance", "MAIN");
        let streamed =
            |side: i32, price: f64, timestamp: i64, sequence: i64| vike_data::ExecFillRow {
                qty: 1.0,
                journal_generation: Some(1),
                journal_sequence: Some(sequence),
                ..booked(
                    fill_on("binance", "BTCUSDT", side, price, timestamp),
                    Some("MAIN"),
                    Some("ours"),
                )
            };
        let series = ("binance".to_string(), "BTCUSDT".to_string());
        let pre_stream = vike_data::ExecFillRow {
            qty: 1.0,
            ..booked(fill_on("binance", "BTCUSDT", 1, 100.0, 5), None, None)
        };
        let boot = ExecutionAccountPositionsWire {
            generation: 1,
            sequence: 10,
            as_of_ms: 15,
            venue: "binance".into(),
            account: "MAIN".into(),
            positions: vec![vike_model::AccountPosition {
                symbol: "BTCUSDT".into(),
                position_side: vike_model::events::PositionSide::Both,
                size: 1.0,
                average_price: 100.0,
            }],
            unknown: None,
        };
        let blind = ExecutionHistoryPage {
            fills: vec![streamed(-1, 110.0, 40, 12)],
            before: [(series, vec![pre_stream, streamed(1, 100.0, 6, 8)])].into(),
            ..ExecutionHistoryPage::default()
        };
        let mine = account_fills(&blind.fills, &main);
        let get = |rows: &[(String, String)], id: &str| {
            rows.iter().find(|(l, _)| l == id).map(|(_, v)| v.clone())
        };
        let refused = opening_summary_rows(&blind, &main, &mine, 1_000.0, &[], &[]);
        assert_eq!(
            refused[0].0, "Not shown",
            "no snapshot: the pre-stream fill blinds it: {refused:?}"
        );

        let page = ExecutionHistoryPage { account_positions: vec![boot.clone()], ..blind.clone() };
        let rows = opening_summary_rows(&page, &main, &mine, 1_000.0, &[], &[]);
        assert_eq!(get(&rows, "n_trades").as_deref(), Some("1"), "one round trip: {rows:?}");
        assert_eq!(get(&rows, "net_profit").as_deref(), Some("10.00"));
        let opened = "opening position at 1970-01-01T00:00:00Z: long 1 BTCUSDT at 100 (the tradehub's \
                      position snapshot of 1970-01-01T00:00:00Z and the account's fills after it)";
        assert_eq!(get(&rows, "Opening").as_deref(), Some(opened));

        let unknown = ExecutionAccountPositionsWire {
            positions: Vec::new(),
            unknown: Some("the journal holds no earlier book".into()),
            ..boot
        };
        let page = ExecutionHistoryPage { account_positions: vec![unknown], ..blind.clone() };
        let rows = opening_summary_rows(&page, &main, &mine, 1_000.0, &[], &[]);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].1.contains("could not know its positions"), "{rows:?}");

        let unread = ExecutionHistoryPage { positions_unread: Some("timed out".into()), ..blind };
        let rows = opening_summary_rows(&unread, &main, &mine, 1_000.0, &[], &[]);
        assert!(rows[0].1.contains("position snapshots did not load (timed out)"), "{rows:?}");
    }

    /// The label is the one wording of a live source, the tier from the node's directory; the default
    /// account is the directory's unlabelled row, and an account it does not list is named without a
    /// tier, never with a guessed one.
    #[test]
    fn the_label_is_the_live_source_with_the_directorys_tier() {
        use vike_tradehub_client::wire::{AccountDirectoryAccountWire, AccountDirectoryWire};
        let row = |label: Option<&str>, tier: &str| AccountDirectoryAccountWire {
            id: 1,
            venue: "binance".to_string(),
            label: label.map(str::to_string),
            tier: tier.to_string(),
            venue_account_id: None,
        };
        let directory = AccountDirectoryWire {
            venues: Vec::new(),
            accounts: vec![row(Some("MAIN"), "demo"), row(None, "live")],
        };
        let day = 1_767_225_600_000;
        let fills = [
            fill_on("binance", "BTCUSDT", 1, 100.0, day),
            fill_on("binance", "BTCUSDT", -1, 110.0, day + 60_000),
        ];

        let main = account_label(&pair("binance", "MAIN"), &fills, Some(&directory));
        assert_eq!(main, "binance/MAIN (demo) 2026-01-01 00:00 – 2026-01-01 00:01 UTC");
        let default = account_label(&pair("binance", "DEFAULT"), &fills, Some(&directory));
        assert_eq!(default, "binance/DEFAULT (live) 2026-01-01 00:00 – 2026-01-01 00:01 UTC");
        assert_eq!(
            account_label(&pair("binance", "MAIN"), &fills, None),
            "binance/MAIN (the node did not report its tier)"
        );
    }
}
