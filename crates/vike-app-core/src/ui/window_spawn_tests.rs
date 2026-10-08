use super::*;
use crate::ui::feed_lifecycle::live_window_keys;
use crate::ui::workspace::series_key;
use std::collections::HashSet;

/// A deliberately NON-ZERO desktop origin: a planner that dropped `desktop_min` on the floor
/// would satisfy every geometry assertion below if the arena happened to start at (0, 0).
const DESKTOP: egui::Pos2 = egui::pos2(12.0, 7.0);

/// The size a Trade window opens at, as a caller measured it ([`trade_open_size`]): deliberately
/// not the spec's 600 × 560, so a planner that ignored it and opened its own size would redden.
const TRADE: egui::Vec2 = egui::vec2(600.0, 687.0);

/// What each request is DECLARED to do about the subscription its window needs. Named per row
/// rather than inferred, so a NEW [`SpawnRequest`] arm has to be classified instead of quietly
/// joining the "ensures nothing" column — the same "a named row proves it was classified,
/// not forgotten" discipline the per-venue capability tables use.
///
/// ⚠ "has to" is only literal because [`the_roster_exercises_every_spawn_request_variant`]
/// makes it so. [`roster`] is a hand-written list with no roster constant behind it (a
/// data-carrying enum has no `ALL`), so on its own it would let a new variant take its
/// [`plan_spawn`] arm and be executed by no test in this module at all — absent, in
/// particular, from [`every_spawn_ensures_a_series_the_reaper_counts_as_live`], the one
/// assertion here that is an invariant rather than a pin.
enum FeedDuty {
    /// Ensures exactly the series its window reads — checked against the REAL
    /// `crates/vike-app-core/src/ui/feed_lifecycle.rs`'s `live_window_keys`, never a restatement
    /// of its rule.
    Ensures,
    /// Ensures nothing, and the argument for why that is correct rather than a hole.
    NoFeed(&'static str),
    /// Declines to spawn at all.
    NoWindow,
}

/// Every spawn path, once each — the six requests plus one row per [`WinKind`], so the launcher
/// arm is covered variant-by-variant rather than by whichever kind a test happened to pick.
fn roster() -> Vec<(&'static str, SpawnRequest, FeedDuty)> {
    let mut rows = vec![
        ("palette/blank", SpawnRequest::Palette { raw: "   ".to_string() }, FeedDuty::NoWindow),
        ("palette/symbol", SpawnRequest::Palette { raw: "eth".to_string() }, FeedDuty::Ensures),
        (
            "menu/new-window",
            SpawnRequest::NewWindowChart { symbol: "BTCUSDT".to_string() },
            FeedDuty::Ensures,
        ),
        (
            "title-bar/clone",
            SpawnRequest::CloneWindow {
                venue: DEFAULT_VENUE.to_string(),
                symbol: "BTCUSDT".to_string(),
                interval: "5m".to_string(),
            },
            FeedDuty::NoFeed(
                "a BINANCE source already ensured this exact key — the clone copies its \
                     symbol and interval, so `spawned` holds it already. ⚠ the reason is \
                     venue-CONDITIONAL: on any other venue the clone's key drops the `venue:` \
                     namespace and is backed by nobody, which is why this row is spelled with \
                     the default venue and why the loss is pinned separately",
            ),
        ),
        (
            "symbols/test-symbol",
            SpawnRequest::TestSymbol { symbol: "SOLUSDT".to_string() },
            FeedDuty::Ensures,
        ),
        (
            "stored/open-bar-series",
            SpawnRequest::StoredOpen {
                venue: "binance".to_string(),
                symbol: "BTCUSDT".to_string(),
                interval: Some("5m".to_string()),
            },
            FeedDuty::Ensures,
        ),
        (
            "stored/open-tick-only-series",
            SpawnRequest::StoredOpen {
                venue: "okx".to_string(),
                symbol: "BTC-USDT".to_string(),
                interval: None,
            },
            FeedDuty::Ensures,
        ),
    ];
    for kind in WinKind::ALL {
        let duty = match kind {
            WinKind::Chart => FeedDuty::Ensures,
            _ => FeedDuty::NoFeed("a tool window drives no bar series of its own"),
        };
        rows.push((
            kind.slug(),
            SpawnRequest::Kind {
                kind,
                poly_seed_token: POLY_PLACEHOLDER_TOKEN.to_string(),
                data_dest_seed: None,
            },
            duty,
        ));
    }
    rows
}

/// How many [`SpawnRequest`] variants [`variant_index`] knows about. Bumped by the author a
/// stale value has already stopped — see [`the_roster_exercises_every_spawn_request_variant`]
/// for the three-step ladder this number sits in the middle of.
const REQUEST_VARIANTS: usize = 6;

/// One dense index per [`SpawnRequest`] variant. EXHAUSTIVE and deliberately index-valued
/// rather than name-valued: a name-returning match plus a hand-written list of expected names
/// is not a gate at all — a new variant would be missing from BOTH sides and the comparison
/// would still hold. An index into a fixed-width array cannot be missing from both.
fn variant_index(req: &SpawnRequest) -> usize {
    match req {
        SpawnRequest::Palette { .. } => 0,
        SpawnRequest::NewWindowChart { .. } => 1,
        SpawnRequest::Kind { .. } => 2,
        SpawnRequest::CloneWindow { .. } => 3,
        SpawnRequest::TestSymbol { .. } => 4,
        SpawnRequest::StoredOpen { .. } => 5,
    }
}

/// The half a compile error cannot give you: being sent to the right FILE is not the same as
/// having added a row.
///
/// [`plan_spawn`]'s `match req` already refuses to compile on a new [`SpawnRequest`] variant,
/// which is what lands the author here — but [`roster`] is a hand-written `Vec`, so a variant
/// can take its planner arm, take a [`variant_index`] arm, and still be exercised by no test
/// in this module. It would be absent above all from
/// [`every_spawn_ensures_a_series_the_reaper_counts_as_live`], and a spawn path whose feed
/// nobody checks against the reaper is precisely the shape that opens a window which paints
/// for one frame and then goes dark.
///
/// The ladder, deliberately three rungs so no single omission is silent: the new variant fails
/// to COMPILE in [`variant_index`]; adding an arm there with a stale [`REQUEST_VARIANTS`]
/// fails THIS test by name; bumping the constant then fails this test again until a [`roster`]
/// row exists. Each rung's failure message names the next.
#[test]
fn the_roster_exercises_every_spawn_request_variant() {
    let mut seen = [false; REQUEST_VARIANTS];
    for (label, req, _) in roster() {
        let i = variant_index(&req);
        assert!(
            i < REQUEST_VARIANTS,
            "{label}: `variant_index` returned {i} — `REQUEST_VARIANTS` is stale, bump it to \
                 the number of `SpawnRequest` variants"
        );
        seen[i] = true;
    }
    let missing: Vec<usize> = (0..REQUEST_VARIANTS).filter(|i| !seen[*i]).collect();
    assert!(
        missing.is_empty(),
        "`SpawnRequest` variant index/indices {missing:?} are exercised by no `roster()` row \
             — add one (with its `FeedDuty`) or every test in this module skips that spawn path"
    );
}

/// **The invariant this module exists to hold, and the only test here that is not a pin.**
///
/// A spawned window and the feed it asks for are decided in two different places — this
/// planner's `ensure_feed`, and `live_window_keys`'s per-KIND rule — and a one-character
/// disagreement is not cosmetic: `reap_orphaned_feeds` runs every frame and stops any
/// `spawned` key no window backs, so the new window's stream dies on the frame after it opens
/// and nothing re-requests it (a spawn's ensure is a ONE-SHOT at creation). Driven over the REAL
/// reaper input, so it cannot drift with a copy of the rule.
#[test]
fn every_spawn_ensures_a_series_the_reaper_counts_as_live() {
    for (label, req, duty) in roster() {
        let plan = plan_spawn(req, DESKTOP, 4, TRADE);
        let win = plan.win;
        let feed = plan.ensure_feed;
        match duty {
            FeedDuty::NoWindow => {
                assert!(win.is_none(), "{label}: declared NoWindow but spawned one");
                assert!(feed.is_none(), "{label}: a feed with no window to read it");
            }
            FeedDuty::NoFeed(why) => {
                assert!(win.is_some(), "{label}: declared a window and spawned none");
                assert!(feed.is_none(), "{label}: unexpected feed — {why}");
            }
            FeedDuty::Ensures => {
                let win = win.unwrap_or_else(|| panic!("{label}: Ensures, but no window"));
                let f = feed.unwrap_or_else(|| panic!("{label}: Ensures, but no feed"));
                let key = series_key(&f.venue, &f.symbol, &f.interval);
                let live = live_window_keys(std::slice::from_ref(&win));
                assert!(
                    live.contains(&key),
                    "{label}: ensures `{key}`, which no window backs — `reap_orphaned_feeds` \
                         stops it on the next frame and nothing re-requests it (live: {live:?})"
                );
            }
        }
    }
}

/// A spawn that forgets to advance the counter hands the next window the SAME `egui::Id`, and
/// egui keys a window's whole persisted state on that id — two windows would share geometry,
/// and `App::tool_views` (keyed by `WinState::id`) would hand them one view state. The id
/// PREFIX differing per family is what made the six inline copies look safe; the COUNTER is
/// shared, so the prefix is decoration and this is the property that actually holds.
#[test]
fn spawning_the_whole_roster_back_to_back_never_reuses_an_egui_id() {
    let mut n = 0;
    let mut ids = Vec::new();
    for (label, req, _) in roster() {
        let plan = plan_spawn(req, DESKTOP, n, TRADE);
        // EXACT, not `>=`. A `>=` passes on an arm that forgets to advance, and whether the
        // duplicate id that follows is actually OBSERVED then depends on the NEXT roster row
        // happening to share an id prefix — `chart-` follows `chart-`, but a `tool-` arm that
        // stalls is followed by the `poly-` row and the collision never materialises. That is
        // an accident of this list's order, not a property, so the step is asserted directly:
        // one id per window published, and none per window declined.
        let expected = if plan.win.is_some() { n + 1 } else { n };
        assert_eq!(plan.next_win_n, expected, "{label}: wrong window-counter step");
        n = plan.next_win_n;
        if let Some(w) = plan.win {
            ids.push(w.id);
        }
    }
    let unique: HashSet<egui::Id> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "two windows in one session share an egui::Id");
}

/// A stray Enter in an empty palette must cost nothing — no window, no live subscribe, and no
/// window id, because the counter IS the id seed and a burned one is a permanent hole in the
/// sequence. Without the guard the mutated build opens a chart titled `USDT · 1m` and issues a
/// real Binance subscribe for a symbol named `USDT`.
#[test]
fn a_blank_palette_spawns_nothing_and_burns_no_cascade_slot() {
    for raw in ["", "   ", "\t\n "] {
        let plan = plan_spawn(SpawnRequest::Palette { raw: raw.to_string() }, DESKTOP, 5, TRADE);
        assert!(plan.win.is_none(), "{raw:?} spawned a window");
        assert!(plan.ensure_feed.is_none(), "{raw:?} subscribed a feed");
        assert_eq!(plan.next_win_n, 5, "{raw:?} burned a window id");
    }
}

/// The palette's symbol ladder, pinned INCLUDING the part that is wrong: the quote-currency
/// suffix is a naive `ends_with`, so `btcusd` becomes `BTCUSDUSDT` rather than being corrected
/// or refused. That is today's behaviour and this move preserves it byte-for-byte; the row is
/// here so a fix is a deliberate edit to a red test rather than an accident.
#[test]
fn the_palette_trims_uppercases_and_appends_the_quote_currency() {
    for (raw, expect) in [
        ("btc", "BTCUSDT"),
        ("  eth  ", "ETHUSDT"),
        ("solusdt", "SOLUSDT"),
        ("SOLUSDT", "SOLUSDT"),
        ("btcusd", "BTCUSDUSDT"),
    ] {
        let plan = plan_spawn(SpawnRequest::Palette { raw: raw.to_string() }, DESKTOP, 0, TRADE);
        let win = plan.win.expect("a non-blank palette entry spawns a chart");
        assert_eq!(win.symbol, expect, "palette {raw:?}");
        assert_eq!(win.interval, "1m", "palette {raw:?}");
        assert_eq!(win.kind, WinKind::Chart, "palette {raw:?}");
        let f = plan.ensure_feed.expect("the chart needs its own feed");
        assert_eq!(
            (f.venue.as_str(), f.symbol.as_str(), f.interval.as_str()),
            (DEFAULT_VENUE, expect, "1m"),
            "palette {raw:?}"
        );
    }
}

/// The exhaustive-match half: every [`WinKind`] opens SOMETHING with a non-zero size and spends
/// exactly one id. The `_ => tool` arm this replaced would have let a new kind inherit the tool
/// geometry silently; the compiler stops that now, and this stops a new arm from being wired to
/// spawn nothing at all.
#[test]
fn every_window_kind_spawns_exactly_one_window_and_consumes_one_id() {
    for kind in WinKind::ALL {
        let req = SpawnRequest::Kind { kind, poly_seed_token: String::new(), data_dest_seed: None };
        let plan = plan_spawn(req, DESKTOP, 3, TRADE);
        let win = plan.win.unwrap_or_else(|| panic!("{kind:?} spawned no window"));
        assert_eq!(win.kind, kind);
        assert_eq!(plan.next_win_n, 4, "{kind:?}");
        assert!(win.size.x > 0.0 && win.size.y > 0.0, "{kind:?} opened at zero size");
    }
}

/// The Data Manager seed's twin of [`the_cockpit_resolves_only_a_still_placeholder_token`]: a
/// `data_dest_seed` is carried out as `SpawnPlan::data_dest_seed` ONLY for `WinKind::Data`, paired
/// with the freshly minted window's own id (the caller, `App::apply_spawn`, pre-seeds
/// `app.tool_views` from exactly that pair). `None` leaves it unset, and every non-Data kind
/// ignores the field outright even when one is supplied.
#[test]
fn the_data_manager_seed_resolves_only_for_winkind_data() {
    let seeded = SpawnRequest::Kind {
        kind: WinKind::Data,
        poly_seed_token: String::new(),
        data_dest_seed: Some(DataDest::Credentials),
    };
    let plan = plan_spawn(seeded, DESKTOP, 5, TRADE);
    let win = plan.win.expect("Data Manager window");
    assert_eq!(plan.data_dest_seed, Some((win.id, DataDest::Credentials)));

    let unseeded = SpawnRequest::Kind {
        kind: WinKind::Data,
        poly_seed_token: String::new(),
        data_dest_seed: None,
    };
    let plan = plan_spawn(unseeded, DESKTOP, 5, TRADE);
    assert_eq!(plan.data_dest_seed, None, "no seed requested, none carried out");

    for kind in WinKind::ALL {
        if kind == WinKind::Data {
            continue;
        }
        let req = SpawnRequest::Kind {
            kind,
            poly_seed_token: String::new(),
            data_dest_seed: Some(DataDest::Credentials),
        };
        let plan = plan_spawn(req, DESKTOP, 5, TRADE);
        assert_eq!(plan.data_dest_seed, None, "{kind:?} must ignore the Data Manager seed");
    }
}

/// The cockpit's background Gamma resolve OVERWRITES the window's `symbol`. Requesting it for a
/// real seeded token would silently move the operator's window to a different market; never
/// requesting it leaves a placeholder window permanently STALE. The gate is the seed still
/// being the placeholder — and it is per-KIND, so a non-cockpit launcher ignores the seed.
#[test]
fn the_cockpit_resolves_only_a_still_placeholder_token() {
    let ph_req = SpawnRequest::Kind {
        kind: WinKind::Polymarket,
        poly_seed_token: POLY_PLACEHOLDER_TOKEN.to_string(),
        data_dest_seed: None,
    };
    let placeholder = plan_spawn(ph_req, DESKTOP, 2, TRADE);
    let win = placeholder.win.expect("cockpit window");
    assert_eq!(placeholder.resolve_poly_token, Some(win.id));
    assert_eq!(win.symbol, POLY_PLACEHOLDER_TOKEN);
    assert_eq!(win.title, "Polymarket · Cockpit");

    let seeded_req = SpawnRequest::Kind {
        kind: WinKind::Polymarket,
        poly_seed_token: "0xfeed".to_string(),
        data_dest_seed: None,
    };
    let seeded = plan_spawn(seeded_req, DESKTOP, 2, TRADE);
    assert_eq!(seeded.resolve_poly_token, None, "a seeded token must not be resolved over");
    assert_eq!(seeded.win.expect("cockpit window").symbol, "0xfeed");

    for kind in WinKind::ALL {
        if kind == WinKind::Polymarket {
            continue;
        }
        let req = SpawnRequest::Kind {
            kind,
            poly_seed_token: POLY_PLACEHOLDER_TOKEN.to_string(),
            data_dest_seed: None,
        };
        let plan = plan_spawn(req, DESKTOP, 2, TRADE);
        assert_eq!(plan.resolve_poly_token, None, "{kind:?} must ignore the cockpit seed");
    }
}

/// The Trade launcher opens at the size its caller measured with no instrument yet: the glue seeds
/// the last-used instrument, or the backend's primary market, on the first frame (spec §3.11). It
/// asks for no bar feed (Ruling R6) and no depth stream — the window loop requests depth, every
/// frame, once the window has a symbol.
#[test]
fn a_trade_launcher_opens_at_the_size_it_was_given_with_no_instrument_yet() {
    let req = SpawnRequest::Kind {
        kind: WinKind::Trade,
        poly_seed_token: String::new(),
        data_dest_seed: None,
    };
    let plan = plan_spawn(req, DESKTOP, 1, TRADE);
    let win = plan.win.expect("Trade window");
    assert_eq!(win.size, TRADE);
    assert_eq!(win.id, egui::Id::new("trade-1"));
    assert_eq!(win.title, "Trade");
    assert!(win.symbol.is_empty(), "the glue seeds it on the first frame");
    assert!(plan.ensure_feed.is_none(), "no bar feed (Ruling R6)");
}

/// The size a Trade window opens at follows the look (FW5 B1): the spec's height where the whole form
/// fits it, as at Compact density, and taller where it does not, as at Comfortable, where the render
/// check of 2026-10-03 found TP/SL 61 pt below the fold. The width never moves with the density; it
/// moves with the TEXT: the spec's 600 at Small, and as much wider as the ticket grew at the default
/// Standard (626), because the ticket grows with the text.
#[test]
fn a_trade_window_opens_as_tall_as_its_whole_form_needs_in_the_look() {
    use vike_ui_theme::appearance::{Appearance, install};
    use vike_ui_theme::metrics::Density;
    let open = |density| {
        let ctx = egui::Context::default();
        install(&ctx, &Appearance { density, ..Appearance::default() });
        let mut size = egui::Vec2::ZERO;
        let out = ctx.run_ui(egui::RawInput::default(), |ui| size = trade_open_size(ui.ctx()));
        out.drop_without_applying_deltas();
        size
    };
    assert_eq!(open(Density::Compact), egui::vec2(626.0, 560.0), "Compact: the spec's size");
    let loose = open(Density::Comfortable);
    assert_eq!(loose.x, 626.0, "the width never moves with the density");
    assert!(loose.y > 560.0 + 61.0, "Comfortable: more than TP/SL's 61 pt: {loose:?}");
}

/// Stored ▸ Open in chart is the ONE site that overrides `venue`, so it is the one that must
/// retitle — and the one whose feed key is venue-namespaced. A tick-only series
/// (`interval: None`) falls back to the venue's 1m klines.
#[test]
fn a_stored_open_carries_its_venue_into_the_window_title_and_the_feed() {
    let req = SpawnRequest::StoredOpen {
        venue: "okx".to_string(),
        symbol: "BTC-USDT".to_string(),
        interval: None,
    };
    let plan = plan_spawn(req, DESKTOP, 0, TRADE);
    let win = plan.win.expect("stored open spawns a chart");
    assert_eq!(win.venue, "okx");
    assert_eq!(win.interval, "1m", "a tick-only series has no timeframe of its own");
    assert_eq!(win.title, "OKX BTC-USDT · 1m", "retitle() ran AFTER the venue was assigned");
    assert_eq!(win.key(), "okx:BTC-USDT@1m");
    let f = plan.ensure_feed.expect("the reopened series' feed");
    assert_eq!(series_key(&f.venue, &f.symbol, &f.interval), win.key());

    // …and a Binance stored open stays byte-identical to every other chart open.
    let plain_req = SpawnRequest::StoredOpen {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: Some("5m".to_string()),
    };
    let plain = plan_spawn(plain_req, DESKTOP, 0, TRADE).win.expect("stored open spawns a chart");
    assert_eq!(plain.title, "BTCUSDT · 5m");
    assert_eq!(plain.key(), "BTCUSDT@5m");
}

/// **A PIN, not an endorsement.** A title-bar clone copies its source window's symbol and
/// interval but NOT its venue, so cloning an OKX chart yields a window pointed at Binance
/// carrying an OKX symbol — and because a clone deliberately ensures no feed (the source's key
/// is already in `spawned`), the clone's own key is subscribed by nobody and it paints nothing.
/// That is the behaviour of every build that has shipped; this move preserves it exactly. The
/// fix is a STEP-2 change that must edit this test, which is the entire point of writing it.
///
/// The request is spelled with a NON-default venue on purpose. A clone request that carried no
/// venue at all could only ever have proved that [`WinState::new`] hardcodes [`DEFAULT_VENUE`]
/// — a fact about `WinState`, true whatever the clone does — so the test would have worn a
/// name it could not check. Handing it `okx` is what makes "loses its source venue" an
/// observation rather than a caption, and the two key assertions below are what makes the
/// CONSEQUENCE observable rather than merely asserted in prose: the clone's key is not the
/// series its source ensured, so nothing backs it.
#[test]
fn a_cloned_window_keeps_symbol_and_interval_but_loses_its_source_venue() {
    let req = SpawnRequest::CloneWindow {
        venue: "okx".to_string(),
        symbol: "BTC-USDT".to_string(),
        interval: "5m".to_string(),
    };
    let plan = plan_spawn(req, DESKTOP, 0, TRADE);
    let win = plan.win.expect("clone spawns a chart");
    assert_eq!(win.symbol, "BTC-USDT");
    assert_eq!(win.interval, "5m");
    assert_eq!(
        win.venue, DEFAULT_VENUE,
        "PINNED: the request CARRIES `okx` and the clone still opens on binance"
    );
    assert!(plan.ensure_feed.is_none(), "PINNED: a clone subscribes nothing of its own");
    assert_eq!(win.key(), "BTC-USDT@5m", "PINNED: the un-namespaced Binance key form");
    assert_ne!(
        win.key(),
        series_key("okx", "BTC-USDT", "5m"),
        "PINNED: and therefore NOT the series the source window ensured — a feed-less clone \
             is only sound while it reproduces its source's key, and this one does not"
    );
    assert_eq!(win.title, "BTC-USDT · 5m", "PINNED: the OKX title prefix goes with the venue");
}

/// The cascade table, pinned per site — including the ONE site that disagrees with the other
/// five about where slot 0 sits. Window ▸ New window has always cascaded from (40, 30) and
/// every other site from (50, 40); nothing documented the difference and nothing depended on
/// it, which is exactly why the tidy-up that unifies them must redden a test instead of
/// silently moving a window ten points.
#[test]
fn each_spawn_site_cascades_from_its_own_pinned_origin_and_size() {
    // n = 3 ⇒ slot 3 ⇒ 28 × 3 = 84 points along the cascade. DESKTOP is (12, 7), so the
    // five-site origin lands at (12+50+84, 7+40+84) and New window's at (12+40+84, 7+30+84).
    let at = |req: SpawnRequest| plan_spawn(req, DESKTOP, 3, TRADE).win.expect("a window");
    let launch = |k: WinKind| SpawnRequest::Kind {
        kind: k,
        poly_seed_token: String::new(),
        data_dest_seed: None,
    };

    let palette = at(SpawnRequest::Palette { raw: "btc".to_string() });
    assert_eq!(palette.pos, egui::pos2(146.0, 131.0));
    assert_eq!(palette.size, egui::vec2(640.0, 420.0));
    assert_eq!(palette.id, egui::Id::new("chart-3"));

    let new_window = at(SpawnRequest::NewWindowChart { symbol: "BTCUSDT".to_string() });
    assert_eq!(
        new_window.pos,
        egui::pos2(136.0, 121.0),
        "PINNED divergence: this site alone cascades from (40, 30)"
    );
    assert_eq!(new_window.size, egui::vec2(700.0, 440.0));

    let chart = at(launch(WinKind::Chart));
    assert_eq!((chart.pos, chart.size), (egui::pos2(146.0, 131.0), egui::vec2(640.0, 420.0)));

    let d = at(launch(WinKind::Trade));
    assert_eq!((d.pos, d.size), (egui::pos2(146.0, 131.0), TRADE));
    assert_eq!(d.id, egui::Id::new("trade-3"));

    let p = at(launch(WinKind::Polymarket));
    assert_eq!((p.pos, p.size), (egui::pos2(146.0, 131.0), egui::vec2(340.0, 640.0)));
    assert_eq!(p.id, egui::Id::new("poly-3"));

    let t = at(launch(WinKind::News));
    assert_eq!((t.pos, t.size), (egui::pos2(146.0, 131.0), egui::vec2(560.0, 400.0)));
    assert_eq!(t.id, egui::Id::new("tool-3"));

    let s = at(launch(WinKind::Settings));
    assert_eq!((s.pos, s.size), (egui::pos2(146.0, 131.0), egui::vec2(640.0, 600.0)));
    assert_eq!(s.id, egui::Id::new("tool-3"));

    let c = at(SpawnRequest::CloneWindow {
        venue: DEFAULT_VENUE.to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1m".to_string(),
    });
    assert_eq!((c.pos, c.size), (egui::pos2(146.0, 131.0), egui::vec2(620.0, 380.0)));

    let ts = at(SpawnRequest::TestSymbol { symbol: "SOLUSDT".to_string() });
    assert_eq!((ts.pos, ts.size), (egui::pos2(146.0, 131.0), egui::vec2(700.0, 440.0)));

    // Eight slots, then the walk WRAPS: slot 8 lands exactly on slot 0. Today's behaviour —
    // the ids still differ, so egui keeps the two windows' state apart and either can be
    // dragged off the other.
    let sym = || SpawnRequest::NewWindowChart { symbol: "BTCUSDT".to_string() };
    let slot0 = plan_spawn(sym(), DESKTOP, 0, TRADE).win.expect("a window");
    let slot8 = plan_spawn(sym(), DESKTOP, 8, TRADE).win.expect("a window");
    assert_eq!(slot0.pos, slot8.pos, "PINNED: the cascade wraps every 8 windows");
    assert_ne!(slot0.id, slot8.id);
}
