//! The Trade window suites' shared rig (`trade_cause_line`, `trade_window_screens`): the REAL
//! market-data session dialled at an in-process datahub ([`Linked`]), and one frame of a window read
//! as painted text ([`render`], [`Frame`]). Both suites carried their own copy of each; the copies
//! differed only in which datahub cases they named, so there is one now.
//!
//! The window is built the way the app does (`crates/vike-desktop/src/tool_window.rs`'s
//! `tool_content`: the title bar's slot, then `trade_tool_content` in the body margin) and read as
//! `crates/vike-app-core/tests/screens/account_window_screens.rs` reads its own: a `Shape::Text`'s galley says
//! whether it was elided (`Galley::elided`), what it says and where it sits.
//!
//! The `screens` binary (`tests/screens.rs`) declares it ONCE, BESIDE `tool_ctx` and `poll`
//! (declared the same way, at the binary's root), which it names through `crate::`.
//! `allow(dead_code)`: a suite uses a subset (the cause-line suite reads no Account
//! body and counts no overlap).
#![allow(dead_code)]

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use egui::epaint::Shape;
use egui::{Pos2, Rect, vec2};
use vike_app_core::data::data_sink::{BookStore, TradeStore};
use vike_app_core::data::md_session::{self, MdMount};
use vike_app_core::tools::ToolView;
use vike_app_core::ui::tool_views::{account_tool_content, trade_tool_content};
use vike_app_core::ui::workspace::{TOOL_BODY_MARGIN, WinKind, tool_title_bar};
use vike_datahub_client::test_support::accept_keyless;
use vike_datahub_client::{FEATURE_MARKET_DATA, md_venue_feature};
use vike_ui_theme::appearance::{self, Appearance};

use crate::poll::until;
use crate::tool_ctx::{Owned, at_every_size};

/// The screen's height: the capture's.
pub const HEIGHT: f32 = 1600.0;
/// Passes per frame. The first lets the bundled fonts land (they apply on the NEXT pass); the
/// ticket remembers last frame's measurements to decide its layout, so it needs a few more to settle.
pub const PASSES: u32 = 5;

/// A datahub that answers the key-less handshake advertising market data for exactly `venues` and
/// nothing else: the one connection the session's PROBE opens (it wants nothing yet) learns the
/// advertisement from it.
pub fn spawn_handshake_only(venues: &[&str]) -> SocketAddr {
    let mut features = vec!["load_bars".to_string(), FEATURE_MARKET_DATA.to_string()];
    features.extend(venues.iter().copied().map(md_venue_feature));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral loopback port");
    let addr = listener.local_addr().expect("the bound address");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let features = features.clone();
            thread::spawn(move || {
                let _ = accept_keyless(&mut stream, features);
            });
        }
    });
    addr
}

/// How the desktop's market-data link stands.
#[derive(Clone, Copy, Debug)]
pub enum Datahub {
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
pub struct Linked {
    pub mount: MdMount,
    /// The address it dialled, when it dialled one.
    pub addr: Option<String>,
}

impl Linked {
    pub fn to(datahub: Datahub) -> Linked {
        // ⚠ BOUND, not `.path()` off a temporary: dropping the `TempDir` deletes the directory, and
        // the key resolution happens inside `build`. It holds no settings database, so no hub key.
        let no_store = tempfile::tempdir().expect("a scratch settings directory");
        let mut mount = md_session::build(
            no_store.path().to_str(),
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
                let dead = TcpListener::bind("127.0.0.1:0")
                    .expect("bind")
                    .local_addr()
                    .expect("local addr");
                session.set_addr(Some(&dead.to_string()));
                until("the refused dial to be recorded", || {
                    session.link_status().contains("error")
                });
                Some(dead.to_string())
            }
            Datahub::Serves(venues) => {
                let addr = spawn_handshake_only(venues);
                session.set_addr(Some(&addr.to_string()));
                until("the probe to learn the advertisement", || session.caps().is_some());
                // The reconciler states the link AFTER it caches the advertisement, and rewrites
                // every mounted line with it: wait for it, or a line read now is not the one drawn.
                until("the reconciler to park on the advertisement", || {
                    session.link_status() == "idle"
                });
                Some(addr.to_string())
            }
        };
        session.sync_mount(&mut mount.feeds, &mut mount.feed_statuses);
        Linked { mount, addr }
    }

    /// The venues the session mounted a feed for, sorted.
    pub fn mounted(&self) -> Vec<&'static str> {
        let mut venues: Vec<&'static str> = self.mount.feeds.keys().copied().collect();
        venues.sort_unstable();
        venues
    }

    /// One venue's status line, as the Trade window reads it.
    pub fn line(&self, venue: &str) -> Option<String> {
        self.mount.feed_statuses.get(venue).map(|h| h.lock().expect("a status line").clone())
    }
}

impl Drop for Linked {
    fn drop(&mut self) {
        // The same call `App::run_bounded_teardown` makes: no thread leaks past the test.
        self.mount.session.stop_and_join();
    }
}

/// One run of painted text: what it says, where it sits, the clip it is painted under, and whether
/// the layout cut it short.
#[derive(Clone, Debug)]
pub struct Txt {
    pub text: String,
    pub rect: Rect,
    pub clip: Rect,
    pub elided: bool,
}

/// One frame of a window at one width and density.
pub struct Frame {
    pub width: f32,
    pub ppp: f32,
    pub texts: Vec<Txt>,
}

fn flatten(shape: Shape, clip: Rect, out: &mut Vec<(Shape, Rect)>) {
    match shape {
        Shape::Vec(v) => v.into_iter().for_each(|s| flatten(s, clip, out)),
        s => out.push((s, clip)),
    }
}

/// Which window body a frame draws.
#[derive(Clone, Copy)]
pub enum Body {
    Trade,
    Account,
}

/// The texts one frame of `body` over `owned` painted at `width` points and `ppp` pixel density.
pub fn render(body: Body, owned: &Owned, width: f32, ppp: f32) -> Frame {
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

/// Every (width, density) the suites read, with its frame of `body`.
pub fn frames_of(body: Body, owned: &Owned) -> Vec<Frame> {
    at_every_size(|w, d| render(body, owned, w, d))
}

/// [`frames_of`] the Trade window, which is what most tests read.
pub fn frames(owned: &Owned) -> Vec<Frame> {
    frames_of(Body::Trade, owned)
}

impl Frame {
    pub fn at(&self) -> String {
        format!("{}w x{}", self.width, self.ppp)
    }

    /// What the frame painted, for a failure message.
    pub fn dump(&self) -> String {
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

    /// Whether `t` is painted whole: not elided, inside the clip it is painted under and inside the
    /// screen.
    pub fn is_whole(&self, t: &Txt) -> bool {
        const SLACK: f32 = 1.5;
        !t.elided
            && t.clip.expand(SLACK).contains_rect(t.rect)
            && t.rect.left() >= -SLACK
            && t.rect.right() <= self.width + SLACK
    }

    /// Pairs of painted texts whose rectangles overlap by more than a point each way.
    pub fn overlapping(&self) -> Vec<String> {
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
