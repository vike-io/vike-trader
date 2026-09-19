//! **The Data Manager's Instruments destination** — one row per roster venue, the per-venue
//! refresh stamp `crates/vike-catalog/src/persist.rs` promised this window would show, and the
//! button that triggers a re-fetch.
//!
//! # Why it is its own destination and not a section of Venues
//!
//! Configure → Venues was the obvious home and was read before this was written.
//! `crates/vike-app-core/src/tool_views/venues.rs` renders per-ACCOUNT ARMING — ceiling,
//! credentials, effective tier — keyed by `VenueArming::key`, and every control on it is a
//! typed-confirm write to `policy.toml`. Three things made it the wrong host:
//!
//! 1. **Its rows are accounts, this table's rows are venues.** A venue with two accounts has two
//!    arming rows and ONE instrument list; hung off those rows the count would render twice, which
//!    is the exact defect `VenueArmingInputs::creds_for` exists to have fixed for credentials.
//! 2. **Its body allocates every pixel it is offered** (`ScrollArea::auto_shrink([false, false])`),
//!    so a second table below it reaches no frame — the defect
//!    `crates/vike-app-core/src/tool_views/connections.rs`'s `strip_reservation` records.
//! 3. A network button next to a control that arms real money is a button pressed by accident.
//!
//! The rail's own footer note sanctions the alternative in as many words: *"A twelfth destination
//! is one more row here."* This is that row.
//!
//! # ⚠ A row with no button is not a row with a broken button
//!
//! Only a venue `vike_catalog::catalog_source_for` can ROUTE renders a control at all — the two
//! `crate::catalog_refresh::RefreshAvailability` arms whose `refreshable()` is true. Everything
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

use vike_ui_theme::palette as theme;

use crate::catalog_refresh::{
    RefreshAvailability, VenueCatalogRow, VenueRefreshState, refresh_block, refreshed_label,
};
use vike_catalog::CatalogAnswer;

/// The button's label — read verbatim off the accessibility tree by
/// `crates/vike-app-core/tests/catalog_refresh_screen.rs`, which counts the buttons a given row set
/// produces. Keep it a bare word: a decorated label would make that gate assert on decoration.
pub const REFRESH_LABEL: &str = "Refresh";

/// How wide the Status cell may grow before it WRAPS.
///
/// ⚠ **A bound rather than a look**, and it is load-bearing: an `egui::Grid` column takes the width
/// of its widest cell, and the refusal sentences this screen now renders are two hundred characters
/// (`vike_datahub_client::catalog::CatalogRefusal::describe` names the venue, the mechanism and the
/// operator's action). Unwrapped, one `ig` row widens the grid past the window and pushes the
/// BUTTON COLUMN off the right-hand edge — every control still drawn, none of them reachable.
/// MEASURED: `crates/vike-app-core/tests/catalog_refresh_screen.rs`'s
/// `a_click_returns_the_venue_of_its_own_row` went red exactly that way, on a 1100px harness, the
/// first time a long sentence landed beside a live button.
const STATUS_MAX_WIDTH: f32 = 420.0;

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
/// caller through `crate::catalog_refresh::CatalogRefresh::request`, which is what keeps the
/// network off the frame thread.
pub fn instruments_screen(
    ui: &mut egui::Ui,
    rows: &[VenueCatalogRow],
    total: usize,
    now_ms: i64,
) -> Option<String> {
    let mut clicked = None;
    ui.label(egui::RichText::new(INSTRUMENTS_INTRO).size(11.0).color(theme::TEXT2));
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(format!(
            "{total} instruments in the picker · {} of {} venues can be refreshed",
            refreshable(rows),
            rows.len()
        ))
        .size(10.5)
        .color(theme::TEXT3),
    );
    ui.add_space(6.0);

    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("dm_instruments").show(
        ui,
        |ui| {
            egui::Grid::new("dm_instruments_grid").num_columns(5).striped(true).show(ui, |ui| {
                for h in ["Venue", "Instruments", "Last refreshed", "Status", ""] {
                    ui.strong(h);
                }
                ui.end_row();

                for row in rows {
                    ui.label(row.venue.clone());
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
                    ui.label(match row.answer() {
                        CatalogAnswer::Nothing => "—".to_string(),
                        _ => row.count().to_string(),
                    });
                    ui.label(age_cell(row, now_ms));
                    // ⚠ WRAPPED, inside a width-bounded scope — see `STATUS_MAX_WIDTH`. The
                    // accessibility label still carries the whole sentence, which is what the
                    // screen gate reads, so wrapping costs the tests nothing and buys the button
                    // column its place on the row.
                    ui.scope(|ui| {
                        ui.set_max_width(STATUS_MAX_WIDTH);
                        ui.add(egui::Label::new(status_line(row)).wrap());
                    });
                    if row.availability().refreshable() {
                        let block = refresh_block(row, now_ms);
                        let resp =
                            ui.add_enabled(block.is_none(), egui::Button::new(REFRESH_LABEL));
                        let resp = match block {
                            Some(b) => resp.on_disabled_hover_text(b.line(&row.venue)),
                            None => resp.on_hover_text(press_hint(row)),
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
/// (`crate::catalog_refresh::VenueCatalogRow::answer`) is what decides; this function only renders
/// it, so the screen and the picker cannot disagree about which source is in force.
///
/// It lands inside [`STATUS_MAX_WIDTH`]'s neighbour column and is deliberately short: the QUALIFIER
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_refresh::RefreshOutcome;
    use vike_catalog::{CatalogMode, CatalogSource, VenueStamp};

    fn row(
        venue: &str,
        source: Option<CatalogSource>,
        mode: Option<CatalogMode>,
    ) -> VenueCatalogRow {
        VenueCatalogRow {
            venue: venue.into(),
            mode,
            source,
            server: Some("127.0.0.1:7878".into()),
            stamp: None,
            baseline: None,
            local: None,
            own_keys: false,
            state: VenueRefreshState::Idle,
        }
    }

    fn direct(venue: &str) -> VenueCatalogRow {
        row(venue, Some(CatalogSource::Direct), Some(CatalogMode::Enumerable))
    }

    #[test]
    fn an_idle_row_states_why_it_cannot_be_refreshed_and_names_the_kind_of_fact() {
        assert_eq!(status_line(&direct("deribit")), "", "a live row says nothing until pressed");
        assert!(
            status_line(&row("binance", Some(CatalogSource::ServerBacked), None))
                .contains("backend's datahub"),
            "a routed row says where its list comes from"
        );
        // The three facts that are NOT about this build.
        let alpaca = status_line(&row("alpaca", None, None));
        assert!(alpaca.contains("credentials"), "{alpaca}");
        assert!(alpaca.contains("alpaca"), "the sentence names the venue: {alpaca}");
        let ig = status_line(&row("ig", None, None));
        assert!(ig.contains("publishes no bulk instrument list"), "{ig}");
        assert!(ig.contains("nothing to arm and nothing to rebuild"), "{ig}");
        let ibkr = status_line(&row("ibkr", None, None));
        assert!(ibkr.contains("publishes no bulk instrument list"), "{ibkr}");
        // ...and none of them reads as an empty venue.
        for line in [&alpaca, &ig, &ibkr] {
            assert!(!line.contains("0 instruments"), "an empty list is a LIE here: {line}");
        }
    }

    /// The outcome wins over the note — otherwise the one venue you CAN refresh renders a blank
    /// Status cell after a failure, which is the state the operator most needs to see.
    #[test]
    fn an_outcome_outranks_the_availability_note() {
        let mut r = direct("deribit");
        r.state = VenueRefreshState::Done {
            at_ms: 1,
            outcome: RefreshOutcome::Failed { error: "reset".into(), kept: 412 },
        };
        let line = status_line(&r);
        assert!(line.contains("kept the 412"), "{line}");
        r.state = VenueRefreshState::InFlight { since_ms: 1 };
        assert_eq!(status_line(&r), "fetching…");
    }

    /// A refusal that came back over the WIRE renders the server's own sentence on the row, not a
    /// failure and not a count.
    #[test]
    fn a_wire_refusal_renders_as_itself() {
        let mut r = row("binance", Some(CatalogSource::ServerBacked), None);
        r.state = VenueRefreshState::Done {
            at_ms: 1,
            outcome: RefreshOutcome::Refused {
                why: "this datahub serves no venue catalog because its operator REFUSED the lane \
                      — `venue_catalog_off = true`"
                    .into(),
                kept: 0,
            },
        };
        let line = status_line(&r);
        assert!(line.contains("venue_catalog_off"), "{line}");
        assert!(!line.contains("failed"), "a refusal is not a failure: {line}");
    }

    #[test]
    fn the_summary_counts_only_venues_that_could_be_fetched() {
        let mut fetched = direct("deribit");
        fetched.stamp =
            Some(VenueStamp { venue: "deribit".into(), last_refreshed_ms: 1, count: 3 });
        assert_eq!(instruments_summary(&[fetched.clone()], 3), "3 instruments across 1 venues");
        assert_eq!(
            instruments_summary(&[fetched.clone(), row("ig", None, None)], 3),
            "3 instruments across 2 venues",
            "a venue that can never be fetched is not an outstanding task"
        );
        assert!(
            instruments_summary(
                &[fetched, row("binance", Some(CatalogSource::ServerBacked), None)],
                3
            )
            .contains("1 venues never fetched")
        );
    }

    /// **A SHIPPED baseline renders as a third state: a real count, a SHIPPED date, and its
    /// qualifier.** `docs/decisions/0066` decision 7.
    ///
    /// ⚠ Mutation proof: make `age_cell` fall through to `refreshed_label` for a baseline row and
    /// the second assertion goes red — the cell reads `never` beside a non-zero count, which is
    /// the undated-but-authoritative rendering the record refuses. Drop the qualifier from
    /// `status_line` and the fourth goes red.
    #[test]
    fn a_shipped_baseline_shows_its_date_and_its_qualifier_and_never_an_age() {
        let mut r = row("alpaca", None, None);
        r.baseline = Some(vike_catalog::BaselineVenue {
            venue: "alpaca".into(),
            fetched: "2026-09-16".into(),
            qualifier: "demo".into(),
            instruments: vec![],
        });
        assert_eq!(r.count(), 0, "an empty shipped row is a measured zero, not an absence");
        assert_eq!(age_cell(&r, 1), "shipped 2026-09-16");
        assert!(!age_cell(&r, 1).contains("ago"), "a shipped date must never read as an age");
        assert!(!age_cell(&r, 1).contains("never"), "it WAS fetched — by us, on that date");

        let line = status_line(&r);
        assert!(line.contains("ships with vike"), "the source is named: {line}");
        assert!(line.contains("demo"), "the qualifier rides the Status cell: {line}");
        assert!(line.contains("2026-09-16"), "{line}");
        // …and the venue's own refusal sentence survives beside it, because the SERVER still
        // refuses this venue and that fact did not change.
        assert!(line.contains("credentials"), "{line}");
    }

    /// **A LOCALLY fetched list outranks the shipped one and is spelled differently.**
    /// `docs/decisions/0066` decision 9.
    ///
    /// ⚠ Mutation proof: point `age_cell` at `row.baseline` instead of `row.answering_list()` and
    /// the second assertion goes red — a venue with a real count renders `never`, the word for
    /// "nothing has ever measured this". Drop the `LocalCredentialed` arm from `status_line` and
    /// the fourth does: their own list is described as one that ships with vike and "was not
    /// fetched with your credentials", which is exactly backwards.
    #[test]
    fn a_locally_fetched_list_is_spelled_as_theirs_and_never_as_shipped() {
        let mut r = row("alpaca", None, None);
        r.baseline = Some(vike_catalog::BaselineVenue {
            venue: "alpaca".into(),
            fetched: "2026-01-01".into(),
            qualifier: "demo".into(),
            instruments: vec![],
        });
        r.local = Some(vike_catalog::BaselineVenue {
            venue: "alpaca".into(),
            fetched: "2026-09-16".into(),
            qualifier: "demo".into(),
            instruments: vec![],
        });
        assert_eq!(r.answer(), vike_catalog::CatalogAnswer::LocalCredentialed);
        let age = age_cell(&r, 1);
        assert_eq!(age, "yours 2026-09-16", "their own list, their own date");
        assert!(!age.contains("never"), "{age}");
        assert!(!age.contains("ago"), "a fetch DATE is not an age: {age}");
        assert!(!age.contains("shipped"), "it did not ship with the binary: {age}");

        let line = status_line(&r);
        assert!(line.contains("this list is YOURS"), "{line}");
        assert!(line.contains("vike-backend catalog refresh alpaca"), "{line}");
        assert!(
            !line.contains("not fetched with your credentials"),
            "the shipped caveat is a FALSE warning about their own list: {line}"
        );
        // …and the server's refusal still stands beside it, because the SERVER still refuses.
        assert!(line.contains("credentials"), "{line}");
    }

    /// **The operator's own fetch WINS, and the row stops mentioning the shipped one.**
    #[test]
    fn an_operator_fetch_outranks_a_shipped_list_even_at_zero() {
        let mut r = row("alpaca", None, None);
        r.baseline = Some(vike_catalog::BaselineVenue {
            venue: "alpaca".into(),
            fetched: "2026-09-16".into(),
            qualifier: "demo".into(),
            instruments: vec![],
        });
        r.stamp = Some(VenueStamp { venue: "alpaca".into(), last_refreshed_ms: 0, count: 0 });
        assert!(age_cell(&r, 1_000).contains("just now"), "{}", age_cell(&r, 1_000));
        assert!(
            !status_line(&r).contains("ships with vike"),
            "a measured venue must not advertise the list it no longer uses: {}",
            status_line(&r)
        );
        assert!(!status_line(&r).contains("this list is YOURS"), "{}", status_line(&r));
    }

    #[test]
    fn the_hover_says_which_box_the_press_spends() {
        assert!(press_hint(&direct("deribit")).contains("from its API"));
        let routed = press_hint(&row("binance", Some(CatalogSource::ServerBacked), None));
        assert!(routed.contains("datahub at 127.0.0.1:7878"), "{routed}");
    }
}
