//! LIVE smoke: THIS binary's own GUI, driven headlessly against the RUNNING trading node, on DEMO
//! accounts. `#[ignore]`d like every `*_smoke.rs`, never scheduled, and run by hand on the box that
//! runs the node (nobody can click the desktop's window in an agent session).
//!
//! # The wiring is `main`'s `--observe` arm, not a mock
//!
//! [`boot`] runs `main.rs`'s own `resolve_settings` (the `vike_boot::boot` walk over
//! `$VIKE_SETTINGS_DIR`: the project's settings rows, its credential store under the
//! `VIKE_NO_LIVE_KEYS` scope, and the hub-key table the desktop's `workspace_credentials` overlays),
//! and [`Desk::launch`] builds `App::new` from `startup_observe(["--observe", <config.node_addr>])`
//! exactly as `main` does. Every frame is the shell's own `eframe::App::ui` inside an `egui_kittest`
//! harness; every order leaves through the Trade window's own Buy button and the shell's own
//! dispatch; every answer is read back from the snapshot the node pushed to that very `App`.
//!
//! Three departures from `main`, each so the project is only READ:
//! - `STATE_DIR` and the workspace family are a scratch directory, set BEFORE the boot (it fills an
//!   empty cell only), so the layouts and `last_layout.txt` this run writes never land
//!   in the project's `settings/state`;
//! - no `vike_log::init`, no `journal_boot_settings` anchor and no user indicators (each writes, or
//!   is not about orders);
//! - the order tests hand the instruments they trade to the launched desktop's catalog
//!   ([`Desk::stage`], `DatahubCatalogRefresh::adopt`; nothing is written to the datahub's lists): the desktop sizes an order by its catalog's lot, and the datahub lists none
//!   for ctrader (a credentialed venue), ig (no bulk list) or dukascopy's bundled pairs (a pip, no
//!   lot). The staged row carries the venue's minimum as its lot; the bracket test and the read-only
//!   pass add a five-place FX tick (a TP/SL ticket offers no exits without a tick grid, and there is
//!   no book to take one from: see below), the id test stages none.
//!
//! # The thin client has no price for these venues
//!
//! The datahub mounts market data for the crypto venues only (the handshake's `market_data_venue=` rows), so
//! a Trade window on ctrader, ig or dukascopy shows no book, its Bid and Ask are disabled, a MARKET
//! order is refused for want of a price, and a LIMIT order needs its price typed. The tests do what
//! a trader does: [`Desk::reference`] takes the window's best bid where a feed exists, and otherwise
//! the ECB's public EUR/USD reference rate ([`ecb_eurusd`], the kit's own source, read with the
//! box's `curl`), then types a price 4 percent under it. A far limit and a refused bracket need
//! nothing nearer.
//!
//! The credential store and the hub keys are read through the store's read-only open; no key value
//! is printed, logged or written anywhere. A store opened under any scope but `NoLiveKeys` is REFUSED.
//!
//! # What each test proves (run ONE per process)
//!
//! Each test is a child of this file, under `desktop_live_smoke/` (`order_ids.rs`, `brackets.rs`,
//! `reads.rs`); the boot, the `Desk` driver and the shared checks stay here.
//!
//! - `desktop_order_ids_are_session_unique_on_the_live_node`: a far LIMIT BUY 1000 (4 percent under
//!   the reference price, never marketable) on the cTrader demo default account's EURUSD, three times: in a first
//!   launch, in a second launch with the same workspace, and after that launch loads a layout with
//!   FEWER windows. Each id matches `^(ui|dom|polymarket|opt)[0-9a-f]{8}[0-9]+$` and is ACCEPTED with no
//!   `already used` refusal; the second session differs from the first; the third keeps the second's
//!   session with a larger sequence. Each order is cancelled through the desktop's own control link
//!   (`CommandWire::Cancel`, by its id) as soon as it is read back, so at most one rests at a time.
//! - `desktop_bracket_tickets_on_the_live_node`: a BUY with TP/SL 1 percent away through the ticket
//!   on the ig and dukascopy demo engines' own symbols (a LIMIT entry 4 percent under the reference
//!   price, so an unexpected acceptance cannot fill) is REFUSED WHOLE: three legs `Rejected`, the
//!   culprit's reason `REDUCE_ONLY_UNSUPPORTED: kind=stop venue=ig` / `kind=limit venue=dukascopy`,
//!   its two siblings `BRACKET_ATOMIC_REFUSED`, no position. The same ticket on the cTrader demo
//!   engine's own symbol is ACCEPTED whole: the far limit entry rests and its two exits are held
//!   for it (`held_exits`); cancelling the entry takes them with it and the book is flat.
//! - `desktop_reads_the_live_node`: READ-ONLY. Opens the Trade window on each target and prints what
//!   a trader would see (the catalog's grid, the book, the strip, Buy and TP/SL, the feeds the
//!   datahub mounted, the stored price), sending nothing.
//!
//! # Before and after
//!
//! Each order test REFUSES to send unless the venue's default-account engine is DEMO tier, `Active`,
//! flat and has nothing resting, and asserts the same at the end. On a panic mid-way, `Desk`'s drop
//! cancels what it placed and flattens what it opened through the same control link.
//!
//! ```sh
//! VIKE_SETTINGS_DIR=<project>/settings VIKE_NO_LIVE_KEYS=true \
//!   cargo test -p vike-desktop --bin vike-desktop -- --ignored --nocapture --exact \
//!   desktop_live_smoke::order_ids::desktop_order_ids_are_session_unique_on_the_live_node
//! ```
//!
//! # What it does NOT prove
//!
//! Pixels and a real pointer: kittest steps `Context::run_ui` on the CPU and clicks through the
//! accessibility tree, so a widget that draws wrong, a click a real mouse would miss, focus taken by
//! the OS, the GPU renderer and eframe's window loop are all outside it. A "restart" is a second `App`
//! in the same process (the session is drawn per `App`, as per process).

#[path = "desktop_live_smoke/brackets.rs"]
mod brackets;
#[path = "desktop_live_smoke/order_ids.rs"]
mod order_ids;
#[path = "desktop_live_smoke/reads.rs"]
mod reads;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::accesskit::{Role, Toggled};
use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
use vike_app_core::ui::window::{WindowKind, WindowState};
use vike_execution::{CoreSnapshot, OrderView, VenueBlock};
use vike_model::{AccountTier, OrderStatus, TradingState};
use vike_tradehub_client::wire::CommandWire;

use super::{App, STATE_DIR, gui_env, resolve_settings, settings};

/// One frame per this much wall time: the observe bridge, the control link and the datahub session
/// run on their own threads in real time, and a frame is what lands their results.
const FRAME: Duration = Duration::from_millis(40);
/// How long the node gets to push its first snapshot and accept the control link.
const NODE_WAIT: Duration = Duration::from_secs(60);
/// How long an order gets to show up in the snapshot, and to reach its next state.
const ORDER_WAIT: Duration = Duration::from_secs(45);
/// How long the catalog and a book get to reach a Trade window.
const DATA_WAIT: Duration = Duration::from_secs(60);
/// The far limit's distance under the bid: never marketable.
const FAR: f64 = 0.96;
/// The order client id test's venue and symbol: the cTrader demo default account, as the kit's M1 names it.
const ORDER_CLIENT_ID_AT: (&str, &str) = ("ctrader", "EURUSD");
/// The size the ticket types: the venues' minimum on these accounts (ig's is one contract).
const SIZE_FX: &str = "1000";
const SIZE_IG: &str = "1";

/// One test per process: the boot, the workspace declaration and the scratch are process-wide.
fn one_per_process() {
    static RAN: AtomicBool = AtomicBool::new(false);
    assert!(
        !RAN.swap(true, Ordering::SeqCst),
        "run ONE desktop live smoke per process (nextest does; with cargo test pass --exact <name>)"
    );
}

/// What [`boot`] resolved: the node to dial and the scratch the workspace family lives in.
struct Boot {
    node: String,
    _scratch: tempfile::TempDir,
}

/// `main`'s boot, once per process, or `None` (with a SKIP line) where this box has no node to
/// smoke. Panics, sending nothing, where it would read a live key.
fn boot() -> Option<&'static Boot> {
    static BOOT: OnceLock<Option<Boot>> = OnceLock::new();
    BOOT.get_or_init(|| {
        vike_log::test_init();
        let scratch =
            tempfile::Builder::new().prefix("desktop-live-smoke").tempdir().expect("scratch");
        let state = scratch.path().join("state");
        std::fs::create_dir_all(&state).expect("the scratch state directory");
        // ⚠ BEFORE the boot: `resolve_settings` only fills an EMPTY cell, so the catalog cache and
        // everything else keyed on `STATE_DIR` stays in the scratch directory.
        let _ = STATE_DIR.set(Some(state.clone()));
        assert_eq!(
            STATE_DIR.get(),
            Some(&Some(state.clone())),
            "STATE_DIR was set before the smoke"
        );
        let booted = resolve_settings().unwrap_or_else(|e| panic!("the boot refused: {e}"));
        let Some(dir) = booted.settings_dir.clone() else {
            eprintln!("SKIP: no project settings directory (set VIKE_SETTINGS_DIR)");
            return None;
        };
        assert_eq!(
            booted.store.scope(),
            vike_secrets::CredentialScope::NoLiveKeys,
            "REFUSED: the credential store is open under a scope that reads live keys; run with \
             VIKE_NO_LIVE_KEYS=true"
        );
        let Some(node) = settings().config.node_addr.clone() else {
            eprintln!("SKIP: {} names no node (config.node_addr)", dir.display());
            return None;
        };
        // One plain connect to the node's own address (the thin client's only peer), not the observe
        // handshake: a node that is up and refuses the keys still fails loudly in `Desk::launch`.
        let mut addrs =
            std::net::ToSocketAddrs::to_socket_addrs(node.as_str()).into_iter().flatten();
        let dial = |a| std::net::TcpStream::connect_timeout(&a, Duration::from_secs(3));
        if !addrs.any(|a| dial(a).is_ok()) {
            eprintln!("SKIP: no node answers at {node} (config.node_addr)");
            return None;
        }
        vike_app_core::ui::window::workspace::declare_project_state_dir(Some(state.clone()))
            .expect("the scratch workspace family");
        let ws = vike_app_core::ui::window::workspace::path().expect("a workspace path");
        assert!(
            ws.starts_with(&state),
            "the workspace file {} is not the scratch one",
            ws.display()
        );
        stage_layouts();
        eprintln!(
            "desktop live smoke: node {node}, project {}, scratch {}",
            dir.display(),
            state.display()
        );
        Some(Boot { node, _scratch: scratch })
    })
    .as_ref()
}

/// Three chart windows as the workspace both launches start from, and a named layout of ONE: the
/// two shapes that made the old window counter repeat an id.
fn stage_layouts() {
    use vike_app_core::ui::window::workspace::{save, save_layout};
    let chart = |i: u8, interval: &str| {
        let at = egui::pos2(60.0 + 40.0 * f32::from(i), 80.0);
        let r = egui::Rect::from_min_size(at, egui::vec2(480.0, 320.0));
        WindowState::new(&format!("smoke-chart-{i}"), "BTCUSDT", interval, WindowKind::Chart, r)
    };
    let mut three = [chart(0, "1m"), chart(1, "5m"), chart(2, "15m")];
    three.iter_mut().for_each(|w| w.venue = "binance".into()); // `WindowState::new` names no venue
    save(&three, &[]).expect("the scratch workspace.json");
    save_layout("fewer", &three[..1], &[]).expect("the layout");
    // ...and "three" LAST: a save records the last-active layout, which a launch restores first.
    save_layout("three", &three, &[]).expect("the layout");
}

/// EUR/USD from the ECB's public reference rate, read as the kit's live tests read it: the box's own
/// `curl` against a public JSON file (no key, no venue). One rate a day is all a far limit needs.
fn ecb_eurusd() -> f64 {
    let url = "https://api.frankfurter.dev/v1/latest?base=EUR&symbols=USD";
    let out = std::process::Command::new("curl").args(["-sL", "-m", "10", url]).output();
    let body = String::from_utf8_lossy(&out.expect("curl runs").stdout).into_owned();
    let rate = body.split("\"USD\":").nth(1).and_then(|r| r.split(['}', ',']).next());
    let rate = rate.and_then(|r| r.trim().parse::<f64>().ok()).filter(|r| (0.5..2.0).contains(r));
    rate.unwrap_or_else(|| panic!("no EUR/USD rate from {url}: {body}"))
}

/// One instrument the order tests trade: venue, symbol, base, quote, the venue's minimum size (its
/// lot too) and its tick (`0.0`: none staged).
type Staged = (String, String, String, String, f64, f64);

/// The tick staged for an FX pair on a TP/SL ticket: five places. The ticket sets a bracket's exits
/// on the tick grid and offers none without one (`vike-order-entry`'s `sizing::exits`), and with no book
/// there is no tick but the catalog's.
const FX_TICK: f64 = 0.00001;

/// The staged row of `symbol` on `venue`: a six-letter pair, or an ig epic (`CS.D.EURGBP.MINI.IP`)
/// whose third field is one.
fn staged(venue: &str, symbol: &str, min: f64, tick: f64) -> Staged {
    let pair = symbol.split('.').nth(2).filter(|p| p.len() == 6).unwrap_or(symbol);
    let (base, quote) = pair.split_at(3.min(pair.len()));
    (venue.to_string(), symbol.to_string(), base.to_string(), quote.to_string(), min, tick)
}

/// The kept lists `rows` stand for, one per venue, stamped now. Printed, because it is the one thing here a
/// trader's box would get from elsewhere (`vike-backend catalog refresh`, a venue's own list).
fn staged_lists(rows: &[Staged]) -> Vec<vike_catalog::VenueInstrumentList> {
    let mut lists: Vec<vike_catalog::VenueInstrumentList> = Vec::new();
    for (venue, symbol, base, quote, min, tick) in rows {
        eprintln!("STAGED catalog row: {venue} {symbol} lot {min} min {min} tick {tick}");
        let properties = vike_model::SymbolProperties {
            step_size: *min,
            min_qty: *min,
            tick_size: *tick,
            ..Default::default()
        };
        let fx = vike_model::Contract::Fx(vike_model::ContractFx::default());
        let row =
            vike_model::Instrument::new(venue, symbol, base, quote, fx).with_properties(properties);
        match lists.iter_mut().find(|l| l.venue == *venue) {
            Some(list) => list.instruments.push(row),
            None => lists.push(vike_catalog::VenueInstrumentList {
                venue: venue.clone(),
                refreshed_ms: Some(vike_model::now_ms()),
                truncated: false,
                last_error: None,
                last_try_ms: None,
                instruments: vec![row],
                added_by_operator: Vec::new(),
            }),
        }
    }
    lists
}

/// The harness state: the shell's `App` and the frame eframe would hand it.
struct Shell {
    app: Option<App>,
    frame: eframe::Frame,
}

/// One launched desktop, plus what it placed and opened (for the drop's cleanup).
struct Desk {
    h: Harness<'static, Shell>,
    placed: Vec<String>,
    opened: Vec<(String, String)>,
}

impl Desk {
    /// `main`'s `--observe` arm on `boot.node`, then frames until the node's first snapshot and the
    /// control link are in. `None` (SKIP) where the hub-key store holds no control key.
    fn launch(boot: &Boot) -> Option<Desk> {
        let h = Harness::builder().with_size(egui::vec2(1920.0, 1080.0)).build_ui_state(
            |ui, s: &mut Shell| {
                let Shell { app, frame } = s;
                if let Some(app) = app.as_mut() {
                    eframe::App::ui(app, ui, frame);
                }
            },
            Shell { app: None, frame: eframe::Frame::_new_kittest() },
        );
        let argv = ["vike-desktop", "--observe", boot.node.as_str()].map(String::from);
        let startup = vike_app_core::backend::connection::startup_observe(argv);
        let backend = vike_app_core::backend::split_plane::observes(false, startup.requested)
            .then_some(startup.record);
        let app = App::new(&h.ctx, None, backend, gui_env(), Default::default());
        assert!(app.env.shot.is_none(), "VIKE_SHOT keeps the deterministic `ui-3-0` ids: unset it");
        let mut d = Desk { h, placed: Vec::new(), opened: Vec::new() };
        d.h.state_mut().app = Some(app);
        if d.app().tradehub_control().is_none() {
            eprintln!("SKIP: no control link: the hub-key store names no tradehub control key");
            return None;
        }
        let up = d.wait("the node's first snapshot and the control link", NODE_WAIT, |d| {
            let a = d.app();
            !a.snapshot_cell.load().portfolio.venues.is_empty()
                && a.tradehub_control().is_some_and(|c| c.is_connected())
        });
        assert!(up, "the desktop never connected to {} (status: {:?})", boot.node, d.app().status);
        Some(d)
    }

    fn app(&self) -> &App {
        self.h.state().app.as_ref().expect("launched")
    }

    fn snapshot(&self) -> Arc<CoreSnapshot> {
        self.app().snapshot_cell.load_full()
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.h.step();
            std::thread::sleep(FRAME);
        }
    }

    /// Frames until `done`, at most `limit`; `false` (and a line) on a timeout.
    fn wait(&mut self, what: &str, limit: Duration, done: impl Fn(&Desk) -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < limit {
            if done(self) {
                return true;
            }
            self.frames(1);
        }
        eprintln!("timed out after {limit:?} waiting for {what}");
        false
    }

    fn win(&self, id: egui::Id) -> &WindowState {
        self.app().windows.iter().find(|w| w.id == id).expect("the window is open")
    }

    fn win_rect(&self, id: egui::Id) -> egui::Rect {
        let w = self.win(id);
        egui::Rect::from_min_size(w.screen_position, w.size)
    }

    fn tv(&self, id: egui::Id) -> Option<&vike_app_core::ui::windows::WindowView> {
        self.app().window_views.get(&id)
    }

    /// The window's status strip, as the trader reads it.
    fn strip(&self, id: egui::Id) -> String {
        let line = self.tv(id).and_then(|tv| tv.trade_status.as_ref());
        line.map_or_else(String::new, |(_, t)| t.clone())
    }

    /// Every node of `role` (any role for `None`) whose label `want` accepts, inside `within` when
    /// given, top to bottom.
    fn find(
        &self,
        role: Option<Role>,
        within: Option<egui::Rect>,
        want: impl Fn(&str) -> bool + 'static,
    ) -> Vec<egui_kittest::Node<'_>> {
        let by = By::new().predicate(move |n| {
            role.is_none_or(|r| n.role() == r) && n.label().is_some_and(|l| want(&l))
        });
        let mut found: Vec<_> = self
            .h
            .query_all(by)
            .filter(|n| within.is_none_or(|r| r.contains(n.rect().center())))
            .collect();
        found.sort_by(|a, b| a.rect().top().total_cmp(&b.rect().top()));
        found
    }

    /// Click the TOPMOST control `want` accepts (the LOWEST with `lowest`), which must be enabled.
    fn click_at(
        &mut self,
        role: Option<Role>,
        within: Option<egui::Rect>,
        lowest: bool,
        what: &str,
        want: impl Fn(&str) -> bool + 'static,
    ) {
        {
            let found = self.find(role, within, want);
            let node = if lowest { found.last() } else { found.first() };
            let node = node.unwrap_or_else(|| panic!("no {what} on screen"));
            assert!(!node.accesskit_node().is_disabled(), "{what} is disabled");
            node.click();
        }
        self.frames(4);
    }

    fn click(
        &mut self,
        within: Option<egui::Rect>,
        what: &str,
        want: impl Fn(&str) -> bool + 'static,
    ) {
        self.click_at(Some(Role::Button), within, false, what, want);
    }

    /// The window's text field number `slot`, top to bottom then left to right: the ticket's Price (a
    /// Limit or Stop), Size, TP and SL, in the order it draws them. A field is found by PLACE and not
    /// by placeholder, which egui files only while the field is EMPTY (measured: the Size holds a
    /// default, and a Price the Bid button typed or the trader just typed has a value too).
    fn field(&self, within: egui::Rect, slot: usize, what: &str) -> egui_kittest::Node<'_> {
        let by = By::new().role(Role::TextInput);
        let mut all: Vec<_> =
            self.h.query_all(by).filter(|n| within.contains(n.rect().center())).collect();
        let (top, left) = (
            |n: &egui_kittest::Node<'_>| n.rect().top(),
            |n: &egui_kittest::Node<'_>| n.rect().left(),
        );
        all.sort_by(|a, b| top(a).total_cmp(&top(b)).then(left(a).total_cmp(&left(b))));
        let found = all.into_iter().nth(slot);
        found.unwrap_or_else(|| {
            panic!("no {what} field (slot {slot}) in {within:?}: {}", self.ui_dump())
        })
    }

    /// Every text field and button on screen with its rect, for a failure: what a screen reader (and
    /// this harness) can reach, which is not always what the window's own rect says.
    fn ui_dump(&self) -> String {
        let fields = self.h.query_all(By::new().role(Role::TextInput));
        let fields: Vec<String> = fields
            .map(|n| {
                let a = n.accesskit_node();
                format!("{:?} = {:?} at {:?}", a.placeholder(), n.value(), n.rect())
            })
            .collect();
        let buttons = self.h.query_all(By::new().role(Role::Button));
        let buttons: Vec<String> = buttons
            .map(|n| {
                format!(
                    "{:?}{} at {:?}",
                    n.accesskit_node().label(),
                    if n.accesskit_node().is_disabled() { " (disabled)" } else { "" },
                    n.rect()
                )
            })
            .collect();
        format!("text fields {fields:?}; buttons {buttons:?}")
    }

    /// Type `text` into the window's field number `slot` ([`Desk::field`]), over what it held: focus,
    /// select all, type, as a trader's keyboard does.
    fn type_into(&mut self, id: egui::Id, slot: usize, what: &str, text: &str) {
        let within = self.win_rect(id);
        self.field(within, slot, what).focus();
        self.frames(2);
        self.h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        self.frames(1);
        self.field(within, slot, what).type_text(text);
        self.frames(3);
        let held = self.field(within, slot, what).value().unwrap_or_default();
        assert_eq!(held, text, "the {what} field holds what was typed");
    }

    /// The last 1m bar (within ten hours) the datahub's STORE holds for `symbol` on `venue`, as
    /// `(close, age)`, read through the desktop's own dial into the store. The datahub serves no
    /// live feed for ctrader, ig or dukascopy (it mounts the crypto venues only), so a trader of the
    /// thin client has no quote to type a price from there: the stored close is the nearest thing,
    /// good for a limit 4 percent away from the market and for nothing nearer.
    fn stored_price(&self, venue: &str, symbol: &str) -> Option<(f64, Duration)> {
        use vike_data::HistoryStore;
        let app = self.app();
        let store = super::open_history_store_datahub(app.resolved_datahub_address()?);
        let now = vike_model::now_ms();
        let range = vike_app_core::data::stored::read_range("1m", now, 600)?;
        let last = store.load_bars(venue, symbol, "1m", range).ok()?.pop()?;
        Some((last.close, Duration::from_millis(u64::try_from(now - last.timestamp).unwrap_or(0))))
    }

    /// Every market-data feed the datahub's handshake mounted, with its status line: a venue the
    /// datahub does not advertise has none, and its Trade window shows `NO DEPTH FEED`.
    fn feeds(&self) -> String {
        let status = |h: &Arc<std::sync::Mutex<vike_model::feed_status::DataClientStatus>>| {
            h.lock().map(|s| s.clone()).unwrap_or_default()
        };
        let mut lines: Vec<String> =
            self.app().feed_statuses.iter().map(|(v, h)| format!("{v}: {:?}", status(h))).collect();
        lines.sort();
        format!("{lines:?}")
    }

    /// What the desktop shows, for a failure: its windows, the Trade windows' strips, the snapshot's
    /// primary market and the status line.
    fn dump(&self) -> String {
        let snapshot = self.snapshot();
        let wins: Vec<String> = self
            .app()
            .windows
            .iter()
            .map(|w| {
                let strip =
                    if w.kind == WindowKind::Trade { self.strip(w.id) } else { String::new() };
                let (open, min, max) = (w.open, w.minimized, w.maximized);
                format!(
                    "{:?} {} {} open {open} min {min} max {max} `{strip}`",
                    w.kind, w.venue, w.symbol
                )
            })
            .collect();
        format!(
            "windows {wins:?}; primary `{}` `{}`; status `{:?}`",
            snapshot.venue,
            snapshot.symbol,
            self.app().status
        )
    }

    fn trade_ids(&self) -> HashSet<egui::Id> {
        let wins = self.app().windows.iter();
        wins.filter(|w| w.kind == WindowKind::Trade && w.open).map(|w| w.id).collect()
    }

    /// Open a Trade window from the launcher and move it to `symbol` on `venue`'s DEFAULT account
    /// through its own symbol picker; maximized, so the whole ticket is on screen.
    fn open_trade(&mut self, venue: &str, symbol: &str) -> egui::Id {
        let before = self.trade_ids();
        // A maximized window HIDES every other one (a lone window opens maximized), so a new
        // window would never draw: restore it first, with its own control.
        let maxed = self.app().windows.iter().find(|w| w.open && !w.minimized && w.maximized);
        if let Some(rect) = maxed.map(|w| egui::Rect::from_min_size(w.screen_position, w.size)) {
            self.click(Some(rect), "the maximized window's restore", |l| l == "Maximize / restore");
        }
        // The launcher row's Trade icon, named for a screen reader by its own name.
        self.click_at(None, None, false, "the `trade` launcher", |l| l == "trade");
        let opened = self.wait("a Trade window", DATA_WAIT, |d| {
            d.trade_ids().difference(&before).any(|id| !d.win(*id).symbol.is_empty())
        });
        assert!(
            opened,
            "the launcher opened no Trade window that found an instrument: {}",
            self.dump()
        );
        let id = *self.trade_ids().difference(&before).next().expect("the new window");
        let rect = self.win_rect(id);
        self.click(Some(rect), "the window's maximize control", |l| l == "Maximize / restore");
        let rect = self.win_rect(id);
        let (at_venue, at_symbol) = (self.win(id).venue.clone(), self.win(id).symbol.clone());
        if (at_venue.as_str(), at_symbol.as_str()) != (venue, symbol) {
            // The instrument bar's symbol button names the window's symbol; its picker searches.
            self.click(Some(rect), "the symbol picker", move |l| l.starts_with(&at_symbol));
            let search = "Search a symbol or a coin";
            let by = |h: &'static str| {
                By::new().role(Role::TextInput).predicate(move |n| n.placeholder() == Some(h))
            };
            self.h.query_all(by(search)).next().expect("the picker's search field").focus();
            self.frames(2);
            self.h
                .query_all(by(search))
                .next()
                .expect("the picker's search field")
                .type_text(symbol);
            self.frames(6);
            let lead = format!("{symbol}, ");
            let rows: Vec<String> = self
                .find(Some(Role::Button), None, move |l| l.starts_with(&lead))
                .iter()
                .filter_map(|n| n.accesskit_node().label())
                .collect();
            eprintln!("  symbol picker rows for {symbol}: {rows:?}");
            let tail = format!(", {venue}");
            let row = rows.iter().find(|l| l.to_lowercase().ends_with(&tail)).cloned();
            let row = row.unwrap_or_else(|| {
                panic!(
                    "the symbol picker lists no {symbol} on {venue}: the desktop has no path to it"
                )
            });
            self.click(None, "the picker row", move |l| l == row);
        }
        let there = self.wait("the window on the picked instrument", DATA_WAIT, |d| {
            let w = d.win(id);
            w.venue == venue && w.symbol == symbol
        });
        assert!(there, "the window did not move to {venue} {symbol}");
        let account = self.tv(id).and_then(|tv| tv.trade_account.clone());
        assert_eq!(account, None, "the window addresses {venue}'s DEFAULT account");
        id
    }

    /// The window's book's best bid, after its depth stream lands.
    fn best_bid(&mut self, venue: &str, symbol: &str) -> f64 {
        let has_bid = |d: &Desk| {
            d.app().books.get(venue, symbol).is_some_and(|(b, _)| b.best_bid().is_some())
        };
        let got = self.wait("a book", DATA_WAIT, has_bid);
        let feed = self
            .app()
            .feed_statuses
            .get(venue)
            .map(|h| h.lock().map(|s| s.clone()).unwrap_or_default());
        assert!(got, "no depth for {venue} {symbol} reached the desktop (feed: {feed:?})");
        let (book, _) = self.app().books.get(venue, symbol).expect("the book");
        book.best_bid().expect("a bid").price
    }

    /// A price near the market to hang a far order from: the window's own book where the datahub
    /// mounts a feed for the venue (the crypto venues), else [`ecb_eurusd`], because the thin client
    /// has no quote for ctrader, ig or dukascopy (`desktop_reads_the_live_node` shows it) and its
    /// trader types a price read from elsewhere.
    fn reference(&mut self, venue: &str, symbol: &str) -> f64 {
        if self.app().feed_statuses.contains_key(venue) {
            return self.best_bid(venue, symbol);
        }
        assert!(symbol.contains("EURUSD"), "no reference price for {venue} {symbol}");
        let rate = ecb_eurusd();
        eprintln!(
            "  reference {venue} {symbol}: {rate} (the ECB rate: the datahub serves no {venue} feed)"
        );
        rate
    }

    /// The ticket typed as a trader types it: the order type, a price (`None`: market), the size,
    /// TP/SL off or at `tpsl` percent both ways; then Buy, and Place where the order is held. `Err`
    /// carries the window's own reason where it will not send.
    fn buy(
        &mut self,
        id: egui::Id,
        price: Option<f64>,
        size: &str,
        take_profit_stop_loss: Option<&str>,
    ) -> Result<(), String> {
        let rect = self.win_rect(id);
        let kind = if price.is_some() { "Limit" } else { "Market" };
        self.click(Some(rect), kind, move |l| l == kind);
        if let Some(p) = price {
            // Bid types the book's best on the tick grid, and the far price keeps its decimals; a
            // venue with no book has no Bid to click, and its trader types the price whole (FX
            // quotes to five places).
            let bid = self.find(Some(Role::Button), Some(rect), |l| l == "Bid");
            let on = bid.first().is_some_and(|n| !n.accesskit_node().is_disabled());
            drop(bid);
            let mut decimals = 5;
            if on {
                self.click(Some(rect), "Bid", |l| l == "Bid");
                let typed = self.tv(id).map(|tv| tv.trade.price.clone()).unwrap_or_default();
                decimals = typed.split('.').nth(1).map_or(0, str::len);
            }
            self.type_into(id, 0, "Price", &format!("{p:.decimals$}"));
        }
        // A Limit's Price field comes first; the Size, then TP and SL (once TP/SL is on), follow.
        let size_at = usize::from(price.is_some());
        self.type_into(id, size_at, "Size", size);
        let toggle = self.find(Some(Role::Button), Some(rect), |l| l == "TP/SL");
        let Some(node) = toggle.first() else { return Err("no TP/SL toggle".to_string()) };
        let (on, off) = (
            node.accesskit_node().toggled() == Some(Toggled::True),
            node.accesskit_node().is_disabled(),
        );
        drop(toggle);
        if on != take_profit_stop_loss.is_some() {
            if off {
                return Err(format!("TP/SL is disabled here; strip: {}", self.strip(id)));
            }
            self.click(Some(rect), "TP/SL", |l| l == "TP/SL");
        }
        if let Some(pct) = take_profit_stop_loss {
            self.type_into(id, size_at + 1, "TP", pct);
            self.type_into(id, size_at + 2, "SL", pct);
        }
        let word = if price.is_some() { "limit" } else { "market" };
        let buys = self.find(Some(Role::Button), Some(rect), move |l| {
            l.starts_with("Buy ") && l.ends_with(word)
        });
        let Some(node) = buys.first() else {
            return Err(format!("no Buy {word} button; strip: {}", self.strip(id)));
        };
        let label = node.accesskit_node().label().unwrap_or_default();
        if node.accesskit_node().is_disabled() {
            // The ticket's reason is a tooltip kittest cannot read: say what it was fed instead.
            let state = self.tv(id).map(|tv| {
                let t = &tv.trade;
                format!(
                    "{:?} size {:?} price {:?} tpsl {} tp {:?} sl {:?}",
                    t.order_type,
                    t.size,
                    t.price,
                    t.take_profit_stop_loss,
                    t.take_profit_text,
                    t.stop_loss_text
                )
            });
            return Err(format!(
                "`{label}` is disabled; ticket {state:?}; strip: {}",
                self.strip(id)
            ));
        }
        // ⚠ The size the button NAMES is the size it sends: nothing else may go out.
        assert!(
            label.replace(',', "").contains(&format!(" {size} ")),
            "`{label}` does not send {size}"
        );
        eprintln!("  click `{label}`");
        node.click();
        drop(buys);
        self.frames(4);
        if self.tv(id).is_some_and(|tv| tv.trade.held.is_some()) {
            self.click(Some(rect), "Place", |l| l == "Place");
        }
        Ok(())
    }

    /// The orders on `venue` the snapshot did not have in `before`, in the snapshot's order.
    fn new_orders(&self, before: &HashSet<String>, venue: &str) -> Vec<OrderView> {
        let snapshot = self.snapshot();
        let new = snapshot
            .orders
            .iter()
            .filter(|o| o.venue == venue && !before.contains(&o.order_client_id));
        new.map(|o| OrderView::clone(o)).collect()
    }

    fn order_client_ids(&self) -> HashSet<String> {
        self.snapshot().orders.iter().map(|o| o.order_client_id.clone()).collect()
    }

    /// Frames until `n` new orders on `venue` exist and `settled` holds for them; returns them.
    fn await_orders(
        &mut self,
        before: &HashSet<String>,
        venue: &str,
        n: usize,
        settled: impl Fn(&[OrderView]) -> bool,
    ) -> Vec<OrderView> {
        self.wait("the node's answer", ORDER_WAIT, |d| {
            let new = d.new_orders(before, venue);
            new.len() >= n && settled(&new)
        });
        self.new_orders(before, venue)
    }

    /// A command through this desktop's own control link (the door), and the node's answer.
    fn door(&mut self, cmd: CommandWire) -> String {
        let ctrl = self.app().tradehub_control().expect("the control link");
        let ticket = ctrl.try_command_with_reason(cmd, Some("desktop live smoke".to_string()));
        let out = ticket.map(|t| ctrl.await_outcome(t, Duration::from_secs(15)));
        self.frames(2);
        format!("{out:?}")
    }

    /// Cancel `order_client_id` through the door, and wait for the snapshot to call it terminal.
    fn cancel(&mut self, order_client_id: &str) -> OrderStatus {
        eprintln!(
            "  cancel {order_client_id}: {}",
            self.door(CommandWire::Cancel(order_client_id.to_string()))
        );
        self.wait("the cancel", ORDER_WAIT, |d| d.status_of(order_client_id).is_terminal());
        self.placed.retain(|p| p != order_client_id);
        self.status_of(order_client_id)
    }

    fn status_of(&self, order_client_id: &str) -> OrderStatus {
        let snapshot = self.snapshot();
        let row = snapshot.orders.iter().find(|o| o.order_client_id == order_client_id);
        row.map_or(OrderStatus::Initialized, |o| o.status)
    }

    fn position(&self, venue: &str, symbol: &str) -> f64 {
        let snapshot = self.snapshot();
        demo_block(&snapshot, venue)
            .positions
            .iter()
            .filter(|p| p.symbol == symbol)
            .map(|p| p.size)
            .sum()
    }

    /// Wait until every staged row is in the running desktop's catalog with its lot.
    /// Hand `rows` to this desktop's catalog as kept lists (`DatahubCatalogRefresh::adopt`), after the launch that trades them.
    fn stage(&self, rows: &[Staged]) {
        self.app().catalog.adopt(staged_lists(rows));
    }

    fn await_staged(&mut self, rows: &[Staged]) {
        for (venue, symbol, _, _, min, _) in rows {
            let listed = |d: &Desk| {
                let filter =
                    vike_catalog::CatalogSearchFilter { tab: None, venue: Some(venue.clone()) };
                let cat = &d.app().symbols_catalog;
                cat.search("", &filter, usize::MAX)
                    .iter()
                    .any(|i| i.raw_symbol == *symbol && (i.properties.step_size - min).abs() < 1e-9)
            };
            assert!(
                self.wait("the staged catalog row", DATA_WAIT, listed),
                "{venue} {symbol} never listed"
            );
        }
    }

    /// Close the desktop: what it placed put back, then the shell's own bounded teardown.
    fn shut(mut self) {
        self.cleanup();
        if let Some(mut app) = self.h.state_mut().app.take() {
            app.run_bounded_teardown();
        }
    }

    /// What this desk placed and opened, put back: cancels by id, flattens through the door.
    fn cleanup(&mut self) {
        if self.h.state().app.is_none() {
            return;
        }
        for order_client_id in std::mem::take(&mut self.placed) {
            if !self.status_of(&order_client_id).is_terminal() {
                eprintln!(
                    "CLEANUP cancel {order_client_id}: {}",
                    self.door(CommandWire::Cancel(order_client_id.clone()))
                );
            }
        }
        for (venue, symbol) in std::mem::take(&mut self.opened) {
            if self.position(&venue, &symbol).abs() > 0.0 {
                let cmd = CommandWire::Flatten {
                    venue: venue.clone(),
                    symbol: symbol.clone(),
                    account: None,
                };
                eprintln!("CLEANUP flatten {venue} {symbol}: {}", self.door(cmd));
            }
        }
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// The venue's DEFAULT-account engine, REFUSED unless it is DEMO and `Active`.
fn demo_block<'s>(snapshot: &'s CoreSnapshot, venue: &str) -> &'s VenueBlock {
    let b = snapshot.portfolio.venues.iter().find(|b| b.venue == venue && b.account.is_none());
    let b =
        b.unwrap_or_else(|| panic!("REFUSED: the node runs no default-account engine for {venue}"));
    assert_eq!(b.tier, Some(AccountTier::Demo), "REFUSED: {venue}'s default account is not DEMO");
    assert_eq!(b.trading_state, TradingState::Active, "REFUSED: {venue}'s engine is not Active");
    b
}

/// REFUSED (before) or FAILED (after) unless `venue` holds no position and nothing rests.
fn assert_flat(snapshot: &CoreSnapshot, venue: &str, when: &str) {
    let b = demo_block(snapshot, venue);
    let open: Vec<String> = b
        .positions
        .iter()
        .filter(|p| p.size.abs() > 0.0)
        .map(|p| format!("{} {}", p.symbol, p.size))
        .collect();
    let resting: Vec<String> = snapshot
        .orders
        .iter()
        .filter(|o| o.venue == venue && !o.status.is_terminal())
        .map(|o| format!("{} {:?}", o.order_client_id, o.status))
        .collect();
    let held: Vec<&str> = snapshot
        .held_exits
        .iter()
        .filter(|h| h.venue == venue)
        .map(|h| h.order_client_id.as_str())
        .collect();
    assert!(
        open.is_empty() && resting.is_empty() && held.is_empty(),
        "{when}: {venue} is not flat: positions {open:?}, resting {resting:?}, held exits {held:?}"
    );
}

/// The symbols the ig, dukascopy and ctrader DEMO engines were started on (a bracket goes to the
/// symbol an engine was started on), read by a first launch that is shut again, so the rows can be
/// staged before the launch that trades them. `None` (SKIP) where that launch skips.
fn engine_symbols(boot: &Boot) -> Option<(String, String, String)> {
    let probe = Desk::launch(boot)?;
    let snapshot = probe.snapshot();
    let mounted = |venue: &str| demo_block(&snapshot, venue).symbol.clone();
    let symbols = (mounted("ig"), mounted("dukascopy"), mounted("ctrader"));
    probe.shut();
    // A catalog refresh the probe started may still land: let it, then merge over it.
    std::thread::sleep(Duration::from_secs(5));
    Some(symbols)
}
