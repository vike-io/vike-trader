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
//! what it says and where it sits, as `crates/vike-app-core/tests/account_window_screens.rs` does.
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

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use egui::epaint::Shape;
use egui::{Pos2, Rect, vec2};
use vike_app_core::data::catalog_refresh::CatalogRefresh;
use vike_app_core::data::data_sink::{BookStore, TradeStore};
use vike_app_core::data::md_session::{self, MdMount};
use vike_app_core::tools::{ToolData, ToolView};
use vike_app_core::ui::tool_views::{
    BookCtx, ControlLink, HistoryCtx, StoredCtx, ToolCtx, account_tool_content, trade_tool_content,
};
use vike_app_core::ui::workspace::{TOOL_BODY_MARGIN, WinKind, tool_title_bar};
use vike_change_journal::Proc;
use vike_datahub_client::{FEATURE_MARKET_DATA, md_venue_feature};
use vike_exec::{CoreSnapshot, OrderView, PositionView, VenueBlock};
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
use vike_model::events::PositionSide;
use vike_model::{AccountTier, MarginMode, OrderStatus, OrderType};
use vike_ui_theme::appearance::{self, Appearance};

#[path = "common/mod.rs"]
mod common;

/// The widths and pixel densities every test reads. 2560 is the capture's width; 640 is a small
/// window; 1200 the common one.
const WIDTHS: [f32; 3] = [640.0, 1200.0, 2560.0];
const DENSITIES: [f32; 2] = [1.0, 2.0];
/// The screen's height: the capture's.
const HEIGHT: f32 = 1600.0;
/// Passes per frame. The first lets the bundled fonts land (they apply on the NEXT pass); the
/// ticket remembers last frame's measurements to decide its layout, so it needs a few more to settle.
const PASSES: u32 = 5;
/// How long a test waits for the session's reconciler thread.
const SETTLE: Duration = Duration::from_secs(5);

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

/// Poll `f` until it answers `true`, or fail naming `what`. Every property awaited here is produced
/// by the session's own threads.
fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + SETTLE;
    while Instant::now() < deadline {
        if f() {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for: {what}");
}

/// The handshake of a datahub that serves market data for exactly `venues`.
fn features_for(venues: &[&str]) -> Vec<String> {
    let mut f = vec!["load_bars".to_string(), FEATURE_MARKET_DATA.to_string()];
    f.extend(venues.iter().copied().map(md_venue_feature));
    f
}

/// A datahub that answers the key-less handshake advertising `features` and nothing else: the one
/// connection the session's PROBE opens (it wants nothing yet) learns the advertisement from it.
fn spawn_handshake_only(features: Vec<String>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral loopback port");
    let addr = listener.local_addr().expect("the bound address");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let features = features.clone();
            thread::spawn(move || {
                let _ = common::accept_keyless(&mut stream, features);
            });
        }
    });
    addr
}

/// How the desktop's market-data link stands.
#[derive(Clone, Copy, Debug)]
enum Datahub {
    /// No datahub address resolved: the session has nothing to dial.
    Absent,
    /// An address that refuses the connection: what a node advertising a datahub the desktop's
    /// tunnel does not reach looks like (the capture's case).
    Refusing,
    /// A datahub that answers the handshake, advertising market data for exactly these venues.
    Serves(&'static [&'static str]),
}

/// The REAL market-data session and the maps it mounts, dialled as `datahub` says, mounted the way
/// the shell does it once per frame (`MdSession::sync_mount`).
struct Linked {
    mount: MdMount,
}

impl Linked {
    fn to(datahub: Datahub) -> Linked {
        // ⚠ BOUND, not `.path()` off a temporary: dropping the `TempDir` deletes the directory, and
        // the key resolution happens inside `build`. It holds no settings database, so no hub key.
        let no_store = tempfile::tempdir().expect("a scratch settings directory");
        let mut mount = md_session::build(
            no_store.path().to_str(),
            &HashMap::new(),
            "VIKE_DATAHUB_OBSERVE_KEY",
            Arc::new(BookStore::default()),
            Arc::new(TradeStore::default()),
            || {},
        );
        let session = mount.session.clone();
        match datahub {
            Datahub::Absent => {}
            Datahub::Refusing => {
                // Bind to learn a free port, then close it.
                let dead = TcpListener::bind("127.0.0.1:0")
                    .expect("bind")
                    .local_addr()
                    .expect("local addr");
                session.set_addr(Some(&dead.to_string()));
                until("the refused dial to be recorded", || {
                    session.link_status().contains("error")
                });
            }
            Datahub::Serves(venues) => {
                let addr = spawn_handshake_only(features_for(venues));
                session.set_addr(Some(&addr.to_string()));
                until("the probe to learn the advertisement", || session.caps().is_some());
                // The reconciler states the link AFTER it caches the advertisement, and rewrites
                // every mounted line with it: wait for it, or a line read now is not the one drawn.
                until("the reconciler to park on the advertisement", || {
                    session.link_status() == "idle"
                });
            }
        }
        session.sync_mount(&mut mount.feeds, &mut mount.feed_statuses);
        Linked { mount }
    }

    /// The venues the session mounted a feed for, sorted.
    fn mounted(&self) -> Vec<&'static str> {
        let mut venues: Vec<&'static str> = self.mount.feeds.keys().copied().collect();
        venues.sort_unstable();
        venues
    }

    /// One venue's status line, as the Trade window reads it.
    fn line(&self, venue: &str) -> Option<String> {
        self.mount.feed_statuses.get(venue).map(|h| h.lock().expect("a status line").clone())
    }
}

impl Drop for Linked {
    fn drop(&mut self) {
        // The same call `App::run_bounded_teardown` makes: no thread leaks past the test.
        self.mount.session.stop_and_join();
    }
}

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

/// Everything `ToolCtx` borrows, owned, so a pass can build one.
struct Owned {
    td: ToolData,
    snap: CoreSnapshot,
    textures: HashMap<String, egui::TextureHandle>,
    dsets: vike_data::datasets::Store,
    gaps: vike_data_manager::GapMap,
    partials: vike_data_manager::PartialDayMap,
    statuses: HashMap<String, Arc<Mutex<String>>>,
    proc: Proc,
    catalog: CatalogRefresh,
    symbols: Arc<vike_catalog::Catalog>,
    control: ControlLink,
    /// The window's `(venue, symbol)`; both empty is the window's first frame (nothing opened yet).
    window: (&'static str, &'static str),
}

impl Owned {
    fn new(snap: CoreSnapshot, linked: Option<&Linked>, control: ControlLink) -> Self {
        // The catalog handle does no I/O at construction.
        let (tx, _rx) = std::sync::mpsc::channel();
        Owned {
            td: ToolData::default(),
            snap,
            textures: HashMap::new(),
            dsets: vike_data::datasets::Store::default(),
            gaps: vike_data_manager::GapMap::default(),
            partials: vike_data_manager::PartialDayMap::default(),
            statuses: linked.map(|l| l.mount.feed_statuses.clone()).unwrap_or_default(),
            proc: Proc::new("vike-test", 4711, "0.1.0"),
            catalog: CatalogRefresh::new(Vec::new(), None, None, None, tx),
            symbols: Arc::new(vike_catalog::Catalog::from_instruments(Vec::new())),
            control,
            window: (VENUE, SYMBOL),
        }
    }

    fn ctx(&self) -> ToolCtx<'_> {
        ToolCtx {
            td: &self.td,
            snap: &self.snap,
            flags: &self.textures,
            logos: &self.textures,
            history: HistoryCtx::none(),
            feeds: &[],
            dsets: &self.dsets,
            display_tz: vike_chart::DisplayTz::default(),
            stored: StoredCtx {
                tree: &[],
                gaps: &self.gaps,
                partials: &self.partials,
                loading: false,
                load_error: None,
                backfill_status: "",
                delete_unavailable: None,
                partials_note: None,
                history: None,
            },
            feed_statuses: &self.statuses,
            credentials: vike_gateway_panel::CredentialWrite {
                settings_dir: Path::new(""),
                journal: None,
                proc: &self.proc,
                now_ms: 0,
            },
            // The window's book: none, as for a venue with no feed (the shell reads it from the
            // `BookStore`, which nothing has written).
            book: BookCtx {
                symbol: self.window.1,
                venue: self.window.0,
                book: None,
                stale: true,
                trades: None,
            },
            catalog: &self.catalog,
            symbols: &self.symbols,
            last_trade: None,
            directory: None,
            directory_unavailable: false,
            key_changed: &[],
            control_link: self.control,
            node_takes_post_only: false,
            datahub_unreachable: None,
        }
    }
}

/// One run of painted text: what it says, where it sits, the clip it is painted under, and whether
/// the layout cut it short.
#[derive(Clone, Debug)]
struct Txt {
    text: String,
    rect: Rect,
    clip: Rect,
    elided: bool,
}

/// One frame of a window at one width and density.
struct Frame {
    width: f32,
    ppp: f32,
    texts: Vec<Txt>,
}

fn flatten(shape: Shape, clip: Rect, out: &mut Vec<(Shape, Rect)>) {
    match shape {
        Shape::Vec(v) => v.into_iter().for_each(|s| flatten(s, clip, out)),
        s => out.push((s, clip)),
    }
}

/// Which window body a frame draws.
#[derive(Clone, Copy)]
enum Body {
    Trade,
    Account,
}

fn render(body: Body, owned: &Owned, width: f32, ppp: f32) -> Frame {
    let ctx = egui::Context::default();
    appearance::install(&ctx, &Appearance::default());
    let mut tv = ToolView::default();
    let mut shapes = Vec::new();
    for pass in 0..PASSES {
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, HEIGHT))),
            time: Some(f64::from(pass) / 60.0),
            ..Default::default()
        };
        raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point =
            Some(ppp);
        let mut full = ctx.run_ui(raw, |ui| match body {
            // As `tool_content` draws it: the bar reserves the view controls' slot, the body sits in
            // the tool margin and is handed that slot.
            Body::Trade => {
                let (_, _, slot) = tool_title_bar(ui, WinKind::Trade, false);
                egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                    trade_tool_content(ui, &owned.ctx(), &mut tv, &slot);
                });
            }
            Body::Account => account_tool_content(ui, &owned.ctx(), &mut tv),
        });
        full.textures_delta.clear();
        shapes.clear();
        for clipped in std::mem::take(&mut full.shapes) {
            flatten(clipped.shape, clipped.clip_rect, &mut shapes);
        }
    }
    let texts = shapes
        .iter()
        .filter_map(|(s, clip)| match s {
            Shape::Text(t) => Some(Txt {
                text: t.galley.text().to_string(),
                rect: t.galley.rect.translate(t.pos.to_vec2()),
                clip: *clip,
                elided: t.galley.elided,
            }),
            _ => None,
        })
        .collect();
    Frame { width, ppp, texts }
}

/// Every (width, density) the tests read, with its frame of `body`.
fn frames_of(body: Body, owned: &Owned) -> Vec<Frame> {
    let mut out = Vec::new();
    for w in WIDTHS {
        for d in DENSITIES {
            out.push(render(body, owned, w, d));
        }
    }
    out
}

/// [`frames_of`] the Trade window, which is what most tests read.
fn frames(owned: &Owned) -> Vec<Frame> {
    frames_of(Body::Trade, owned)
}

impl Frame {
    fn at(&self) -> String {
        format!("{}w x{}", self.width, self.ppp)
    }

    /// What the frame painted, for a failure message.
    fn dump(&self) -> String {
        let mut shown: Vec<String> = self
            .texts
            .iter()
            .filter(|t| !t.text.trim().is_empty())
            .map(|t| format!("{:?}{}", t.text, if t.elided { " [elided]" } else { "" }))
            .collect();
        shown.sort();
        shown.dedup();
        shown.join(" | ")
    }

    /// Whether `t` is painted whole: inside the clip it is painted under and inside the screen.
    fn is_whole(&self, t: &Txt) -> bool {
        const SLACK: f32 = 1.5;
        !t.elided
            && t.clip.expand(SLACK).contains_rect(t.rect)
            && t.rect.left() >= -SLACK
            && t.rect.right() <= self.width + SLACK
    }

    /// Pairs of painted texts whose rectangles overlap by more than a point each way.
    fn overlapping(&self) -> Vec<String> {
        let drawn: Vec<&Txt> = self.texts.iter().filter(|t| !t.text.trim().is_empty()).collect();
        let mut out = Vec::new();
        for (i, a) in drawn.iter().enumerate() {
            for b in &drawn[i + 1..] {
                let both = a.rect.intersect(b.rect);
                if both.width() > 1.0 && both.height() > 1.0 {
                    out.push(format!("{:?} overlaps {:?}", a.text, b.text));
                }
            }
        }
        out
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
        let owned = Owned::new(node(), Some(&linked), ControlLink::Open);
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
    let owned = Owned::new(node(), Some(&linked), ControlLink::Open);
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
        let read_only = Owned::new(node(), Some(&linked), ControlLink::ReadOnly);
        for f in frames(&read_only) {
            let mut here = Vec::new();
            // The ticket's line and the status strip's: two places.
            want_whole(&f, "the read-only line", |t| t == READ_ONLY, 2, &mut here);
            failures.extend(here.into_iter().map(|h| format!("{case}, read-only desktop, {h}")));
        }
        let open = Owned::new(node(), Some(&linked), ControlLink::Open);
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
    let mut owned = Owned::new(CoreSnapshot::empty("observing", ""), None, ControlLink::Open);
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
    let owned = Owned::new(node(), Some(&linked), ControlLink::ReadOnly);
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
