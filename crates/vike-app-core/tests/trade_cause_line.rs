//! **The Trade window says WHY its ladder is empty when the datahub does not answer.**
//!
//! A desktop whose tunnel does not reach its datahub mounts no feed and writes no status line for any
//! venue (`MdSession::sync_mount` mounts what the server ADVERTISES, and nothing has answered), so the
//! window's cause line had nothing to say: the headline named an absent feed over an EMPTY cause.
//! "Connection refused" lived only in `MdSession::link_status` and the log. `MdSession::unreachable_why`
//! now words that fact, `ToolCtx::datahub_unreachable` carries it and the window states it as the cause.
//!
//! The test dials the REAL market-data session at a port nothing listens on, mounts it the way the
//! shell does once per frame (`MdSession::sync_mount`), builds the window as the app does
//! (`crates/vike-desktop/src/tool_window.rs`'s `tool_content`: the title bar's slot, then
//! `trade_tool_content` in the body margin) and reads the painted text at 640, 1200 and 2560 points
//! and 1x and 2x, because every earlier test ran on a canvas where nothing is ever cut. The technique
//! is `crates/vike-app-core/tests/account_window_screens.rs`'s: a `Shape::Text`'s galley says whether
//! it was elided, what it says and where it sits.
//!
//! ⚠ It names two of the roster's venues, on purpose: a file that names all (or all but one) is
//! taken by `crates/vike-ops/tests/venues/new_venue_gate.rs` for a per-venue table.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use egui::epaint::Shape;
use egui::{Pos2, Rect, vec2};
use vike_app_core::data::catalog_refresh::CatalogRefresh;
use vike_app_core::data::data_sink::{BookStore, TradeStore};
use vike_app_core::data::md_session::{self, MdMount};
use vike_app_core::tools::{ToolData, ToolView};
use vike_app_core::ui::tool_views::{
    BookCtx, ControlLink, HistoryCtx, StoredCtx, ToolCtx, trade_tool_content,
};
use vike_app_core::ui::workspace::{TOOL_BODY_MARGIN, WinKind, tool_title_bar};
use vike_change_journal::Proc;
use vike_datahub_client::{FEATURE_MARKET_DATA, md_venue_feature};
use vike_exec::{CoreSnapshot, VenueBlock};
use vike_model::AccountTier;
use vike_ui_theme::appearance::{self, Appearance};

#[path = "common/mod.rs"]
mod common;

const WIDTHS: [f32; 3] = [640.0, 1200.0, 2560.0];
const DENSITIES: [f32; 2] = [1.0, 2.0];
/// Passes per frame: the first lets the bundled fonts land, the ticket needs a few more to settle.
const PASSES: u32 = 5;
/// How long a test waits for the session's reconciler thread.
const SETTLE: Duration = Duration::from_secs(5);

/// What the window says over an empty ladder for a venue the datahub does not feed.
const NO_FEED_HEADLINE: &str = "NO DEPTH FEED FOR THIS VENUE";
/// The words of the cause, before the address. A literal, not the constant: a changed word must
/// change this file too.
const CANNOT_REACH: &str = "Cannot reach the datahub at";

/// The window under test: binance's default account.
const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

/// Poll `f` until it answers `true`, or fail naming `what`.
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

/// A datahub that answers the key-less handshake advertising market data for `venues` and nothing else.
fn spawn_handshake_only(venues: &[&str]) -> SocketAddr {
    let mut features = vec!["load_bars".to_string(), FEATURE_MARKET_DATA.to_string()];
    features.extend(venues.iter().copied().map(md_venue_feature));
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
    /// No datahub address resolved: nothing to dial.
    Absent,
    /// An address that refuses the connection: the tunnel does not reach the datahub.
    Refusing,
    /// A datahub that answers the handshake and serves another venue only.
    Answers,
}

/// The REAL market-data session, dialled as `datahub` says and mounted the way the shell mounts it.
struct Linked {
    mount: MdMount,
    /// The address it dialled, when it dialled one.
    addr: Option<String>,
}

impl Linked {
    fn to(datahub: Datahub) -> Linked {
        // ⚠ BOUND, not `.path()` off a temporary: the key resolution happens inside `build`.
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
        let addr = match datahub {
            Datahub::Absent => None,
            Datahub::Refusing => {
                // Bind to learn a free port, then close it.
                let dead =
                    TcpListener::bind("127.0.0.1:0").expect("bind").local_addr().expect("addr");
                session.set_addr(Some(&dead.to_string()));
                until("the refused dial to be recorded", || {
                    session.link_status().contains("error")
                });
                Some(dead.to_string())
            }
            Datahub::Answers => {
                let addr = spawn_handshake_only(&["okx"]);
                session.set_addr(Some(&addr.to_string()));
                until("the probe to learn the advertisement", || session.caps().is_some());
                until("the reconciler to park on the advertisement", || {
                    session.link_status() == "idle"
                });
                Some(addr.to_string())
            }
        };
        session.sync_mount(&mut mount.feeds, &mut mount.feed_statuses);
        Linked { mount, addr }
    }
}

impl Drop for Linked {
    fn drop(&mut self) {
        // The same call `App::run_bounded_teardown` makes: no thread leaks past the test.
        self.mount.session.stop_and_join();
    }
}

/// The node: binance's default account, trading BTCUSDT.
fn node() -> CoreSnapshot {
    let mut snap = CoreSnapshot::empty(VENUE, SYMBOL);
    snap.portfolio.venues = vec![VenueBlock {
        venue: VENUE.to_string(),
        route_key: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        tier: Some(AccountTier::Demo),
        equity: 5_000.0,
        free_bp: 4_980.0,
        admits_symbols: true,
        ..Default::default()
    }];
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
    statuses: HashMap<String, Arc<std::sync::Mutex<String>>>,
    proc: Proc,
    catalog: CatalogRefresh,
    symbols: Arc<vike_catalog::Catalog>,
    /// What the shell hands the window: the session's own answer, taken once per frame.
    unreachable: Option<String>,
}

impl Owned {
    fn new(linked: &Linked) -> Self {
        // The catalog handle does no I/O at construction.
        let (tx, _rx) = std::sync::mpsc::channel();
        Owned {
            td: ToolData::default(),
            snap: node(),
            textures: HashMap::new(),
            dsets: vike_data::datasets::Store::default(),
            gaps: vike_data_manager::GapMap::default(),
            partials: vike_data_manager::PartialDayMap::default(),
            statuses: linked.mount.feed_statuses.clone(),
            proc: Proc::new("vike-test", 4711, "0.1.0"),
            catalog: CatalogRefresh::new(Vec::new(), None, None, None, tx),
            symbols: Arc::new(vike_catalog::Catalog::from_instruments(Vec::new())),
            unreachable: linked.mount.session.unreachable_why(),
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
            // The window's book: none, as for a venue with no feed.
            book: BookCtx { symbol: SYMBOL, venue: VENUE, book: None, stale: true, trades: None },
            catalog: &self.catalog,
            symbols: &self.symbols,
            last_trade: None,
            directory: None,
            directory_unavailable: false,
            key_changed: &[],
            control_link: ControlLink::Open,
            node_takes_post_only: false,
            datahub_unreachable: self.unreachable.as_deref(),
        }
    }
}

/// One run of painted text: what it says, where it sits, the clip it is painted under, and whether
/// the layout cut it short.
struct Txt {
    text: String,
    rect: Rect,
    clip: Rect,
    elided: bool,
}

/// The texts one frame of the Trade window painted at `width` points and `ppp` pixel density.
fn render(owned: &Owned, width: f32, ppp: f32) -> Vec<Txt> {
    let ctx = egui::Context::default();
    appearance::install(&ctx, &Appearance::default());
    let mut tv = ToolView::default();
    let mut texts = Vec::new();
    for pass in 0..PASSES {
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1600.0))),
            time: Some(f64::from(pass) / 60.0),
            ..Default::default()
        };
        raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point =
            Some(ppp);
        let mut full = ctx.run_ui(raw, |ui| {
            // As `tool_content` draws it: the bar reserves the view controls' slot, the body sits in
            // the tool margin and is handed that slot.
            let (_, _, slot) = tool_title_bar(ui, WinKind::Trade, false);
            egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                trade_tool_content(ui, &owned.ctx(), &mut tv, &slot);
            });
        });
        full.textures_delta.clear();
        texts.clear();
        for clipped in std::mem::take(&mut full.shapes) {
            let mut flat = Vec::new();
            flatten(clipped.shape, &mut flat);
            for s in flat {
                if let Shape::Text(t) = s {
                    texts.push(Txt {
                        text: t.galley.text().to_string(),
                        rect: t.galley.rect.translate(t.pos.to_vec2()),
                        clip: clipped.clip_rect,
                        elided: t.galley.elided,
                    });
                }
            }
        }
    }
    texts
}

fn flatten(shape: Shape, out: &mut Vec<Shape>) {
    match shape {
        Shape::Vec(v) => v.into_iter().for_each(|s| flatten(s, out)),
        s => out.push(s),
    }
}

/// Whether `t` is painted whole: not elided, inside the clip it is painted under and inside the screen.
fn is_whole(t: &Txt, width: f32) -> bool {
    const SLACK: f32 = 1.5;
    !t.elided
        && t.clip.expand(SLACK).contains_rect(t.rect)
        && t.rect.left() >= -SLACK
        && t.rect.right() <= width + SLACK
}

/// Every (width, density) of `owned`'s window, each as `(label, width, texts)`.
fn frames(owned: &Owned) -> Vec<(String, f32, Vec<Txt>)> {
    let mut out = Vec::new();
    for w in WIDTHS {
        for d in DENSITIES {
            out.push((format!("{w}w x{d}"), w, render(owned, w, d)));
        }
    }
    out
}

/// ⚠ **A datahub that refuses the connection is NAMED as the cause**, in words, whole, at every width
/// and density: the window still says its headline (no feed for this venue), and beneath it the one
/// sentence that the datahub cannot be reached, with the address it tried. The venue has no status
/// line, which the test asserts of the mount first: it is the mount's output that is the window's input.
#[test]
fn a_datahub_that_refuses_is_named_as_the_cause_of_the_empty_ladder() {
    let linked = Linked::to(Datahub::Refusing);
    let addr = linked.addr.clone().expect("the refusing datahub has an address");
    assert!(linked.mount.feed_statuses.is_empty(), "nothing answered, so nothing mounted");
    let sentence = format!("{CANNOT_REACH} {addr}. Retrying.");
    let owned = Owned::new(&linked);
    let mut failures = Vec::new();
    if owned.unreachable.as_deref() != Some(sentence.as_str()) {
        failures.push(format!("the session says {:?}, not {sentence:?}", owned.unreachable));
    }
    for (at, width, texts) in frames(&owned) {
        let painted: Vec<&Txt> = texts.iter().filter(|t| t.text == sentence).collect();
        match painted.as_slice() {
            [one] if is_whole(one, width) => {}
            [one] => {
                failures.push(format!("{at}: the cause is cut: {:?} {:?}", one.rect, one.clip))
            }
            other => failures.push(format!(
                "{at}: {} lines say {sentence:?}, one wanted. Lines that name the datahub: {:?}",
                other.len(),
                texts
                    .iter()
                    .map(|t| t.text.as_str())
                    .filter(|t| t.contains("datahub"))
                    .collect::<Vec<_>>()
            )),
        }
        if !texts.iter().any(|t| t.text == NO_FEED_HEADLINE) {
            failures.push(format!("{at}: the window lost its headline {NO_FEED_HEADLINE:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "the refused datahub must be the stated cause:\n{}",
        failures.join("\n")
    );
}

/// **The positive controls**: a datahub that ANSWERS (serving other venues) and a desktop with no
/// datahub address are not "cannot reach" and the window never says so, while it is still the same
/// window (the headline is there). Without this the test above would also pass for a window that
/// always drew the sentence.
#[test]
fn a_datahub_that_answers_or_is_not_configured_is_never_called_unreachable() {
    let mut failures = Vec::new();
    for (case, datahub) in [("answers", Datahub::Answers), ("no address", Datahub::Absent)] {
        let linked = Linked::to(datahub);
        let owned = Owned::new(&linked);
        if owned.unreachable.is_some() {
            failures.push(format!("{case}: the session says {:?}", owned.unreachable));
        }
        for (at, _, texts) in frames(&owned) {
            if texts.iter().any(|t| t.text.contains(CANNOT_REACH)) {
                failures.push(format!("{case}, {at}: the window says it cannot reach the datahub"));
            }
            if !texts.iter().any(|t| t.text == NO_FEED_HEADLINE) {
                failures
                    .push(format!("{case}, {at}: not the Trade window: no {NO_FEED_HEADLINE:?}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "an answering datahub is not unreachable:\n{}",
        failures.join("\n")
    );
}
