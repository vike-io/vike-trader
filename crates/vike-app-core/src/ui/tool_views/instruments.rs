//! **The Data Manager's Instruments destination** — one row per roster venue, the per-venue
//! refresh stamp `crates/vike-catalog/src/persist.rs` promised this window would show, and the
//! button that triggers a re-fetch.
//!
//! # Why it is its own destination
//!
//! It was written beside the Venues arming grid (deleted with the venue ceiling) and refused that
//! host for reasons that still decide where it may go:
//!
//! 1. **Its rows are venues.** A per-ACCOUNT table has two rows for a venue with two accounts and
//!    ONE instrument list; hung off those rows the count would render twice.
//! 2. **A body that allocates every pixel it is offered**
//!    (`ScrollArea::auto_shrink([false, false])`) leaves a second table below it no frame — the defect
//!    `crates/vike-app-core/src/ui/tool_views/data.rs`'s `data_tool_content` documents and cures
//!    with `egui::Panel::bottom`.
//! 3. A network button next to a control that arms real money is a button pressed by accident.
//!
//! The rail's own footer note sanctions the alternative in as many words: *"A twelfth destination
//! is one more row here."* This is that row.
//!
//! # ⚠ A row with no button is not a row with a broken button
//!
//! Only a venue `vike_catalog::catalog_source_for` can ROUTE renders a control at all — the two
//! `crate::data::catalog_refresh::RefreshAvailability` arms whose `refreshable()` is true. Everything
//! else renders its reason in the Status column instead, because a disabled button is a thing
//! people keep clicking and a dead one is worse.
//!
//! ⚠ **What changed, and why the old sentence was worse than no sentence.** Every unrouted row used
//! to read *"not in this build — this venue's catalog lives in its bridge crate, which this binary
//! does not link"*, for THIRTEEN of the fourteen venues. That is true only in the narrow
//! local-provider sense: for the eight publicly-enumerable ones the backend's datahub can answer,
//! and for alpaca/oanda/ctrader/ig/ibkr the reason is a different KIND of fact — a venue property
//! or an identity cost that no build and no switch can change. Those five now render the
//! SERVER-side sentence (`vike_datahub_client::catalog::CatalogRefusal::describe`), so the daemon,
//! the CLI and this table cannot describe one refusal three different ways.

use vike_ui_theme::components::Tokens;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::instruments;

use crate::data::catalog_refresh::{
    RefreshAvailability, VenueCatalogRow, VenueRefreshState, refresh_block, refreshed_label,
};
use vike_catalog::CatalogAnswer;

/// The button's label — read verbatim off the accessibility tree by
/// `crates/vike-app-core/tests/catalog_refresh_screen.rs`, which counts the buttons a given row set
/// produces. Keep it a bare word: a decorated label would make that gate assert on decoration.
pub const REFRESH_LABEL: &str = "Refresh";

/// The paragraph above the table. It states what a press COSTS, because the press spends a venue's
/// public API allowance — and ⚠ **not always from this box**, which is the sentence this paragraph
/// got wrong until the server route existed: a `ServerBacked` press is made by the connected
/// backend's datahub, from ITS box and against ITS rate budget.
///
/// ⚠ Its last clause used to be "which is the whole reason `VIKE_DATAHUB_VENUE_CATALOG` is off by
/// default there", and `docs/decisions/0066` made that false twice over — the lane is ON by default
/// now, and the switch was never what bounded the cost. The paragraph states the cost and stops
/// there, because naming a server's configuration in a desktop's intro text is how a sentence goes
/// stale on a box the reader does not administer.
pub const INSTRUMENTS_INTRO: &str = "The symbol picker searches this list. A venue's instruments are fetched once and then cached \
     on disk — nothing re-fetches them in the background, so Refresh is the only thing that \
     updates a venue. A venue this build links a bridge for is called from this box; every other \
     listable venue is fetched by the connected backend's datahub, against that box's rate \
     budget. Either way a venue is refused a second press while one is running and for a minute \
     after one finishes. A venue with no button carries its reason in the Status column.";

/// **The Instruments screen.** Returns the venue whose Refresh was clicked this frame, or `None`.
///
/// Pure with respect to I/O — it renders rows and reports a click. The fetch is spawned by the
/// caller through `crate::data::catalog_refresh::CatalogRefresh::request`, which is what keeps the
/// network off the frame thread.
pub fn instruments_screen(
    ui: &mut egui::Ui,
    rows: &[VenueCatalogRow],
    total: usize,
    now_ms: i64,
) -> Option<String> {
    let t = Tokens::of(ui.ctx());
    let mut clicked = None;
    ui.label(
        egui::RichText::new(INSTRUMENTS_INTRO).font(t.font(TextRole::Body)).color(t.theme.text2),
    );
    ui.add_space(space::MD);
    ui.label(
        egui::RichText::new(format!(
            "{total} instruments in the picker · {} of {} venues can be refreshed",
            refreshable(rows),
            rows.len()
        ))
        .font(t.font(TextRole::Caption))
        .color(t.theme.text3),
    );
    ui.add_space(space::MD);

    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("dm_instruments").show(
        ui,
        |ui| {
            egui::Grid::new("dm_instruments_grid").num_columns(5).striped(true).show(ui, |ui| {
                for h in ["Venue", "Instruments", "Last refreshed", "Status", ""] {
                    ui.label(
                        egui::RichText::new(h).font(t.font(TextRole::Caption)).color(t.theme.text3),
                    );
                }
                ui.end_row();

                for row in rows {
                    ui.label(
                        egui::RichText::new(row.venue.clone())
                            .font(t.font(TextRole::Body))
                            .color(t.theme.text),
                    );
                    // A venue that NOTHING has answered for reads `—`, not `0`: zero is a measured
                    // claim about a list, and nothing has measured one.
                    //
                    // ⚠ The other THREE answers all carry a real count and all render it — the
                    // cache's own refresh, the operator's locally-credentialed fetch, and the
                    // shipped baseline (`docs/decisions/0066` decision 7). What separates them is
                    // not the count but WHEN and BY WHOM, which is [`age_cell`]'s column and
                    // `status_line`'s sentence. Keying this cell on the ANSWER rather than on
                    // `stamp.is_some()` is what stops a venue with a shipped or a local list
                    // reading `—` beside a picker that is searching it.
                    ui.label(
                        egui::RichText::new(match row.answer() {
                            CatalogAnswer::Nothing => "—".to_string(),
                            _ => row.count().to_string(),
                        })
                        .font(t.mono(TextRole::Body))
                        .color(t.theme.text),
                    );
                    ui.label(
                        egui::RichText::new(age_cell(row, now_ms))
                            .font(t.font(TextRole::Body))
                            .color(t.theme.text2),
                    );
                    // ⚠ WRAPPED, inside a width-bounded scope. `instruments::STATUS_MAX_WIDTH` is how
                    // wide the Status cell may grow before it WRAPS.
                    //
                    // ⚠ **A bound rather than a look**, and it is load-bearing: an `egui::Grid` column
                    // takes the width of its widest cell, and the refusal sentences this screen now
                    // renders are two hundred characters
                    // (`vike_datahub_client::catalog::CatalogRefusal::describe` names the venue, the
                    // mechanism and the operator's action). Unwrapped, one `ig` row widens the grid
                    // past the window and pushes the BUTTON COLUMN off the right-hand edge — every
                    // control still drawn, none of them reachable. MEASURED:
                    // `crates/vike-app-core/tests/catalog_refresh_screen.rs`'s
                    // `a_click_returns_the_venue_of_its_own_row` went red exactly that way, on a
                    // 1100px harness, the first time a long sentence landed beside a live button.
                    //
                    // The accessibility label still carries the whole sentence, which is what the
                    // screen gate reads, so wrapping costs the tests nothing and buys the button
                    // column its place on the row.
                    ui.scope(|ui| {
                        ui.set_max_width(instruments::STATUS_MAX_WIDTH);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(status_line(row))
                                    .font(t.font(TextRole::Body))
                                    .color(t.theme.text2),
                            )
                            .wrap(),
                        );
                    });
                    if row.availability().refreshable() {
                        let block = refresh_block(row, now_ms).map(|b| b.line(&row.venue));
                        // A bare word: `crates/vike-app-core/tests/catalog_refresh_screen.rs`
                        // counts these buttons by `REFRESH_LABEL`.
                        let refresh = ActionButton::secondary(REFRESH_LABEL);
                        let refresh = match &block {
                            Some(why) => refresh.disabled_because(why),
                            None => refresh,
                        };
                        let resp = ui.add(refresh);
                        let resp = if block.is_none() {
                            resp.on_hover_text(press_hint(row))
                        } else {
                            resp
                        };
                        if resp.clicked() {
                            clicked = Some(row.venue.clone());
                        }
                    } else {
                        // No control at all — see the module doc. The reason is already in the
                        // Status cell, so this one stays empty rather than repeating it.
                        ui.label("");
                    }
                    ui.end_row();
                }
            });
        },
    );
    clicked
}

/// **The "Last refreshed" cell — and it must say WHICH SOURCE answered, not merely when.**
///
/// `docs/decisions/0066`'s decision 7, rule 3. `refreshed_label`'s `"never"` arm is the seam: a
/// venue with no operator fetch and no shipped list still reads `never`, while one whose list was
/// fetched by US before the tag reads `shipped <date>` — so "we fetched this for you in September"
/// is distinguishable from "you refreshed this yesterday" without reading a second column, and the
/// two sources can never silently disagree about which one is on screen.
///
/// ⚠ **The operator's own fetch WINS**, and this is where that is visible: a venue that has both
/// reads its own age, and the shipped row is simply not mentioned. The row's data half
/// (`crate::data::catalog_refresh::VenueCatalogRow::answer`) is what decides; this function only renders
/// it, so the screen and the picker cannot disagree about which source is in force.
///
/// It lands inside `instruments::STATUS_MAX_WIDTH`'s neighbour column and is deliberately short: the QUALIFIER
/// (alpaca's environment, oanda's division) rides the Status cell, where there is room for it.
fn age_cell(row: &VenueCatalogRow, now_ms: i64) -> String {
    // ⚠ `answering_list` rather than a match on `baseline` alone: BOTH list sources carry a fetch
    // DATE rather than an age, and the version of this that read only the shipped one rendered a
    // LOCALLY fetched venue as `never` — a row with a real count beside the word for "nothing has
    // ever measured this". The row's own precedence decides which list answered; this renders it.
    //
    // ⚠ …and the two are spelled DIFFERENTLY in the one word this cell has room for, because WHO
    // fetched it is the difference an operator acts on: `shipped` came with the binary and may
    // describe another account's universe, `yours` was fetched on this box with their own keys.
    // Neither may read like an AGE.
    match (row.answer(), row.answering_list()) {
        (CatalogAnswer::LocalCredentialed, Some(b)) => format!("yours {}", b.fetched),
        (_, Some(b)) => b.label(),
        (_, None) => refreshed_label(row.stamp.as_ref(), now_ms),
    }
}

/// How many rows carry a control — the header's numerator, derived from the ONE predicate
/// [`instruments_screen`] gates the button on rather than from a second list of variants.
fn refreshable(rows: &[VenueCatalogRow]) -> usize {
    rows.iter().filter(|r| r.availability().refreshable()).count()
}

/// The live button's hover: **where the press goes**, which differs by route and is the one thing
/// a rate-limit-conscious operator wants to know before pressing.
fn press_hint(row: &VenueCatalogRow) -> String {
    match (row.availability(), row.server.as_deref()) {
        (RefreshAvailability::ServerBacked, Some(addr)) => {
            format!("ask the datahub at {addr} to list this venue now")
        }
        _ => "fetch this venue's instrument list from its API now".to_string(),
    }
}

/// The Status cell: what the last attempt did, or why this venue cannot be refreshed here.
///
/// An outcome OUTRANKS an availability note, because "failed — kept the 412 already cached" is the
/// thing the operator pressed the button to find out.
fn status_line(row: &VenueCatalogRow) -> String {
    match &row.state {
        VenueRefreshState::InFlight { .. } => "fetching…".to_string(),
        VenueRefreshState::Done { outcome, .. } => outcome.line(),
        VenueRefreshState::Idle => {
            let note = row.availability().note(&row.venue).unwrap_or_default();
            // ⚠ The baseline's QUALIFIER rides HERE and not in the Last-refreshed cell, and the
            // reason is that it is the half that makes the list HONEST rather than the half that
            // makes it datable: alpaca's list is common per venue only within one ENVIRONMENT and
            // oanda's only within one regulatory DIVISION (`docs/decisions/0066` decision 6), so an
            // operator in another one is looking at instruments they may not be able to trade. A
            // list that shows its age and hides its qualifier is the looks-authoritative failure
            // wearing a date.
            //
            // ⚠ The two list SOURCES say DIFFERENT things and must not share a sentence. A shipped
            // list was not fetched with the operator's credentials and may describe another
            // account's universe — that is the caveat. A locally fetched one IS theirs, so the same
            // caveat would be a false warning; what it owes them instead is which TIER it came
            // from, because a demo list is not the live universe.
            let provenance = match (row.answer(), row.answering_list()) {
                (CatalogAnswer::ShippedBaseline, Some(b)) => Some(format!(
                    "this list ships with vike ({}, fetched {}) — it was not fetched with your \
                     credentials, and a venue whose universe differs by account will differ from \
                     this.",
                    b.qualifier, b.fetched
                )),
                (CatalogAnswer::LocalCredentialed, Some(b)) => Some(format!(
                    "this list is YOURS — fetched on this box with your own credentials ({}, {}). \
                     Re-run `vike-backend catalog refresh {}` to update it.",
                    b.qualifier, b.fetched, row.venue
                )),
                _ => None,
            };
            match provenance {
                Some(p) if note.is_empty() => p,
                Some(p) => format!("{note} {p}"),
                None => note,
            }
        }
    }
}

/// The right-aligned half of this destination's breadcrumb.
///
/// ⚠ "never fetched" counts only venues that COULD be fetched. It used to count `stamp.is_none()`
/// over every row, which on the real roster read *"13 venues never fetched"* — and for ig and ibkr
/// that is not a state that can ever change, so it reported a permanent venue property as an
/// outstanding task.
#[must_use]
pub fn instruments_summary(rows: &[VenueCatalogRow], total: usize) -> String {
    let stale = rows.iter().filter(|r| r.availability().refreshable() && r.stamp.is_none()).count();
    if stale == 0 {
        format!("{total} instruments across {} venues", rows.len())
    } else {
        format!("{total} instruments · {stale} venues never fetched")
    }
}

#[path = "instruments_tests.rs"]
#[cfg(test)]
mod instruments_tests;
