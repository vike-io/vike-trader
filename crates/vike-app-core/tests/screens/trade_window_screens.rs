//! **The Trade window, read off a real frame, from the datahub link to the words on screen.**
//!
//! The window was captured on a 2560-wide screen against the live node (v0.1.86). It drew
//! `NO DEPTH FEED FOR THIS VENUE` over an empty ladder and `Read-only: this desktop cannot send
//! orders.` in its ticket and its status strip. Every PIECE of that was tested (the widget in
//! `crates/vike-panels/tests/trade_no_book.rs`, the glue's `window_parts` in
//! `crates/vike-app-core/src/ui/tool_views/trade_tests.rs`), and the JOIN between them was not:
//!
//! ```text
//! datahub address  ->  md_session::build + MdSession::sync_mount  ->  App::feed_statuses
//!   ->  ToolCtx::feed_statuses  ->  trade_tool_content  ->  depth_absence  ->  the ladder's words
//! ```
//!
//! A venue the connected datahub does not advertise has NO status line, and that absence IS the
//! statement (`crates/vike-app-core/src/orders/dom_math.rs`'s `depth_absence`: `None` is "no
//! producer"). Nothing checked that the real mount leaves a venue's line out when no datahub is
//! connected, when the address refuses, or when the datahub serves other venues, nor that the
//! window then says it in words a person can read at the widths and pixel densities a desktop has.
//! Each test below builds the window the way the app does (`crates/vike-desktop/src/tool_window.rs`'s
//! `tool_content`: the title bar's slot, then `trade_tool_content` in the body margin) over the
//! REAL market-data session, which dials an in-process datahub that answers only the handshake, and
//! reads the painted text: a `Shape::Text`'s galley says whether it was elided (`Galley::elided`),
//! what it says and where it sits, as `crates/vike-app-core/tests/screens/account_window_screens.rs` does.
//!
//! ⚠ The window is read at 640, 1200 and 2560 points and at 1x and 2x, because every earlier test
//! ran on a canvas where nothing is ever cut.
//!
//! The same file holds the contact sheet's `07-trade` pose to the words the window paints when no
//! backend has answered (the sheet cannot reach a node): `scripts/qa_shots.sh` is read at run time,
//! never `include_str!`d, because the public mirror withholds `scripts/`.
//!
//! ⚠ It names twelve of the roster's fourteen venues, on purpose: a file that names all (or all but
//! one) is taken by `crates/vike-ops/tests/venues/new_venue_gate.rs` for a per-venue table. Keep two
//! venues out of it.

use std::path::Path;

use egui::{Pos2, Rect, vec2};
use vike_app_core::ui::tool_views::ControlLink;
use vike_exec::{CoreSnapshot, OrderView, PositionView, VenueBlock};
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
use vike_model::events::PositionSide;
use vike_model::{AccountTier, MarginMode, OrderStatus, OrderType};

use crate::tool_ctx::Owned;
use crate::trade_frames::{Body, Datahub, Frame, Linked, Txt, frames, frames_of};

/// What the Trade window says over an empty ladder for a venue with no market-data feed, as the
/// capture showed it. Literals, not the constants: a changed word must change this file too.
const NO_FEED_HEADLINE: &str = "NO DEPTH FEED FOR THIS VENUE";
const NOTHING_DRAWN: &str = "nothing is drawn below";
const NO_FEED_NEXT_STEP: &str = "advertises no market-data feed for it";
/// What a desktop with no control channel says, in its ticket and its status strip.
const READ_ONLY: &str = "Read-only: this desktop cannot send orders.";

/// The window under test: binance's default account, BTCUSDT.
const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

/// The account label `l` names.
fn label(l: &str) -> AccountLabel {
    AccountLabel::parse(l).expect("a fixture label parses")
}

/// One engine of the node: the account a venue block names and its tier.
fn engine(venue: &str, account: Option<&str>, tier: Option<AccountTier>) -> VenueBlock {
    let account = account.map(label);
    VenueBlock {
        venue: venue.to_string(),
        route_key: account.as_ref().map_or_else(|| venue.to_string(), |l| route_key_of(venue, l)),
        account,
        symbol: SYMBOL.to_string(),
        tier,
        equity: 5_000.0,
        free_bp: 4_980.0,
        // A current node publishes it for the venues that take any symbol.
        admits_symbols: venue == VENUE,
        ..Default::default()
    }
}

/// A working order on binance's default account.
fn working(i: usize, side: i32, price: f64, kind: OrderType) -> OrderView {
    OrderView {
        client_order_id: format!("vike-20261024-{i:04}-{VENUE}"),
        venue: VENUE.to_string(),
        account: None,
        symbol: SYMBOL.to_string(),
        side,
        qty: 0.0010,
        order_type: kind,
        price: Some(price),
        trigger_price: None,
        status: OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        data_delay: None,
        reason: None,
        pending_price: None,
    }
}

/// The live node's shape (v0.1.86): seventeen engines over twelve venues, the lane twin
/// (`binance#PERP`) and a second account of several venues among them, a LIVE account that is not
/// the Trade window's, a position and two working orders on the window's own account.
fn node() -> CoreSnapshot {
    let (d, l) = (Some(AccountTier::Demo), Some(AccountTier::Live));
    let mut blocks = vec![
        engine("binance", None, d),
        engine("okx", None, d),
        engine("hyperliquid", None, d),
        engine("aster", None, None),
        engine("deribit", None, d),
        engine("dukascopy", None, d),
        engine("ctrader", None, d),
        engine("ig", None, d),
        engine("oanda", None, d),
        engine("ibkr", None, d),
        engine("polymarket", None, l),
        engine("oanda", Some("SUB"), d),
        engine("ig", Some("SUB"), d),
        engine("binance", Some("PERP"), d),
        engine("bybit", None, d),
        engine("bybit", Some("LIVE"), l),
        engine("hyperliquid", Some("LIVE"), l),
    ];
    blocks[0].positions.push(PositionView {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        position_side: PositionSide::Both,
        size: 0.0010,
        avg_px: 98_000.0,
        unrealized: -0.31,
        mark_source: None,
        leverage: 1.0,
        liq_price: 0.0,
        margin_mode: MarginMode::Cross,
        isolated_margin: None,
    });
    let mut snap = CoreSnapshot::empty(VENUE, SYMBOL);
    snap.portfolio.venues = blocks;
    snap.portfolio.equity_total = 1_350_390.12;
    snap.orders =
        vec![working(1, 1, 97_000.0, OrderType::Limit), working(2, -1, 99_000.0, OrderType::Stop)];
    snap
}

/// The Trade window over `snap` (binance's default account, BTCUSDT) and `linked`'s status lines,
/// with `control` as its control link. The window's book: none, as for a venue with no feed (the
/// shell reads it from the `BookStore`, which nothing has written).
fn window_over(snap: CoreSnapshot, linked: Option<&Linked>, control: ControlLink) -> Owned {
    Owned {
        statuses: linked.map(|l| l.mount.feed_statuses.clone()).unwrap_or_default(),
        control,
        window: (VENUE, SYMBOL),
        stale: true,
        ..Owned::new(snap)
    }
}

/// Demand that `frame` paints at least `at_least` texts satisfying `is`, every one of them whole.
/// A failure is recorded, never raised, so a test reports every width and density at once.
fn want_whole(
    frame: &Frame,
    what: &str,
    is: impl Fn(&str) -> bool,
    at_least: usize,
    failures: &mut Vec<String>,
) {
    let found: Vec<&Txt> = frame.texts.iter().filter(|t| is(&t.text)).collect();
    if found.len() < at_least {
        failures.push(format!(
            "{}: {what}: {} painted, at least {at_least} wanted. Painted: {}",
            frame.at(),
            found.len(),
            frame.dump()
        ));
    }
    for t in found.iter().filter(|t| !frame.is_whole(t)) {
        failures.push(format!(
            "{}: {what} is cut: elided={} rect={:?} clip={:?}: {:?}",
            frame.at(),
            t.elided,
            t.rect,
            t.clip,
            t.text
        ));
    }
}

/// Demand that no text satisfying `is` is painted.
fn want_none(frame: &Frame, what: &str, is: impl Fn(&str) -> bool, failures: &mut Vec<String>) {
    let n = frame.texts.iter().filter(|t| is(&t.text)).count();
    if n > 0 {
        failures.push(format!(
            "{}: {what}: {n} painted, none wanted: {}",
            frame.at(),
            frame.dump()
        ));
    }
}

/// Panic with every failure at once.
fn all_ok(what: &str, failures: Vec<String>) {
    assert!(failures.is_empty(), "{what}:\n{}", failures.join("\n"));
}

/// ⚠ **A venue the connected datahub does not feed says so, in words, whole, at every width and
/// density** — over the real session in each way a desktop can fail to reach a ladder: no datahub
/// address at all, an address that refuses, and a datahub that serves other venues. The venue has no
/// status line in any of the three, which the test asserts of the mount first, so it is the mount's
/// output that is the window's input.
#[test]
fn a_venue_the_datahub_does_not_feed_says_so_whole_at_every_width_and_density() {
    let mut failures = Vec::new();
    for (case, datahub) in [
        ("no datahub", Datahub::Absent),
        ("a datahub that refuses", Datahub::Refusing),
        ("a datahub serving other venues", Datahub::Serves(&["bybit", "okx"])),
    ] {
        let linked = Linked::to(datahub);
        assert_eq!(linked.line(VENUE), None, "{case}: the mount left a line for {VENUE}");
        if let Datahub::Serves(venues) = datahub {
            assert_eq!(linked.mounted(), venues, "{case}: it mounts what it advertised");
        } else {
            assert!(linked.mounted().is_empty(), "{case}: nothing is advertised, nothing mounts");
        }
        let owned = window_over(node(), Some(&linked), ControlLink::Open);
        for f in frames(&owned) {
            let who = format!("{case}, ");
            let mut here = Vec::new();
            want_whole(&f, "the headline", |t| t == NO_FEED_HEADLINE, 1, &mut here);
            want_whole(
                &f,
                "the line that nothing is drawn",
                |t| t.contains(NOTHING_DRAWN),
                1,
                &mut here,
            );
            want_whole(&f, "the next step", |t| t.contains(NO_FEED_NEXT_STEP), 1, &mut here);
            failures.extend(here.into_iter().map(|h| format!("{who}{h}")));
        }
    }
    all_ok("the ladder must say why there is no depth", failures);
}

/// **The positive control**: a datahub that DOES advertise the venue gives it a status line, and the
/// window then says that line verbatim as the cause and never says `NO DEPTH FEED`. Without this the
/// test above would also pass for a window that always drew that headline.
#[test]
fn a_venue_the_datahub_feeds_is_not_called_unfed_and_its_line_is_the_cause() {
    let linked = Linked::to(Datahub::Serves(&["binance", "okx"]));
    let line = linked.line(VENUE).expect("the mount gave an advertised venue its line");
    assert!(!line.trim().is_empty(), "an advertised venue's line is never empty");
    let owned = window_over(node(), Some(&linked), ControlLink::Open);
    let mut failures = Vec::new();
    for f in frames(&owned) {
        want_none(&f, "the unfed headline", |t| t == NO_FEED_HEADLINE, &mut failures);
        want_none(&f, "the unfed next step", |t| t.contains(NO_FEED_NEXT_STEP), &mut failures);
        let cause = line.trim().to_string();
        want_whole(&f, "the link's own line", |t| t == cause, 1, &mut failures);
    }
    all_ok("an advertised venue must show its own link line", failures);
}

/// ⚠ **A desktop that holds no control key says `Read-only` in the order panel and in the strip**,
/// whole, at every width and density, with and without a datahub — and says it only then: the same
/// window over an open control link paints no such line.
#[test]
fn a_read_only_desktop_says_so_in_the_ticket_and_the_strip_and_an_open_one_does_not() {
    let mut failures = Vec::new();
    for (case, datahub) in [
        ("no datahub", Datahub::Absent),
        ("a datahub serving other venues", Datahub::Serves(&["okx"])),
    ] {
        let linked = Linked::to(datahub);
        let read_only = window_over(node(), Some(&linked), ControlLink::ReadOnly);
        for f in frames(&read_only) {
            let mut here = Vec::new();
            // The ticket's line and the status strip's: two places.
            want_whole(&f, "the read-only line", |t| t == READ_ONLY, 2, &mut here);
            failures.extend(here.into_iter().map(|h| format!("{case}, read-only desktop, {h}")));
        }
        let open = window_over(node(), Some(&linked), ControlLink::Open);
        for f in frames(&open) {
            let mut here = Vec::new();
            // The control: the window is there (it says why it has no ladder), and is not read-only.
            want_whole(&f, "the window itself", |t| t == NO_FEED_HEADLINE, 1, &mut here);
            want_none(&f, "the read-only line", |t| t.contains("Read-only"), &mut here);
            failures.extend(here.into_iter().map(|h| format!("{case}, open link, {h}")));
        }
    }
    all_ok("a read-only desktop must say so before the click", failures);
}

// ---- the contact sheet's `07-trade` pose --------------------------------------------------------

/// What the window paints on its first frame, before any node has named an instrument.
const WAITING: &str = "Waiting for the node.";

// Same spelling as `tests/qa_slugs.rs`'s `workspace_root` (keeps the `..`).
fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The script's text, or `None` when this checkout carries no `scripts/` (the public mirror).
fn qa_shots() -> Option<String> {
    std::fs::read_to_string(workspace_root().join("scripts").join("qa_shots.sh")).ok()
}

/// What the `<name>)` arm of the script's pose `case` hands the judge in its `field=` line (`pose`
/// or `look`), up to the arm's `;;` (a comment is for the maintainer and is not read); `None` when
/// the script has no such arm or the arm sets no such field.
fn pose_field(script: &str, name: &str, field: &str) -> Option<String> {
    let head = format!("{name})");
    let key = format!("{field}=");
    let mut lines = script.lines().skip_while(|l| l.trim() != head);
    lines.next()?;
    lines
        .take_while(|l| l.trim() != ";;")
        .find(|l| l.trim_start().starts_with(&key))
        .map(str::to_string)
}

/// ⚠ **The `07-trade` pose says what the window paints, in the window's own words.** The capture
/// runs no backend, and its pose once listed a price ladder, a ticket, an instrument bar and a status
/// strip it never draws, without once saying what the frame does hold. The window's first frame
/// (nothing opened, the observer's placeholder snapshot) is painted here, at every width and density:
/// it must hold ONE sentence, and both the arm's `pose=` and its `look=` must quote it, so a reworded
/// `WAITING_FOR_NODE` (`crates/vike-app-core/src/ui/tool_views/trade/reasons.rs`) or a reworded arm
/// reddens this, and so does a sentence the window gains, until the arm says it.
#[test]
fn the_contact_sheets_trade_pose_quotes_what_a_window_with_no_backend_paints() {
    let Some(script) = qa_shots() else {
        eprintln!(
            "SKIPPED the_contact_sheets_trade_pose_quotes_what_a_window_with_no_backend_paints: \
             scripts/qa_shots.sh is not in this checkout (the public mirror withholds scripts/); the \
             full tree runs it"
        );
        return;
    };
    let said = |field: &str| {
        pose_field(&script, "07-trade", field)
            .unwrap_or_else(|| panic!("the 07-trade arm of the sheet sets no {field}="))
    };
    // The discrimination: a field is its arm's line, not the script, and a name the script lacks is none.
    assert!(pose_field(&script, "99-not-a-capture", "pose").is_none());
    assert!(
        !pose_field(&script, "08-options", "look").expect("the 08-options arm").contains(WAITING),
        "pose_field leaked past its arm, so the check below would pass for any capture"
    );
    assert!(
        script
            .lines()
            .any(|l| l.starts_with("shot_app 07-trade ") && l.contains("VIKE_SHOT_WIN=trade")),
        "the matrix runs no 07-trade row, so its pose describes a capture nobody takes"
    );

    // `backend_conn`'s placeholder: the snapshot before a node has answered. No window venue or symbol.
    let mut owned = window_over(CoreSnapshot::empty("observing", ""), None, ControlLink::Open);
    owned.window = ("", "");
    let mut failures = Vec::new();
    for f in frames(&owned) {
        let sentences: Vec<&str> = f
            .texts
            .iter()
            .map(|t| t.text.as_str())
            .filter(|t| t.split_whitespace().count() >= 2)
            .collect();
        if sentences != [WAITING] {
            failures
                .push(format!("{}: the first frame paints {sentences:?}, not {WAITING:?}", f.at()));
        }
        for field in ["pose", "look"] {
            let line = said(field);
            for s in sentences.iter().filter(|s| !line.contains(**s)) {
                failures.push(format!("{}: the 07-trade {field}= does not quote {s:?}", f.at()));
            }
        }
    }
    all_ok("the 07-trade pose must quote the words the window paints", failures);
}

// ---- the live node's shape through the Account and Trade windows --------------------------------

/// **The overlap check can fail**: two texts one over the other are seen, two side by side are not.
/// Without this the replay below could be green because its check never sees anything.
#[test]
fn the_overlap_check_sees_two_texts_on_top_of_each_other_and_not_two_side_by_side() {
    let at = |x: f32, text: &str| Txt {
        text: text.to_string(),
        rect: Rect::from_min_size(Pos2::new(x, 0.0), vec2(40.0, 14.0)),
        clip: Rect::EVERYTHING,
        elided: false,
    };
    let frame = |texts| Frame { width: 100.0, ppp: 1.0, texts };
    assert_eq!(frame(vec![at(0.0, "a"), at(30.0, "b")]).overlapping().len(), 1);
    assert!(frame(vec![at(0.0, "a"), at(40.0, "b")]).overlapping().is_empty());
}

/// ⚠ **Replay of the live node's shape** (seventeen engines over twelve venues with the lane twin,
/// a position and two working orders; `node`) through the Account window and the Trade window as the
/// capture's desktop held it (no datahub, no control key): at 1200 and 2560 points and 1x and 2x no
/// text of either is elided, and no two texts lie over one another. It is a hand-built node, not a
/// recording: a serialized capture of the real one is what would make it a replay proper. The Chart
/// body is not here: `crates/vike-chart` is another change's.
#[test]
fn the_nodes_shape_renders_the_account_and_trade_windows_whole_and_apart_at_1200_and_2560() {
    let linked = Linked::to(Datahub::Absent);
    let owned = window_over(node(), Some(&linked), ControlLink::ReadOnly);
    let mut failures = Vec::new();
    for (name, body) in [("Account", Body::Account), ("Trade", Body::Trade)] {
        let wide: Vec<Frame> =
            frames_of(body, &owned).into_iter().filter(|f| f.width >= 1200.0).collect();
        assert_eq!(wide.len(), 4, "{name}: two widths at two densities");
        for f in &wide {
            let who = format!("{name} {}", f.at());
            for t in f.texts.iter().filter(|t| t.elided) {
                failures.push(format!("{who}: {:?} is elided", t.text));
            }
            failures.extend(f.overlapping().into_iter().map(|o| format!("{who}: {o}")));
            // The window is the one it is named for, and the node's rows reached it.
            let count = |s: &str| f.texts.iter().filter(|t| t.text == s).count();
            match body {
                Body::Account => {
                    if count("Orders (2)") != 1 || count("bybit") < 2 || count("hyperliquid") < 2 {
                        failures.push(format!("{who}: not the node's account table: {}", f.dump()));
                    }
                }
                Body::Trade => {
                    if count(NO_FEED_HEADLINE) != 1 || count(READ_ONLY) < 2 {
                        failures
                            .push(format!("{who}: not the capture's Trade window: {}", f.dump()));
                    }
                }
            }
        }
    }
    all_ok("the node's windows must render whole and apart", failures);
}
