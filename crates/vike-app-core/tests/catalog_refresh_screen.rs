//! **The Data Manager's Instruments destination, gated off the accessibility tree.**
//!
//! Four properties, each with the defect it exists for:
//!
//! 1. **⚠ A venue that cannot be refreshed is offered NO CONTROL.** Not a disabled button — none
//!    at all, with the reason in its Status cell instead. A `QueryBacked` venue has no bulk list to
//!    fetch; a credentialed venue can be listed only by spending the operator's identity, which no
//!    server will do on a client's request; and `ig`/`ibkr` publish no bulk list at any price. A
//!    button on any of those rows is a control that can never do anything, which is the thing this
//!    screen was written to avoid. The count is read off the REAL renderer, because a test of
//!    `RefreshAvailability` alone stays green with the button wired to every row.
//! 2. **⚠ A venue that CAN be refreshed through the backend IS offered one.** The other half of the
//!    same property and the one this PR adds: before it, thirteen of fourteen rows carried no
//!    control and one sentence — *"not in this build"* — which is true only in the narrow
//!    local-provider sense and tells an operator nothing can be done where a server can answer.
//! 3. **The stamp and the count reach the screen**, since they are the whole of how an operator
//!    tells a press that worked from one that did nothing.
//! 4. **A click returns the venue it was drawn beside.** The screen reports which row was pressed
//!    and the caller spawns the fetch; a renderer that returned the wrong venue would refresh one
//!    venue while the operator watched another.
//!
//! ⚠ **Nothing here spawns a fetch, and nothing here opens a socket.**
//! `instruments_screen` is a renderer that reports a click; the local half is
//! `crates/vike-app-core/tests/catalog_refresh.rs`'s business and the wire half is
//! `crates/vike-app-core/tests/catalog_wire.rs`'s. Keeping the three apart is what lets this file
//! run with no thread and no timeout.

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_app_core::catalog_refresh::{
    REFRESH_COOLDOWN_MS, RefreshOutcome, VenueCatalogRow, VenueRefreshState,
};
use vike_app_core::tool_views::{REFRESH_LABEL, instruments_screen};
use vike_catalog::{CatalogMode, CatalogSource, VenueStamp};

/// 2026-09-16T00:00:00Z.
const T: i64 = 1_789_516_800_000;

/// A row with an explicit ROUTE, which is what `availability()` reads. `server` is set, so a
/// `ServerBacked` row is pressable unless a test clears it.
fn routed(
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

/// The LINKED, locally-enumerable shape — the one venue the desktop has today.
fn direct(venue: &str) -> VenueCatalogRow {
    routed(venue, Some(CatalogSource::Direct), Some(CatalogMode::Enumerable))
}

/// The routed shape — a venue the backend's datahub lists.
fn server_backed(venue: &str) -> VenueCatalogRow {
    routed(venue, Some(CatalogSource::ServerBacked), None)
}

/// A venue the routing table sends NOWHERE. Which of the two reasons applies is read off
/// `vike_catalog::catalog_availability` by `availability()`, so the venue name IS the case.
fn unroutable(venue: &str) -> VenueCatalogRow {
    routed(venue, None, None)
}

fn stamped(venue: &str, count: usize, at: i64) -> VenueCatalogRow {
    let mut r = direct(venue);
    r.stamp = Some(VenueStamp { venue: venue.into(), last_refreshed_ms: at, count });
    r
}

/// One frame of the REAL screen over a fixed row set, with the click it reported.
struct Screen {
    harness: Harness<'static, ()>,
    clicked: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl Screen {
    fn new(rows: Vec<VenueCatalogRow>) -> Self {
        let total = rows.iter().map(VenueCatalogRow::count).sum();
        let clicked = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = std::sync::Arc::clone(&clicked);
        let harness = Harness::builder().with_size(egui::vec2(1100.0, 700.0)).build_ui(move |ui| {
            if let Some(v) = instruments_screen(ui, &rows, total, T) {
                *sink.lock().unwrap() = Some(v);
            }
        });
        Self { harness, clicked }
    }

    fn run(&mut self) {
        self.harness.run();
    }

    fn texts(&self) -> Vec<String> {
        self.harness
            .root()
            .children_recursive()
            .filter_map(|n| {
                let a = n.accesskit_node();
                a.label().map(|s| s.to_string()).or_else(|| a.value().map(|s| s.to_string()))
            })
            .collect()
    }

    fn contains(&self, needle: &str) -> bool {
        self.texts().iter().any(|t| t.contains(needle))
    }

    /// Every Refresh control the frame actually drew, in row order.
    fn refresh_buttons(&self) -> Vec<Node<'_>> {
        self.harness
            .root()
            .children_recursive()
            .filter(|n| {
                let a = n.accesskit_node();
                a.role() == Role::Button && a.label().as_deref() == Some(REFRESH_LABEL)
            })
            .collect()
    }
}

// ------------------------------------------------------------------------------------------------
// 1 & 2. The button is offered for exactly the venues that can use it — by ROUTE
// ------------------------------------------------------------------------------------------------

/// **Two routes get a control; three unroutable classes get a sentence.**
///
/// The count is the assertion, not the presence: five rows, exactly TWO controls, so a renderer
/// that drew a button on every routed-looking row fails here even though each row's
/// `availability()` is correct.
#[test]
fn a_control_is_drawn_for_exactly_the_two_routed_classes() {
    let mut screen = Screen::new(vec![
        direct("deribit"),
        server_backed("binance"),
        routed("okx", Some(CatalogSource::Direct), Some(CatalogMode::QueryBacked)),
        unroutable("alpaca"),
        unroutable("ig"),
    ]);
    screen.run();
    assert_eq!(
        screen.refresh_buttons().len(),
        2,
        "exactly the Direct and ServerBacked rows may carry a control: {:?}",
        screen.texts()
    );
    assert!(
        screen.contains("searched live per query"),
        "the QueryBacked row must SAY why it has no button: {:?}",
        screen.texts()
    );
    assert!(
        screen.contains("2 of 5 venues can be refreshed"),
        "the header counts the ROUTES, not the linked providers: {:?}",
        screen.texts()
    );
}

/// **⚠ An unroutable venue says WHY, in the SERVER-side type's own words, and never as a count.**
///
/// The three classes are different facts about the world and an operator's next move differs for
/// each: a credentialed venue is refused by construction and nothing arms it; a venue with no bulk
/// list is a venue property with nothing to rebuild. Rendering either as "0 instruments" — or as
/// the old *"not in this build"* — sends the operator to fix a packaging problem that does not
/// exist. This is `docs/decisions/0062`'s decision 5, read off the real accessibility tree.
#[test]
fn an_unroutable_venue_renders_its_reason_and_never_an_empty_list() {
    let mut screen = Screen::new(vec![
        unroutable("alpaca"),
        unroutable("oanda"),
        unroutable("ig"),
        unroutable("ibkr"),
    ]);
    screen.run();
    assert!(screen.refresh_buttons().is_empty(), "none of the four may carry a control");
    let texts = screen.texts();
    // The credentialed pair: the identity cost, named, with no switch to chase.
    assert!(
        texts.iter().any(|t| t.contains("alpaca") && t.contains("venue credentials")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("oanda") && t.contains("venue credentials")),
        "{texts:?}"
    );
    // The no-bulk-list pair: a VENUE property, with the sentence saying there is nothing to do.
    assert!(
        texts.iter().any(|t| t.contains("ig") && t.contains("publishes no bulk instrument list")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("ibkr") && t.contains("nothing to arm and nothing to rebuild")),
        "{texts:?}"
    );
    // ...and no ROW renders as a venue with no instruments. ⚠ The header legitimately reads
    // "0 instruments in the picker" — that is a measured claim about the CACHE, not about a venue
    // — so it is excluded by name rather than by loosening the check.
    assert!(
        !texts.iter().any(|t| t.contains("0 instruments") && !t.contains("in the picker")),
        "an empty list is the LIE this screen exists to avoid: {texts:?}"
    );
    assert!(
        !screen.contains("not in this build"),
        "…and so is telling the operator it is a packaging problem: {texts:?}"
    );
    // The count column still reads the em dash rather than a measured-looking 0.
    assert!(texts.iter().any(|t| t == "—"), "{texts:?}");
    assert!(screen.contains("0 of 4 venues can be refreshed"), "{texts:?}");
}

/// A `ServerBacked` row with NO datahub resolved keeps its control, DISABLED, with the sentence
/// naming the fix — this venue can be refreshed, just not from a desktop with nothing to ask.
/// (Removal is reserved for a venue that can never be refreshed at all; see the test above.)
#[test]
fn a_routed_venue_with_no_backend_keeps_a_disabled_button() {
    let mut orphan = server_backed("binance");
    orphan.server = None;
    let mut screen = Screen::new(vec![orphan, direct("deribit")]);
    screen.run();
    let buttons = screen.refresh_buttons();
    assert_eq!(buttons.len(), 2, "both routed rows keep a control");
    assert!(buttons[0].accesskit_node().is_disabled(), "the row with no datahub is refused");
    assert!(!buttons[1].accesskit_node().is_disabled(), "the local one is unaffected");
}

// ------------------------------------------------------------------------------------------------
// 3. The stamp and the count reach the screen
// ------------------------------------------------------------------------------------------------

/// **The count and "last refreshed" are both rendered**, and a venue that has never been fetched
/// reads `—` / `never` rather than a `0` that looks like a measured empty list.
#[test]
fn the_stamp_and_the_count_reach_the_screen() {
    let mut screen = Screen::new(vec![stamped("deribit", 2431, T - 600_000), server_backed("okx")]);
    screen.run();
    let texts = screen.texts();
    assert!(texts.iter().any(|t| t == "2431"), "the cached count renders: {texts:?}");
    assert!(texts.iter().any(|t| t == "10 min ago"), "the stamp renders as elapsed: {texts:?}");
    assert!(texts.iter().any(|t| t == "never"), "an unfetched venue reads never: {texts:?}");
    assert!(
        texts.iter().any(|t| t == "—"),
        "…and an em dash rather than a measured-looking 0: {texts:?}"
    );
    assert!(screen.contains("2431 instruments in the picker"), "{texts:?}");
}

/// A completed attempt's OUTCOME is on the screen, including a failure and what it kept — the one
/// thing an operator pressed the button to find out.
#[test]
fn a_failure_is_reported_on_its_own_row() {
    let mut failed = stamped("deribit", 2431, T - 600_000);
    failed.state = VenueRefreshState::Done {
        at_ms: T - REFRESH_COOLDOWN_MS * 2,
        outcome: RefreshOutcome::Failed { error: "503 Service Unavailable".into(), kept: 2431 },
    };
    let mut busy = server_backed("okx");
    busy.state = VenueRefreshState::InFlight { since_ms: T - 500 };

    let mut screen = Screen::new(vec![failed, busy]);
    screen.run();
    assert!(screen.contains("kept the 2431 already cached"), "{:?}", screen.texts());
    assert!(screen.contains("503 Service Unavailable"), "the venue's own words reach the row");
    assert!(screen.contains("fetching"), "an in-flight row says so");
    assert_eq!(
        screen.refresh_buttons().iter().filter(|b| !b.accesskit_node().is_disabled()).count(),
        1,
        "the in-flight row's button is disabled; the settled one's is live"
    );
}

/// **A REFUSAL that came back over the wire renders as itself**, not as a failure and not as a
/// count — and the row keeps its control, because a server the operator then arms can answer.
#[test]
fn a_wire_refusal_renders_the_servers_sentence_on_its_own_row() {
    let mut unarmed = server_backed("binance");
    unarmed.state = VenueRefreshState::Done {
        at_ms: T - REFRESH_COOLDOWN_MS * 2,
        outcome: RefreshOutcome::Refused {
            why: "this datahub serves no venue catalog because its operator REFUSED the lane — \
                  `venue_catalog_off = true` in <project>/settings/flags.toml. Delete that and \
                  restart it to serve again."
                .into(),
            kept: 0,
        },
    };
    let mut screen = Screen::new(vec![unarmed]);
    screen.run();
    assert!(screen.contains("venue_catalog_off"), "{:?}", screen.texts());
    assert!(screen.contains("restart it"), "{:?}", screen.texts());
    assert!(
        !screen.contains("answered with nothing"),
        "an unarmed lane is not a venue with no instruments: {:?}",
        screen.texts()
    );
    assert_eq!(screen.refresh_buttons().len(), 1, "the row keeps its control");
}

/// A venue refreshed seconds ago has its button DISABLED rather than removed — the control still
/// belongs on that row, it is simply not spendable yet.
#[test]
fn a_just_refreshed_venue_keeps_a_disabled_button() {
    let mut recent = stamped("deribit", 12, T - 1_000);
    recent.state = VenueRefreshState::Done {
        at_ms: T - 1_000,
        outcome: RefreshOutcome::Refreshed { count: 12, previous: 11, truncated: false },
    };
    let mut screen = Screen::new(vec![recent]);
    screen.run();
    let buttons = screen.refresh_buttons();
    assert_eq!(buttons.len(), 1, "the row keeps its control");
    assert!(
        buttons[0].accesskit_node().is_disabled(),
        "…and it is refused while the budget is spent"
    );
}

/// **A TRUNCATED listing says so on the row.** Nothing else on this screen would tell an operator
/// that the picker is missing the tail of a venue's universe.
#[test]
fn a_truncated_listing_says_so_on_its_row() {
    let mut short = server_backed("polymarket");
    short.state = VenueRefreshState::Done {
        at_ms: T - REFRESH_COOLDOWN_MS * 2,
        outcome: RefreshOutcome::Refreshed { count: 25_000, previous: 0, truncated: true },
    };
    let mut screen = Screen::new(vec![short]);
    screen.run();
    assert!(screen.contains("TRUNCATED"), "{:?}", screen.texts());
}

// ------------------------------------------------------------------------------------------------
// 4. A click names its own row
// ------------------------------------------------------------------------------------------------

/// **The click returns the venue it was drawn beside.** Three rows, two of them routed by DIFFERENT
/// routes; the second control is pressed, and the screen must report that one — a renderer that
/// reported the first would refresh a venue the operator was not looking at.
#[test]
fn a_click_returns_the_venue_of_its_own_row() {
    let mut screen = Screen::new(vec![direct("deribit"), unroutable("ig"), server_backed("okx")]);
    screen.run();
    let buttons = screen.refresh_buttons();
    assert_eq!(buttons.len(), 2, "two routed rows draw two controls");
    buttons[1].click();
    drop(buttons);
    screen.run();
    assert_eq!(
        screen.clicked.lock().unwrap().as_deref(),
        Some("okx"),
        "the press must name the row it was drawn on, not the first refreshable one"
    );
}
