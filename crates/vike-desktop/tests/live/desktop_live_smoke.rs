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
//!   empty cell only), so the layouts, `catalog.json` and `last_layout.txt` this run writes never land
//!   in the project's `settings/state`;
//! - no `vike_log::init`, no `journal_boot_settings` anchor and no user indicators (each writes, or
//!   is not about orders);
//! - the order tests write the instruments they trade into that scratch `catalog.json`
//!   ([`stage_catalog`]): the desktop sizes an order by its catalog's lot, and the datahub lists none
//!   for ctrader (a credentialed venue), ig (no bulk list) or dukascopy's bundled pairs (a pip, no
//!   lot). The staged row carries the venue's minimum as its lot; the bracket test and the read-only
//!   pass add a five-place FX tick (a TP/SL ticket offers no exits without a tick grid, and there is
//!   no book to take one from: see below), the id test stages none.
//!
//! # The thin client has no price for these venues
//!
//! The datahub mounts market data for the crypto venues only (the handshake's `md_venue=` rows), so
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
//! - `desktop_order_ids_are_session_unique_on_the_live_node`: a far LIMIT BUY 1000 (4 percent under
//!   the reference price, never marketable) on the cTrader demo default account's EURUSD, three times: in a first
//!   launch, in a second launch with the same workspace, and after that launch loads a layout with
//!   FEWER windows. Each id matches `^(ui|dom|poly|opt)[0-9a-f]{8}[0-9]+$` and is ACCEPTED with no
//!   `already used` refusal; the second session differs from the first; the third keeps the second's
//!   session with a larger sequence. Each order is cancelled through the desktop's own control link
//!   (`WireCommand::Cancel`, by its id) as soon as it is read back, so at most one rests at a time.
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
//!   desktop_live_smoke::desktop_order_ids_are_session_unique_on_the_live_node
//! ```
//!
//! # What it does NOT prove
//!
//! Pixels and a real pointer: kittest steps `Context::run_ui` on the CPU and clicks through the
//! accessibility tree, so a widget that draws wrong, a click a real mouse would miss, focus taken by
//! the OS, the GPU renderer and eframe's window loop are all outside it. A "restart" is a second `App`
//! in the same process (the session is drawn per `App`, as per process).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::accesskit::{Role, Toggled};
use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
use vike_app_core::ui::workspace::{WinKind, WinState};
use vike_exec::{CoreSnapshot, OrderView, VenueBlock};
use vike_model::{AccountTier, OrderStatus, OrderType, TradingState};
use vike_tradehub_client::wire::WireCommand;

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
/// The coid test's venue and symbol: the cTrader demo default account, as the kit's M1 names it.
const COID_AT: (&str, &str) = ("ctrader", "EURUSD");
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
    state: PathBuf,
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
        vike_app_core::ui::workspace::persist::declare_project_state_dir(Some(state.clone()))
            .expect("the scratch workspace family");
        let ws = vike_app_core::ui::workspace::persist::path().expect("a workspace path");
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
        Some(Boot { node, state, _scratch: scratch })
    })
    .as_ref()
}

/// Three chart windows as the workspace both launches start from, and a named layout of ONE: the
/// two shapes that made the old window counter repeat an id.
fn stage_layouts() {
    use vike_app_core::ui::workspace::persist::{save, save_layout};
    let chart = |i: u8, interval: &str| {
        let at = egui::pos2(60.0 + 40.0 * f32::from(i), 80.0);
        let r = egui::Rect::from_min_size(at, egui::vec2(480.0, 320.0));
        WinState::new(&format!("smoke-chart-{i}"), "BTCUSDT", interval, WinKind::Chart, r)
    };
    let three = [chart(0, "1m"), chart(1, "5m"), chart(2, "15m")];
    let tz = vike_chart::DisplayTz::Utc;
    save(&three, tz, false, &[]).expect("the scratch workspace.json");
    save_layout("fewer", &three[..1], tz, false, &[]).expect("the layout");
    // ...and "three" LAST: a save records the last-active layout, which a launch restores first.
    save_layout("three", &three, tz, false, &[]).expect("the layout");
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
/// on the tick grid and offers none without one (`vike-panels`' `sizing::exits`), and with no book
/// there is no tick but the catalog's.
const FX_TICK: f64 = 0.00001;

/// The staged row of `symbol` on `venue`: a six-letter pair, or an ig epic (`CS.D.EURGBP.MINI.IP`)
/// whose third field is one.
fn staged(venue: &str, symbol: &str, min: f64, tick: f64) -> Staged {
    let pair = symbol.split('.').nth(2).filter(|p| p.len() == 6).unwrap_or(symbol);
    let (base, quote) = pair.split_at(3.min(pair.len()));
    (venue.to_string(), symbol.to_string(), base.to_string(), quote.to_string(), min, tick)
}

/// Merge `rows` into the scratch `catalog.json`, stamped so the desktop does not fetch those venues
/// over them, BEFORE the launch that trades them. Printed, because it is the one thing here a
/// trader's box would get from elsewhere (`vike-backend catalog refresh`, a venue's own list).
fn stage_catalog(boot: &Boot, rows: &[Staged]) {
    let path = boot.state.join("catalog.json");
    let mut cache = vike_catalog::load_cache(&path).unwrap_or_default();
    for (venue, symbol, base, quote, min, tick) in rows {
        eprintln!("STAGED catalog row: {venue} {symbol} lot {min} min {min} tick {tick}");
        let props = vike_model::SymbolProperties {
            step_size: *min,
            min_qty: *min,
            tick_size: *tick,
            ..Default::default()
        };
        let fx = vike_model::Contract::Fx(vike_model::FxSpec::default());
        cache.instruments.retain(|i| !(i.venue == *venue && i.raw_symbol == *symbol));
        cache.instruments.push(
            vike_model::Instrument::new(venue, symbol, base, quote, fx).with_properties(props),
        );
        let count = cache.instruments.iter().filter(|i| i.venue == *venue).count();
        cache.fetched.retain(|s| s.venue != *venue);
        let last_refreshed_ms = vike_model::now_ms();
        cache.fetched.push(vike_catalog::VenueStamp {
            venue: venue.clone(),
            last_refreshed_ms,
            count,
        });
    }
    vike_catalog::save_cache(&path, &cache).expect("the scratch catalog.json");
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
        let startup = vike_app_core::backend::backend_conn::startup_observe(argv);
        let backend = vike_app_core::backend::split_plane::observes(false, startup.requested)
            .then_some(startup.record);
        let app = App::new(&h.ctx, None, backend, gui_env());
        assert!(app.env.shot.is_none(), "VIKE_SHOT keeps the deterministic `ui-3-0` ids: unset it");
        let mut d = Desk { h, placed: Vec::new(), opened: Vec::new() };
        d.h.state_mut().app = Some(app);
        if d.app().remote_ctrl().is_none() {
            eprintln!("SKIP: no control link: the hub-key store names no tradehub control key");
            return None;
        }
        let up = d.wait("the node's first snapshot and the control link", NODE_WAIT, |d| {
            let a = d.app();
            !a.snap_cell.load().portfolio.venues.is_empty()
                && a.remote_ctrl().is_some_and(|c| c.is_connected())
        });
        assert!(up, "the desktop never connected to {} (status: {})", boot.node, d.app().status);
        Some(d)
    }

    fn app(&self) -> &App {
        self.h.state().app.as_ref().expect("launched")
    }

    fn snap(&self) -> Arc<CoreSnapshot> {
        self.app().snap_cell.load_full()
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

    fn win(&self, id: egui::Id) -> &WinState {
        self.app().wins.iter().find(|w| w.id == id).expect("the window is open")
    }

    fn win_rect(&self, id: egui::Id) -> egui::Rect {
        let w = self.win(id);
        egui::Rect::from_min_size(w.pos, w.size)
    }

    fn tv(&self, id: egui::Id) -> Option<&vike_app_core::tools::ToolView> {
        self.app().tool_views.get(&id)
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
        use vike_data::HistStore;
        let app = self.app();
        let store =
            super::remote_hist_store(app.resolved_datahub_addr()?, &app.datahub_observe_key_name());
        let now = vike_model::now_ms();
        let range = vike_app_core::data::store_bars::read_range("1m", now, 600)?;
        let last = store.load_bars(venue, symbol, "1m", range).ok()?.pop()?;
        Some((last.close, Duration::from_millis(u64::try_from(now - last.ts).unwrap_or(0))))
    }

    /// Every market-data feed the datahub's handshake mounted, with its status line: a venue the
    /// datahub does not advertise has none, and its Trade window shows `NO DEPTH FEED`.
    fn feeds(&self) -> String {
        let status =
            |h: &Arc<std::sync::Mutex<String>>| h.lock().map(|s| s.clone()).unwrap_or_default();
        let mut lines: Vec<String> =
            self.app().feed_statuses.iter().map(|(v, h)| format!("{v}: {:?}", status(h))).collect();
        lines.sort();
        format!("{lines:?}")
    }

    /// What the desktop shows, for a failure: its windows, the Trade windows' strips, the snapshot's
    /// primary market and the status line.
    fn dump(&self) -> String {
        let snap = self.snap();
        let wins: Vec<String> = self
            .app()
            .wins
            .iter()
            .map(|w| {
                let strip = if w.kind == WinKind::Trade { self.strip(w.id) } else { String::new() };
                let (open, min, max) = (w.open, w.minimized, w.maximized);
                format!(
                    "{:?} {} {} open {open} min {min} max {max} `{strip}`",
                    w.kind, w.venue, w.symbol
                )
            })
            .collect();
        format!(
            "windows {wins:?}; primary `{}` `{}`; status `{}`",
            snap.venue,
            snap.symbol,
            self.app().status
        )
    }

    fn trade_ids(&self) -> HashSet<egui::Id> {
        let wins = self.app().wins.iter();
        wins.filter(|w| w.kind == WinKind::Trade && w.open).map(|w| w.id).collect()
    }

    /// Open a Trade window from the launcher and move it to `symbol` on `venue`'s DEFAULT account
    /// through its own symbol picker; maximized, so the whole ticket is on screen.
    fn open_trade(&mut self, venue: &str, symbol: &str) -> egui::Id {
        let before = self.trade_ids();
        // A maximized window HIDES every other one (a lone window opens maximized), so a new
        // window would never draw: restore it first, with its own control.
        let maxed = self.app().wins.iter().find(|w| w.open && !w.minimized && w.maximized);
        if let Some(rect) = maxed.map(|w| egui::Rect::from_min_size(w.pos, w.size)) {
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
        tpsl: Option<&str>,
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
        if on != tpsl.is_some() {
            if off {
                return Err(format!("TP/SL is disabled here; strip: {}", self.strip(id)));
            }
            self.click(Some(rect), "TP/SL", |l| l == "TP/SL");
        }
        if let Some(pct) = tpsl {
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
                    t.order_type, t.size, t.price, t.tpsl, t.tp_text, t.sl_text
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
        let snap = self.snap();
        let new =
            snap.orders.iter().filter(|o| o.venue == venue && !before.contains(&o.client_order_id));
        new.cloned().collect()
    }

    fn coids(&self) -> HashSet<String> {
        self.snap().orders.iter().map(|o| o.client_order_id.clone()).collect()
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
    fn door(&mut self, cmd: WireCommand) -> String {
        let ctrl = self.app().remote_ctrl().expect("the control link");
        let ticket = ctrl.try_command_with_reason(cmd, Some("desktop live smoke".to_string()));
        let out = ticket.map(|t| ctrl.await_outcome(t, Duration::from_secs(15)));
        self.frames(2);
        format!("{out:?}")
    }

    /// Cancel `coid` through the door, and wait for the snapshot to call it terminal.
    fn cancel(&mut self, coid: &str) -> OrderStatus {
        eprintln!("  cancel {coid}: {}", self.door(WireCommand::Cancel(coid.to_string())));
        self.wait("the cancel", ORDER_WAIT, |d| d.status_of(coid).is_terminal());
        self.placed.retain(|p| p != coid);
        self.status_of(coid)
    }

    fn status_of(&self, coid: &str) -> OrderStatus {
        let snap = self.snap();
        let row = snap.orders.iter().find(|o| o.client_order_id == coid);
        row.map_or(OrderStatus::Initialized, |o| o.status)
    }

    fn position(&self, venue: &str, symbol: &str) -> f64 {
        let snap = self.snap();
        demo_block(&snap, venue)
            .positions
            .iter()
            .filter(|p| p.symbol == symbol)
            .map(|p| p.size)
            .sum()
    }

    /// Wait until every staged row is in the running desktop's catalog with its lot.
    fn await_staged(&mut self, rows: &[Staged]) {
        for (venue, symbol, _, _, min, _) in rows {
            let listed = |d: &Desk| {
                let filter = vike_catalog::SearchFilter { tab: None, venue: Some(venue.clone()) };
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
        for coid in std::mem::take(&mut self.placed) {
            if !self.status_of(&coid).is_terminal() {
                eprintln!(
                    "CLEANUP cancel {coid}: {}",
                    self.door(WireCommand::Cancel(coid.clone()))
                );
            }
        }
        for (venue, symbol) in std::mem::take(&mut self.opened) {
            if self.position(&venue, &symbol).abs() > 0.0 {
                let cmd = WireCommand::Flatten {
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
fn demo_block<'s>(snap: &'s CoreSnapshot, venue: &str) -> &'s VenueBlock {
    let b = snap.portfolio.venues.iter().find(|b| b.venue == venue && b.account.is_none());
    let b =
        b.unwrap_or_else(|| panic!("REFUSED: the node runs no default-account engine for {venue}"));
    assert_eq!(b.tier, Some(AccountTier::Demo), "REFUSED: {venue}'s default account is not DEMO");
    assert_eq!(b.trading_state, TradingState::Active, "REFUSED: {venue}'s engine is not Active");
    b
}

/// REFUSED (before) or FAILED (after) unless `venue` holds no position and nothing rests.
fn assert_flat(snap: &CoreSnapshot, venue: &str, when: &str) {
    let b = demo_block(snap, venue);
    let open: Vec<String> = b
        .positions
        .iter()
        .filter(|p| p.size.abs() > 0.0)
        .map(|p| format!("{} {}", p.symbol, p.size))
        .collect();
    let resting: Vec<String> = snap
        .orders
        .iter()
        .filter(|o| o.venue == venue && !o.status.is_terminal())
        .map(|o| format!("{} {:?}", o.client_order_id, o.status))
        .collect();
    let held: Vec<&str> = snap
        .held_exits
        .iter()
        .filter(|h| h.venue == venue)
        .map(|h| h.client_order_id.as_str())
        .collect();
    assert!(
        open.is_empty() && resting.is_empty() && held.is_empty(),
        "{when}: {venue} is not flat: positions {open:?}, resting {resting:?}, held exits {held:?}"
    );
}

/// `(prefix, session, sequence)` of an id shaped `^(ui|dom|poly|opt)[0-9a-f]{8}[0-9]+$`.
fn coid_parts(coid: &str) -> Option<(&str, &str, u64)> {
    let prefix = ["poly", "dom", "opt", "ui"].into_iter().find(|p| coid.starts_with(p))?;
    let (session, seq) = coid[prefix.len()..].split_at_checked(8)?;
    let hex = session.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    let digits = !seq.is_empty() && seq.bytes().all(|b| b.is_ascii_digit());
    if hex && digits { Some((prefix, session, seq.parse().ok()?)) } else { None }
}

/// The coid test's far limit through the ticket of a window on [`COID_AT`], read back.
fn place_far_limit(d: &mut Desk, id: egui::Id) -> OrderView {
    let (venue, symbol) = COID_AT;
    let before = d.coids();
    let price = d.reference(venue, symbol) * FAR;
    d.opened.push((venue.to_string(), symbol.to_string()));
    d.buy(id, Some(price), SIZE_FX, None)
        .unwrap_or_else(|why| panic!("the ticket will not send: {why}"));
    let answered = |o: &[OrderView]| {
        o.iter().all(|o| !matches!(o.status, OrderStatus::Initialized | OrderStatus::Submitted))
    };
    let new = d.await_orders(&before, venue, 1, answered);
    let strip = d.strip(id);
    let order = new
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no order reached the node; strip: {strip}"));
    d.placed.push(order.client_order_id.clone());
    assert!(!strip.contains("already used"), "the node refused a repeated id: {strip}");
    eprintln!(
        "  placed {} {:?} side {} {} {} @ {:?} -> {:?} {}",
        order.client_order_id,
        order.order_type,
        order.side,
        order.symbol,
        order.qty,
        order.price,
        order.status,
        order.reason.as_deref().unwrap_or("")
    );
    order
}

/// Load the named layout, which holds `windows` windows, through the File menu, as a trader does.
/// It is read off the window list: the status line the load writes is overwritten by the link's
/// own line within a frame (measured), so it is no witness.
fn load_layout(d: &mut Desk, name: &'static str, windows: usize) {
    d.click(None, "the File menu", |l| l == "File");
    d.click(None, "Load layout", |l| l.starts_with("Load layout"));
    d.click(None, "the layout's row", move |l| l == name);
    let loaded = d.wait("the layout", DATA_WAIT, |d| {
        let wins = &d.app().wins;
        wins.len() == windows && wins.iter().all(|w| w.kind == WinKind::Chart)
    });
    assert!(loaded, "layout '{name}' did not load: {}", d.dump());
}

#[test]
#[ignore = "LIVE: places and cancels far limit orders on the cTrader DEMO account of the node; by hand only"]
fn desktop_order_ids_are_session_unique_on_the_live_node() {
    one_per_process();
    let Some(boot) = boot() else { return };
    let (venue, symbol) = COID_AT;
    let rows = [staged(venue, symbol, 1000.0, 0.0)];
    stage_catalog(boot, &rows);

    // Launch 1, on the three-window workspace.
    let Some(mut d) = Desk::launch(boot) else { return };
    assert_flat(&d.snap(), venue, "REFUSED before any order");
    d.await_staged(&rows);
    let w = d.open_trade(venue, symbol);
    let first = place_far_limit(&mut d, w);
    let first_end = d.cancel(&first.client_order_id);
    d.shut();

    // Launch 2: a restart on the same workspace.
    let Some(mut d) = Desk::launch(boot) else { return };
    d.await_staged(&rows);
    let w = d.open_trade(venue, symbol);
    let second = place_far_limit(&mut d, w);
    let second_end = d.cancel(&second.client_order_id);
    // ...then a layout with FEWER windows, in the same session. The load takes the Trade window
    // with it (a layout holds chart windows only), so the trader opens a new one.
    load_layout(&mut d, "fewer", 1);
    assert_eq!(d.app().wins.len(), 1, "the layout holds one chart window");
    let w = d.open_trade(venue, symbol);
    let third = place_far_limit(&mut d, w);
    let third_end = d.cancel(&third.client_order_id);
    assert_flat(&d.snap(), venue, "AFTER the three orders");
    d.shut();

    let rows = [(&first, first_end), (&second, second_end), (&third, third_end)];
    for (o, end) in &rows {
        eprintln!("RESULT {} -> {:?}, then {end:?}", o.client_order_id, o.status);
    }
    let parts: Vec<(&str, &str, u64)> = rows
        .iter()
        .map(|(o, _)| {
            coid_parts(&o.client_order_id)
                .unwrap_or_else(|| panic!("bad id shape {}", o.client_order_id))
        })
        .collect();
    for (o, end) in &rows {
        assert_eq!(
            o.status,
            OrderStatus::Accepted,
            "{} was not accepted: {:?}",
            o.client_order_id,
            o.reason
        );
        assert_eq!(*end, OrderStatus::Canceled, "{} was not cancelled", o.client_order_id);
        let limit_buy = o.order_type == OrderType::Limit && o.side == 1;
        assert!(limit_buy, "{} is a limit buy", o.client_order_id);
    }
    assert_ne!(parts[0].1, parts[1].1, "a restart draws a new session");
    assert_eq!(parts[1].1, parts[2].1, "a layout load keeps the session");
    assert!(parts[2].2 > parts[1].2, "the sequence grows across a layout load");
    let ids: HashSet<&str> = rows.iter().map(|(o, _)| o.client_order_id.as_str()).collect();
    assert_eq!(ids.len(), 3, "three distinct ids");
}

/// The symbols the ig, dukascopy and ctrader DEMO engines were started on (a bracket goes to the
/// symbol an engine was started on), read by a first launch that is shut again, so the rows can be
/// staged before the launch that trades them. `None` (SKIP) where that launch skips.
fn engine_symbols(boot: &Boot) -> Option<(String, String, String)> {
    let probe = Desk::launch(boot)?;
    let snap = probe.snap();
    let mounted = |venue: &str| demo_block(&snap, venue).symbol.clone();
    let symbols = (mounted("ig"), mounted("dukascopy"), mounted("ctrader"));
    probe.shut();
    // A catalog refresh the probe started may still land: let it, then merge over it.
    std::thread::sleep(Duration::from_secs(5));
    Some(symbols)
}

#[test]
#[ignore = "LIVE: sends TP/SL brackets (far limits) on the ig, dukascopy and cTrader DEMO accounts; by hand only"]
fn desktop_bracket_tickets_on_the_live_node() {
    one_per_process();
    let Some(boot) = boot() else { return };
    let Some((ig, duka, ct)) = engine_symbols(boot) else { return };
    let rows = [
        staged("ig", &ig, 1.0, FX_TICK),
        staged("dukascopy", &duka, 1000.0, FX_TICK),
        staged("ctrader", &ct, 1000.0, FX_TICK),
    ];
    stage_catalog(boot, &rows);
    let Some(mut d) = Desk::launch(boot) else { return };
    for venue in ["ig", "dukascopy", "ctrader"] {
        assert_flat(&d.snap(), venue, "REFUSED before any order");
    }
    d.await_staged(&rows);
    let mut report: Vec<(&str, bool)> = Vec::new();

    // ig and dukascopy: refused WHOLE, nothing sent.
    for (venue, symbol, size, kind) in
        [("ig", &ig, SIZE_IG, "stop"), ("dukascopy", &duka, SIZE_FX, "limit")]
    {
        let w = d.open_trade(venue, symbol);
        let before = d.coids();
        let price = d.reference(venue, symbol) * FAR;
        d.opened.push((venue.to_string(), symbol.to_string()));
        if let Err(why) = d.buy(w, Some(price), size, Some("1")) {
            eprintln!("RESULT {venue} {symbol}: the desktop sent nothing: {why}");
            report.push((venue, false));
            continue;
        }
        let legs = d.await_orders(&before, venue, 3, |o| o.iter().all(|o| o.status.is_terminal()));
        d.placed.extend(legs.iter().map(|o| o.client_order_id.clone()));
        let said =
            |o: &OrderView, words: &str| o.reason.as_deref().is_some_and(|r| r.contains(words));
        let culprit = format!("REDUCE_ONLY_UNSUPPORTED: kind={kind} venue={venue}");
        let whole = format!("bracket for {venue} REFUSED WHOLE");
        let note = d.snap().recent_events.iter().any(|e| e.contains(&whole));
        for o in &legs {
            eprintln!(
                "  leg {} {:?} side {} {:?}: {}",
                o.client_order_id,
                o.order_type,
                o.side,
                o.status,
                o.reason.as_deref().unwrap_or("")
            );
        }
        eprintln!(
            "RESULT {venue} {symbol}: {} legs; `{whole}` in the recent events: {note}; strip `{}`",
            legs.len(),
            d.strip(w)
        );
        let ok = legs.len() == 3
            && legs.iter().all(|o| o.status == OrderStatus::Rejected)
            && legs.iter().filter(|o| said(o, &culprit)).count() == 1
            && legs.iter().filter(|o| said(o, "BRACKET_ATOMIC_REFUSED")).count() == 2;
        report.push((venue, ok));
        d.cleanup();
        assert_flat(&d.snap(), venue, "AFTER the bracket");
    }

    // cTrader: the SAME ticket is accepted whole. The entry is a far limit (the thin client has no
    // price to send a market order from), so it rests, its two exits are held for it, and
    // cancelling the entry takes them with it.
    let w = d.open_trade("ctrader", &ct);
    let before = d.coids();
    let price = d.reference("ctrader", &ct) * FAR;
    d.opened.push(("ctrader".to_string(), ct.clone()));
    let sent = d.buy(w, Some(price), SIZE_FX, Some("1"));
    let held = |d: &Desk| d.snap().held_exits.iter().filter(|h| h.venue == "ctrader").count();
    let rests = |d: &Desk| {
        let new = d.new_orders(&before, "ctrader");
        new.iter().any(|o| o.status == OrderStatus::Accepted) && held(d) == 2
    };
    if sent.is_ok() {
        d.wait("the entry to rest and its two exits to be held", ORDER_WAIT, rests);
    }
    let legs = d.new_orders(&before, "ctrader");
    d.placed.extend(legs.iter().map(|o| o.client_order_id.clone()));
    for o in &legs {
        eprintln!(
            "  leg {} {:?} side {} {:?}: {}",
            o.client_order_id,
            o.order_type,
            o.side,
            o.status,
            o.reason.as_deref().unwrap_or("")
        );
    }
    for h in d.snap().held_exits.iter().filter(|h| h.venue == "ctrader") {
        eprintln!(
            "  held exit {} {:?} side {} price {:?} trigger {:?} for {:?}",
            h.client_order_id, h.order_type, h.side, h.price, h.trigger_price, h.parent_order_id
        );
    }
    let accepted = sent.is_ok() && legs.len() == 1 && rests(&d);
    eprintln!(
        "RESULT ctrader {ct} bracket: sent {sent:?}; {} orders, {} held exits; strip `{}`",
        legs.len(),
        held(&d),
        d.strip(w)
    );
    let parent = legs.iter().find(|o| o.order_type == OrderType::Limit && o.side == 1);
    let cancelled = parent.is_some_and(|p| d.cancel(&p.client_order_id) == OrderStatus::Canceled);
    let gone = d.wait("the exits to go with their entry", ORDER_WAIT, |d| held(d) == 0);
    eprintln!("RESULT ctrader after the entry's cancel: held exits {}", held(&d));
    report.push(("ctrader", accepted && cancelled && gone));
    d.cleanup();
    for venue in ["ig", "dukascopy", "ctrader"] {
        assert_flat(&d.snap(), venue, "AFTER the brackets");
    }
    d.shut();
    eprintln!("RESULT brackets: {report:?}");
    assert!(report.iter().all(|(_, ok)| *ok), "a bracket did not end as expected: {report:?}");
}

#[test]
#[ignore = "LIVE: READ-ONLY, sends nothing; needs the node"]
fn desktop_reads_the_live_node() {
    one_per_process();
    let Some(boot) = boot() else { return };
    let Some((ig, duka, ct)) = engine_symbols(boot) else { return };
    // A fresh desktop fetches no catalog by itself (the Data Manager's Refresh does), so the picker
    // and the ticket's lot know nothing of these venues: the rows the order tests stage are staged
    // here too, into the scratch cache only, to show what their Trade windows then say.
    let rows = [
        staged("ig", &ig, 1.0, FX_TICK),
        staged("dukascopy", &duka, 1000.0, FX_TICK),
        staged("ctrader", &ct, 1000.0, FX_TICK),
    ];
    stage_catalog(boot, &rows);
    let Some(mut d) = Desk::launch(boot) else { return };
    let snap = d.snap();
    for b in &snap.portfolio.venues {
        eprintln!(
            "  engine {} {:?} symbol {} extra {:?} tier {:?} {:?}",
            b.venue, b.account, b.symbol, b.extra_symbols, b.tier, b.trading_state
        );
    }
    d.frames(250);
    eprintln!("READ before any window: {}", d.dump());
    eprintln!("READ market-data feeds the datahub mounted: {}", d.feeds());
    let targets =
        [("ig", ig), ("dukascopy", duka), ("ctrader", ct)].map(|(v, s)| (v.to_string(), s));
    eprintln!("READ catalog: {} rows", d.app().symbols_catalog.len());
    for (venue, symbol) in &targets {
        let filter = vike_catalog::SearchFilter { tab: None, venue: Some(venue.clone()) };
        let rows: Vec<String> = d
            .app()
            .symbols_catalog
            .search(symbol, &filter, 5)
            .iter()
            .map(|i| {
                format!(
                    "{} tick {} lot {} min {}",
                    i.raw_symbol,
                    i.properties.tick_size,
                    i.properties.step_size,
                    i.properties.min_qty
                )
            })
            .collect();
        eprintln!("READ {venue} {symbol}: catalog rows {rows:?}");
        eprintln!(
            "READ {venue} {symbol}: stored 1m price (close, age) {:?}",
            d.stored_price(venue, symbol)
        );
        let opened =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.open_trade(venue, symbol)));
        let Ok(w) = opened else {
            eprintln!(
                "READ {venue} {symbol}: no Trade window reaches it (the panic above says why)"
            );
            continue;
        };
        d.frames(100);
        let rect = d.win_rect(w);
        // The ticket as a trader sets up a limit order (clicks only, nothing is sent): where the
        // Price field is, against the window's own rect.
        d.click(Some(rect), "Limit", |l| l == "Limit");
        eprintln!("READ {venue} {symbol}: window {rect:?}; {}", d.ui_dump());
        let book = d
            .app()
            .books
            .get(venue, symbol)
            .map(|(b, _)| (b.best_bid().map(|l| l.price), b.best_ask().map(|l| l.price)));
        let buttons: Vec<String> = d
            .find(Some(Role::Button), Some(rect), |l| {
                l.starts_with("Buy ") || l == "TP/SL" || l == "Bid"
            })
            .iter()
            .map(|n| {
                format!(
                    "{} (disabled {})",
                    n.accesskit_node().label().unwrap_or_default(),
                    n.accesskit_node().is_disabled()
                )
            })
            .collect();
        eprintln!("READ {venue} {symbol}: book {book:?}; strip `{}`; {buttons:?}", d.strip(w));
    }
    eprintln!("READ market-data feeds after the windows: {}", d.feeds());
    d.shut();
}
