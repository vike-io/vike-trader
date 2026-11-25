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
//! The `screens` binary (`tests/screens.rs`) declares it ONCE, BESIDE `tool_context` and `poll`
//! (declared the same way, at the binary's root), which it names through `crate::`.
//! `allow(dead_code)`: a suite uses a subset (the cause-line suite reads no Account
//! body and counts no overlap).
#![allow(dead_code)]

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use egui::epaint::Shape;
use egui::{Pos2, Rect, vec2};
use vike_app_core::data::data_sink::{L2BookStore, TickStore};
use vike_app_core::data::market_data_session::{self, MarketDataSessionMount};
use vike_app_core::ui::window::{TOOL_BODY_MARGIN, WindowKind, tool_title_bar};
use vike_app_core::ui::windows::WindowView;
use vike_app_core::ui::windows::{account_tool_content, trade_tool_content};
use vike_datahub_client::FEATURE_MARKET_DATA;
use vike_datahub_client::test_support::accept_keyless;
use vike_hub_protocol::features::market_data_venue_feature;
use vike_ui_theme::appearance::{self, ThemeAppearance};

use crate::poll::until;
use crate::tool_context::{Owned, at_every_size};

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
    features.extend(venues.iter().copied().map(market_data_venue_feature));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral loopback port");
    let address = listener.local_addr().expect("the bound address");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let features = features.clone();
            thread::spawn(move || {
                let _ = accept_keyless(&mut stream, features);
            });
        }
    });
    address
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
/// the shell does it once per frame (`MarketDataSession::sync_mount`).
pub struct Linked {
    pub mount: MarketDataSessionMount,
    /// The address it dialled, when it dialled one.
    pub address: Option<String>,
}

impl Linked {
    pub fn to(datahub: Datahub) -> Linked {
        // ⚠ BOUND, not `.path()` off a temporary: dropping the `TempDir` deletes the directory, and
        // the key resolution happens inside `build`. It holds no settings database, so no device key.
        let no_store = tempfile::tempdir().expect("a scratch settings directory");
        let mut mount = market_data_session::build(
            no_store.path().to_str(),
            Arc::new(L2BookStore::default()),
            Arc::new(TickStore::default()),
            || {},
        );
        let session = mount.session.clone();
        let address = match datahub {
            Datahub::Absent => None,
            Datahub::Refusing => {
                let dead = vike_app_core::test_support::dead_address();
                session.set_address(Some(&dead));
                until("the refused dial to be recorded", || {
                    session.link_status().state
                        == vike_model::feed_status::DataClientConnectionState::Error
                });
                Some(dead)
            }
            Datahub::Serves(venues) => {
                let address = spawn_handshake_only(venues);
                session.set_address(Some(&address.to_string()));
                until("the probe to learn the advertisement", || session.capabilities().is_some());
                // The reconciler states the link AFTER it caches the advertisement, and rewrites
                // every mounted line with it: wait for it, or a line read now is not the one drawn.
                until("the reconciler to park on the advertisement", || {
                    session.link_status().text == "idle"
                });
                Some(address.to_string())
            }
        };
        session.sync_mount(&mut mount.feeds, &mut mount.feed_statuses);
        Linked { mount, address }
    }

    /// The venues the session mounted a feed for, sorted.
    pub fn mounted(&self) -> Vec<&'static str> {
        let mut venues: Vec<&'static str> = self.mount.feeds.keys().copied().collect();
        venues.sort_unstable();
        venues
    }

    /// One venue's status line, as the Trade window reads it.
    pub fn line(&self, venue: &str) -> Option<String> {
        self.mount.feed_statuses.get(venue).map(|h| h.lock().expect("a status line").text.clone())
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
    let context = egui::Context::default();
    appearance::install(&context, &ThemeAppearance::default());
    let mut tv = WindowView::default();
    let mut shapes = Vec::new();
    for pass in 0..PASSES {
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, HEIGHT))),
            time: Some(f64::from(pass) / 60.0),
            ..Default::default()
        };
        raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point =
            Some(ppp);
        let mut full = context.run_ui(raw, |ui| match body {
            // As `tool_content` draws it: the bar reserves the view controls' slot, the body sits in
            // the tool margin and is handed that slot.
            Body::Trade => {
                let (_, _, slot) = tool_title_bar(ui, WindowKind::Trade, false);
                egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                    trade_tool_content(ui, &owned.context(), &mut tv, &slot);
                });
            }
            Body::Account => account_tool_content(ui, &owned.context(), &mut tv),
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
