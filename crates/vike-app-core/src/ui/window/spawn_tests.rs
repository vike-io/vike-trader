use super::*;
use crate::data::chart_key::series_key;
use crate::ui::feed_lifecycle::live_window_keys;
use crate::ui::windows::POLYMARKET_PLACEHOLDER_TOKEN;
use std::collections::HashSet;

/// A deliberately NON-ZERO desktop origin: a planner that dropped `desktop_min` on the floor
/// would satisfy every geometry assertion below if the arena happened to start at (0, 0).
const DESKTOP: egui::Pos2 = egui::pos2(12.0, 7.0);

/// The size a Trade window opens at, as a caller measured it ([`trade_open_size`]): deliberately
/// not the spec's 600 × 560, so a planner that ignored it and opened its own size would redden.
const TRADE: egui::Vec2 = egui::vec2(600.0, 687.0);

/// The default series a shell resolved ([`default_series`]): deliberately a venue and a symbol no
/// code names, so a planner that fell back to a compiled one would redden.
fn last() -> SeriesSpec {
    SeriesSpec {
        venue: "some-venue".to_string(),
        symbol: "SOME-PAIR".to_string(),
        interval: "5m".to_string(),
        asset_class: None,
    }
}

/// What each request is DECLARED to do about the subscription its window needs. Named per row
/// rather than inferred, so a NEW [`WindowSpawnRequest`] arm has to be classified instead of quietly
/// joining the "ensures nothing" column — the same "a named row proves it was classified,
/// not forgotten" discipline the per-venue capability tables use.
///
/// ⚠ "has to" is only literal because [`the_roster_exercises_every_spawn_request_variant`]
/// makes it so. [`roster`] is a hand-written list with no roster constant behind it (a
/// data-carrying enum has no `ALL`), so on its own it would let a new variant take its
/// [`plan_window_spawn`] arm and be executed by no test in this module at all — absent, in
/// particular, from [`every_spawn_ensures_a_series_the_reaper_counts_as_live`], the one
/// assertion here that is an invariant rather than a pin.
enum FeedDuty {
    /// Ensures exactly the series its window reads — checked against the REAL
    /// `crates/vike-app-core/src/ui/feed_lifecycle/mod.rs`'s `live_window_keys`, never a restatement
    /// of its rule.
    Ensures,
    /// Ensures nothing, and the argument for why that is correct rather than a hole.
    NoFeed(&'static str),
    /// Declines to spawn at all.
    NoWindow,
}

/// Every spawn path, once each — the seven requests plus one row per [`WindowKind`], so the launcher
/// arm is covered variant-by-variant rather than by whichever kind a test happened to pick.
fn roster() -> Vec<(&'static str, WindowSpawnRequest, FeedDuty)> {
    let mut rows = vec![
        (
            "palette/blank",
            WindowSpawnRequest::Palette { raw: "   ".to_string() },
            FeedDuty::NoWindow,
        ),
        (
            "palette/symbol",
            WindowSpawnRequest::Palette { raw: "eth".to_string() },
            FeedDuty::Ensures,
        ),
        ("menu/new-window", WindowSpawnRequest::NewWindowChart, FeedDuty::Ensures),
        (
            "title-bar/clone",
            WindowSpawnRequest::CloneWindow {
                venue: "okx".to_string(),
                symbol: "BTC-USDT".to_string(),
                interval: "5m".to_string(),
            },
            FeedDuty::NoFeed(
                "the source already ensured this exact key — the clone keeps its venue, symbol \
                     and interval, so `spawned` holds it already",
            ),
        ),
        (
            "symbols/test-symbol",
            WindowSpawnRequest::TestSymbol {
                series: SeriesSpec {
                    venue: "binance".to_string(),
                    symbol: "SOLUSDT".to_string(),
                    interval: "1h".to_string(),
                    asset_class: None,
                },
            },
            FeedDuty::Ensures,
        ),
        (
            "stored/open-bar-series",
            WindowSpawnRequest::StoredOpen {
                venue: "binance".to_string(),
                symbol: "BTCUSDT".to_string(),
                interval: Some("5m".to_string()),
            },
            FeedDuty::Ensures,
        ),
        (
            "stored/open-tick-only-series",
            WindowSpawnRequest::StoredOpen {
                venue: "okx".to_string(),
                symbol: "BTC-USDT".to_string(),
                interval: None,
            },
            FeedDuty::Ensures,
        ),
        (
            "launcher/greeks",
            WindowSpawnRequest::PortfolioGreeks,
            FeedDuty::NoFeed("a tool window drives no bar series of its own"),
        ),
    ];
    for kind in WindowKind::ALL {
        let duty = match kind {
            WindowKind::Chart => FeedDuty::Ensures,
            _ => FeedDuty::NoFeed("a tool window drives no bar series of its own"),
        };
        rows.push((
            kind.slug(),
            WindowSpawnRequest::Kind {
                kind,
                polymarket_seed_token: POLYMARKET_PLACEHOLDER_TOKEN.to_string(),
                data_destination_seed: None,
            },
            duty,
        ));
    }
    rows
}

/// How many [`WindowSpawnRequest`] variants [`variant_index`] knows about. Bumped by the author a
/// stale value has already stopped — see [`the_roster_exercises_every_spawn_request_variant`]
/// for the three-step ladder this number sits in the middle of.
const REQUEST_VARIANTS: usize = 7;

/// One dense index per [`WindowSpawnRequest`] variant. EXHAUSTIVE and deliberately index-valued
/// rather than name-valued: a name-returning match plus a hand-written list of expected names
/// is not a gate at all — a new variant would be missing from BOTH sides and the comparison
/// would still hold. An index into a fixed-width array cannot be missing from both.
fn variant_index(req: &WindowSpawnRequest) -> usize {
    match req {
        WindowSpawnRequest::Palette { .. } => 0,
        WindowSpawnRequest::NewWindowChart => 1,
        WindowSpawnRequest::Kind { .. } => 2,
        WindowSpawnRequest::CloneWindow { .. } => 3,
        WindowSpawnRequest::TestSymbol { .. } => 4,
        WindowSpawnRequest::StoredOpen { .. } => 5,
        WindowSpawnRequest::PortfolioGreeks => 6,
    }
}

/// The half a compile error cannot give you: being sent to the right FILE is not the same as
/// having added a row.
///
/// [`plan_window_spawn`]'s `match req` already refuses to compile on a new [`WindowSpawnRequest`] variant,
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
                 the number of `WindowSpawnRequest` variants"
        );
        seen[i] = true;
    }
    let missing: Vec<usize> = (0..REQUEST_VARIANTS).filter(|i| !seen[*i]).collect();
    assert!(
        missing.is_empty(),
        "`WindowSpawnRequest` variant index/indices {missing:?} are exercised by no `roster()` row \
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
/// reaper input, so it cannot drift with a copy of the rule. Run with a default series, so every
/// chart opener has one to open on; with none they open EMPTY, which
/// [`a_chart_opened_with_no_default_is_empty_and_asks_for_no_feed`] pins.
#[test]
fn every_spawn_ensures_a_series_the_reaper_counts_as_live() {
    for (label, req, duty) in roster() {
        let plan = plan_window_spawn(req, DESKTOP, 4, TRADE, Some(&last()));
        let win = plan.window;
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
/// and `App::window_views` (keyed by `WindowState::id`) would hand them one view state. The id
/// PREFIX differing per family is what made the six inline copies look safe; the COUNTER is
/// shared, so the prefix is decoration and this is the property that actually holds.
#[test]
fn spawning_the_whole_roster_back_to_back_never_reuses_an_egui_id() {
    let mut n = 0;
    let mut ids = Vec::new();
    for (label, req, _) in roster() {
        let plan = plan_window_spawn(req, DESKTOP, n, TRADE, Some(&last()));
        // EXACT, not `>=`. A `>=` passes on an arm that forgets to advance, and whether the
        // duplicate id that follows is actually OBSERVED then depends on the NEXT roster row
        // happening to share an id prefix — `chart-` follows `chart-`, but a `tool-` arm that
        // stalls is followed by the `polymarket-` row and the collision never materialises. That is
        // an accident of this list's order, not a property, so the step is asserted directly:
        // one id per window published, and none per window declined.
        let expected = if plan.window.is_some() { n + 1 } else { n };
        assert_eq!(plan.next_window_number, expected, "{label}: wrong window-counter step");
        n = plan.next_window_number;
        if let Some(w) = plan.window {
            ids.push(w.id);
        }
    }
    let unique: HashSet<egui::Id> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "two windows in one session share an egui::Id");
}

/// A stray Enter in an empty palette must cost nothing — no window, no live subscribe, and no
/// window id, because the counter IS the id seed and a burned one is a permanent hole in the
/// sequence. Without the guard the mutated build opens a chart on a blank symbol.
#[test]
fn a_blank_palette_spawns_nothing_and_burns_no_cascade_slot() {
    for raw in ["", "   ", "\t\n "] {
        let req = WindowSpawnRequest::Palette { raw: raw.to_string() };
        let plan = plan_window_spawn(req, DESKTOP, 5, TRADE, Some(&last()));
        assert!(plan.window.is_none(), "{raw:?} spawned a window");
        assert!(plan.ensure_feed.is_none(), "{raw:?} subscribed a feed");
        assert_eq!(plan.next_window_number, 5, "{raw:?} burned a window id");
    }
}

/// The palette opens what was TYPED, trimmed and upper-cased, and guesses nothing onto it: no
/// quote currency is appended (it appended `USDT`, so `btcusd` became `BTCUSDUSDT`, until the
/// owner's ruling of 2026-11-13). The venue is the default series'; with none the chart names no
/// venue and asks for no feed — its badge says what is missing.
#[test]
fn the_palette_trims_uppercases_and_appends_nothing() {
    for (raw, expect) in [("btc", "BTC"), ("  eth  ", "ETH"), ("btcusd", "BTCUSD")] {
        let req = WindowSpawnRequest::Palette { raw: raw.to_string() };
        let plan = plan_window_spawn(req, DESKTOP, 0, TRADE, Some(&last()));
        let win = plan.window.expect("a non-blank palette entry spawns a chart");
        assert_eq!((win.venue.as_str(), win.symbol.as_str()), ("some-venue", expect), "{raw:?}");
        assert_eq!(win.interval, EMPTY_CHART_INTERVAL, "palette {raw:?}");
        assert_eq!(win.kind, WindowKind::Chart, "palette {raw:?}");
        let f = plan.ensure_feed.expect("the chart needs its own feed");
        assert_eq!(series_key(&f.venue, &f.symbol, &f.interval), win.key(), "{raw:?}");

        let req = WindowSpawnRequest::Palette { raw: raw.to_string() };
        let plan = plan_window_spawn(req, DESKTOP, 0, TRADE, None);
        let win = plan.window.expect("a typed symbol still opens a chart");
        assert_eq!((win.venue.as_str(), win.symbol.as_str()), ("", expect), "{raw:?}");
        assert!(plan.ensure_feed.is_none(), "{raw:?}: no venue, so nothing to subscribe");
    }
}

/// **No compiled series**: with no default (no chart names a symbol and no DataSet a venue), the
/// launcher's chart and Window ▸ New window open EMPTY — no venue, no symbol, no feed — and
/// unpinned, so a series the backend publishes is adopted into them; with one, both open on it,
/// venue, interval and class included.
#[test]
fn a_chart_opened_with_no_default_is_empty_and_asks_for_no_feed() {
    let launch = || WindowSpawnRequest::Kind {
        kind: WindowKind::Chart,
        polymarket_seed_token: String::new(),
        data_destination_seed: None,
    };
    for req in [launch(), WindowSpawnRequest::NewWindowChart] {
        let plan = plan_window_spawn(req, DESKTOP, 0, TRADE, None);
        let win = plan.window.expect("a chart window");
        assert_eq!((win.venue.as_str(), win.symbol.as_str()), ("", ""));
        assert_eq!(win.interval, EMPTY_CHART_INTERVAL);
        assert!(!win.series_pinned, "nobody chose it");
        assert!(plan.ensure_feed.is_none(), "an empty chart asks for nothing");
        let empty_title =
            format!("{} · {EMPTY_CHART_INTERVAL}", crate::ui::window::state::NO_SYMBOL_LABEL);
        assert_eq!(win.title, empty_title);
    }
    let classed = SeriesSpec { asset_class: Some(vike_model::AssetClass::CryptoPerp), ..last() };
    for req in [launch(), WindowSpawnRequest::NewWindowChart] {
        let plan = plan_window_spawn(req, DESKTOP, 0, TRADE, Some(&classed));
        let win = plan.window.expect("a chart window");
        assert_eq!(win.key(), series_key("some-venue", "SOME-PAIR", "5m"));
        assert_eq!(win.asset_class, classed.asset_class);
        assert_eq!(win.title, "SOME-VENUE SOME-PAIR · 5m");
        assert_eq!(plan.ensure_feed, Some(classed.clone()));
    }
}

/// **Every chart OPENED on a series is a choice** (`series_pinned`), the venue-less palette pick with
/// no default included: only a chart opened EMPTY may adopt what the backend publishes. The owner's
/// Data Manager opens were unpinned and swapped onto the node's series while their history loaded.
#[test]
fn a_chart_opened_on_a_series_is_a_choice_and_only_an_empty_one_may_adopt() {
    for default in [Some(last()), None] {
        for (label, req, _) in roster() {
            let opens_default = matches!(
                req,
                WindowSpawnRequest::NewWindowChart
                    | WindowSpawnRequest::Kind { kind: WindowKind::Chart, .. }
            );
            let Some(win) = plan_window_spawn(req, DESKTOP, 0, TRADE, default.as_ref()).window
            else {
                continue;
            };
            if win.kind == WindowKind::Chart {
                let chosen = !(opens_default && default.is_none());
                assert_eq!(win.series_pinned, chosen, "{label} (default {default:?})");
            }
        }
    }
}

/// [`default_series`] is the user's NEWEST chart that names a symbol, then the first symbol of the
/// first DataSet whose provider names a venue (on the set's interval), then nothing.
#[test]
fn the_default_series_is_the_newest_chart_then_the_first_symbol_set_then_nothing() {
    let set = |name: &str, provider: &str, symbols: &[&str]| SymbolSet {
        name: name.to_string(),
        symbols: symbols.iter().map(|s| s.to_string()).collect(),
        provider: provider.to_string(),
        interval: "1h".to_string(),
        benchmark: String::new(),
        user: true,
    };
    let sets =
        vec![set("any", "Auto", &["XX"]), set("empty", "v", &[]), set("mine", "v", &["YY", "ZZ"])];
    assert_eq!(default_series(&[], &[]), None, "nothing compiled in");
    let from_set = default_series(&[], &sets).expect("the first set that names a venue");
    assert_eq!((from_set.venue.as_str(), from_set.symbol.as_str()), ("v", "YY"));
    assert_eq!(from_set.interval, "1h");

    let r = egui::Rect::from_min_size(DESKTOP, egui::vec2(10.0, 10.0));
    let older = chart_on("a", Some(&SeriesSpec { symbol: "OLD".to_string(), ..last() }), r);
    let newer = chart_on("b", Some(&last()), r);
    let empty = chart_on("c", None, r);
    let tool = WindowState::tool("d", WindowKind::News, r);
    let wins = vec![older, newer, empty, tool];
    assert_eq!(default_series(&wins, &sets), Some(last()), "the newest chart naming a symbol wins");
}

/// The pickers' offers before the instrument list loads: open charts newest first, then every
/// DataSet symbol on a provider that names a venue, each pair once; nothing on a fresh install.
#[test]
fn quick_picks_are_the_users_own_charts_then_symbol_sets_each_once() {
    let pick = |venue: &str, symbol: &str| QuickPick { venue: venue.into(), symbol: symbol.into() };
    assert!(quick_picks(&[], &[]).is_empty(), "nothing compiled in");
    let r = egui::Rect::from_min_size(DESKTOP, egui::vec2(10.0, 10.0));
    let wins = vec![
        chart_on("a", Some(&SeriesSpec { symbol: "ONE".to_string(), ..last() }), r),
        chart_on("b", Some(&last()), r),
        chart_on("c", None, r),
    ];
    let sets = vec![
        SymbolSet {
            name: "s".into(),
            symbols: vec!["SOME-PAIR".into(), "TWO".into()],
            provider: "some-venue".into(),
            interval: "1m".into(),
            benchmark: String::new(),
            user: true,
        },
        SymbolSet {
            name: "auto".into(),
            symbols: vec!["THREE".into()],
            provider: "Auto".into(),
            interval: "1m".into(),
            benchmark: String::new(),
            user: true,
        },
    ];
    assert_eq!(
        quick_picks(&wins, &sets),
        [pick("some-venue", "SOME-PAIR"), pick("some-venue", "ONE"), pick("some-venue", "TWO")]
    );
}

/// The exhaustive-match half: every [`WindowKind`] opens SOMETHING with a non-zero size and spends
/// exactly one id. The `_ => tool` arm this replaced would have let a new kind inherit the tool
/// geometry silently; the compiler stops that now, and this stops a new arm from being wired to
/// spawn nothing at all.
#[test]
fn every_window_kind_spawns_exactly_one_window_and_consumes_one_id() {
    for kind in WindowKind::ALL {
        let req = WindowSpawnRequest::Kind {
            kind,
            polymarket_seed_token: String::new(),
            data_destination_seed: None,
        };
        let plan = plan_window_spawn(req, DESKTOP, 3, TRADE, None);
        let win = plan.window.unwrap_or_else(|| panic!("{kind:?} spawned no window"));
        assert_eq!(win.kind, kind);
        assert_eq!(plan.next_window_number, 4, "{kind:?}");
        assert!(win.size.x > 0.0 && win.size.y > 0.0, "{kind:?} opened at zero size");
    }
}

/// The Data Manager seed's twin of
/// [`the_cockpit_carries_its_seed_token_and_no_other_kind_reads_it`]: a
/// `data_destination_seed` is carried out as `WindowSpawnPlan::data_destination_seed` ONLY for `WindowKind::Data`, paired
/// with the freshly minted window's own id (the caller, `App::apply_spawn`, pre-seeds
/// `app.window_views` from exactly that pair). `None` leaves it unset, and every non-Data kind
/// ignores the field outright even when one is supplied.
#[test]
fn the_data_manager_seed_resolves_only_for_window_kind_data() {
    let seeded = WindowSpawnRequest::Kind {
        kind: WindowKind::Data,
        polymarket_seed_token: String::new(),
        data_destination_seed: Some(DataManagerDestination::Gateway),
    };
    let plan = plan_window_spawn(seeded, DESKTOP, 5, TRADE, None);
    let win = plan.window.expect("Data Manager window");
    assert_eq!(plan.data_destination_seed, Some((win.id, DataManagerDestination::Gateway)));

    let unseeded = WindowSpawnRequest::Kind {
        kind: WindowKind::Data,
        polymarket_seed_token: String::new(),
        data_destination_seed: None,
    };
    let plan = plan_window_spawn(unseeded, DESKTOP, 5, TRADE, None);
    assert_eq!(plan.data_destination_seed, None, "no seed requested, none carried out");

    for kind in WindowKind::ALL {
        if kind == WindowKind::Data {
            continue;
        }
        let req = WindowSpawnRequest::Kind {
            kind,
            polymarket_seed_token: String::new(),
            data_destination_seed: Some(DataManagerDestination::Gateway),
        };
        let plan = plan_window_spawn(req, DESKTOP, 5, TRADE, None);
        assert_eq!(plan.data_destination_seed, None, "{kind:?} must ignore the Data Manager seed");
    }
}

/// The cockpit window carries the seed it was handed — a real token verbatim, else the
/// placeholder — and the seed is per-KIND: a non-cockpit launcher ignores it, so no other window
/// can come up subscribed to a Polymarket token.
#[test]
fn the_cockpit_carries_its_seed_token_and_no_other_kind_reads_it() {
    let ph_request = WindowSpawnRequest::Kind {
        kind: WindowKind::Polymarket,
        polymarket_seed_token: POLYMARKET_PLACEHOLDER_TOKEN.to_string(),
        data_destination_seed: None,
    };
    let placeholder = plan_window_spawn(ph_request, DESKTOP, 2, TRADE, None);
    let win = placeholder.window.expect("cockpit window");
    assert_eq!(win.symbol, POLYMARKET_PLACEHOLDER_TOKEN);
    assert_eq!(win.title, "Polymarket · Cockpit");

    let seeded_request = WindowSpawnRequest::Kind {
        kind: WindowKind::Polymarket,
        polymarket_seed_token: "0xfeed".to_string(),
        data_destination_seed: None,
    };
    let seeded = plan_window_spawn(seeded_request, DESKTOP, 2, TRADE, None);
    assert_eq!(seeded.window.expect("cockpit window").symbol, "0xfeed");

    for kind in WindowKind::ALL {
        if kind == WindowKind::Polymarket {
            continue;
        }
        let req = WindowSpawnRequest::Kind {
            kind,
            polymarket_seed_token: POLYMARKET_PLACEHOLDER_TOKEN.to_string(),
            data_destination_seed: None,
        };
        let plan = plan_window_spawn(req, DESKTOP, 2, TRADE, None);
        let seeded = plan.window.is_some_and(|w| w.symbol == POLYMARKET_PLACEHOLDER_TOKEN);
        assert!(!seeded, "{kind:?} must ignore the cockpit seed");
    }
}

/// The Trade launcher opens at the size its caller measured with no instrument yet: the glue seeds
/// the last-used instrument, or the backend's primary market, on the first frame (spec §3.11). It
/// asks for no bar feed (Ruling R6) and no depth stream — the window loop requests depth, every
/// frame, once the window has a symbol.
#[test]
fn a_trade_launcher_opens_at_the_size_it_was_given_with_no_instrument_yet() {
    let req = WindowSpawnRequest::Kind {
        kind: WindowKind::Trade,
        polymarket_seed_token: String::new(),
        data_destination_seed: None,
    };
    let plan = plan_window_spawn(req, DESKTOP, 1, TRADE, None);
    let win = plan.window.expect("Trade window");
    assert_eq!(win.size, TRADE);
    assert_eq!(win.id, egui::Id::unique("trade-1"));
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
    use vike_ui_theme::appearance::{ThemeAppearance, install};
    use vike_ui_theme::metrics::ThemeDensity;
    let open = |density| {
        let context = egui::Context::default();
        install(&context, &ThemeAppearance { density, ..ThemeAppearance::default() });
        let mut size = egui::Vec2::ZERO;
        let out = context.run_ui(egui::RawInput::default(), |ui| size = trade_open_size(ui.ctx()));
        out.drop_without_applying_deltas();
        size
    };
    assert_eq!(open(ThemeDensity::Compact), egui::vec2(626.0, 560.0), "Compact: the spec's size");
    let loose = open(ThemeDensity::Comfortable);
    assert_eq!(loose.x, 626.0, "the width never moves with the density");
    assert!(loose.y > 560.0 + 61.0, "Comfortable: more than TP/SL's 61 pt: {loose:?}");
}

/// Stored ▸ Open in chart carries its venue into the title and the venue-namespaced feed key
/// (`WindowState::new` titles a window as Binance, so the venue is set, THEN retitled). A tick-only series
/// (`interval: None`) falls back to the venue's 1m klines.
#[test]
fn a_stored_open_carries_its_venue_into_the_window_title_and_the_feed() {
    let req = WindowSpawnRequest::StoredOpen {
        venue: "okx".to_string(),
        symbol: "BTC-USDT".to_string(),
        interval: None,
    };
    let plan = plan_window_spawn(req, DESKTOP, 0, TRADE, None);
    let win = plan.window.expect("stored open spawns a chart");
    assert_eq!(win.venue, "okx");
    assert_eq!(win.interval, "1m", "a tick-only series has no timeframe of its own");
    assert_eq!(win.title, "OKX BTC-USDT · 1m", "retitle() ran AFTER the venue was assigned");
    assert_eq!(win.key(), "okx:BTC-USDT@1m");
    let f = plan.ensure_feed.expect("the reopened series' feed");
    assert_eq!(series_key(&f.venue, &f.symbol, &f.interval), win.key());

    // …and a Binance stored open is spelled like every other venue's: title and key name it.
    let plain_request = WindowSpawnRequest::StoredOpen {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: Some("5m".to_string()),
    };
    let plain = plan_window_spawn(plain_request, DESKTOP, 0, TRADE, None)
        .window
        .expect("stored open spawns a chart");
    assert_eq!(plain.title, "BINANCE BTCUSDT · 5m");
    assert_eq!(plain.key(), "binance:BTCUSDT@5m");
}

/// **The flipped pin.** A title-bar clone used to copy its source's symbol and interval but NOT
/// its venue, so cloning an OKX chart opened on Binance under the un-namespaced key, subscribed
/// by nobody. A chart has no default venue to fall back to any more (the owner's ruling of
/// 2026-11-13), so the clone keeps all three: its key IS the series its source ensured, which
/// is why it needs no feed of its own.
#[test]
fn a_cloned_window_keeps_its_source_series_venue_included() {
    let req = WindowSpawnRequest::CloneWindow {
        venue: "okx".to_string(),
        symbol: "BTC-USDT".to_string(),
        interval: "5m".to_string(),
    };
    let plan = plan_window_spawn(req, DESKTOP, 0, TRADE, None);
    let win = plan.window.expect("clone spawns a chart");
    assert_eq!((win.venue.as_str(), win.symbol.as_str()), ("okx", "BTC-USDT"));
    assert_eq!(win.interval, "5m");
    assert!(plan.ensure_feed.is_none(), "a clone subscribes nothing of its own");
    assert_eq!(win.key(), series_key("okx", "BTC-USDT", "5m"), "the source's own key");
    assert_eq!(win.title, "OKX BTC-USDT · 5m", "the venue prefix goes with the venue");
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
    let at = |req: WindowSpawnRequest| {
        plan_window_spawn(req, DESKTOP, 3, TRADE, None).window.expect("a window")
    };
    let launch = |k: WindowKind| WindowSpawnRequest::Kind {
        kind: k,
        polymarket_seed_token: String::new(),
        data_destination_seed: None,
    };

    let palette = at(WindowSpawnRequest::Palette { raw: "btc".to_string() });
    assert_eq!(palette.screen_position, egui::pos2(146.0, 131.0));
    assert_eq!(palette.size, egui::vec2(640.0, 420.0));
    assert_eq!(palette.id, egui::Id::unique("chart-3"));

    let new_window = at(WindowSpawnRequest::NewWindowChart);
    assert_eq!(
        new_window.screen_position,
        egui::pos2(136.0, 121.0),
        "PINNED divergence: this site alone cascades from (40, 30)"
    );
    assert_eq!(new_window.size, egui::vec2(700.0, 440.0));

    let chart = at(launch(WindowKind::Chart));
    assert_eq!(
        (chart.screen_position, chart.size),
        (egui::pos2(146.0, 131.0), egui::vec2(640.0, 420.0))
    );

    let d = at(launch(WindowKind::Trade));
    assert_eq!((d.screen_position, d.size), (egui::pos2(146.0, 131.0), TRADE));
    assert_eq!(d.id, egui::Id::unique("trade-3"));

    let p = at(launch(WindowKind::Polymarket));
    assert_eq!((p.screen_position, p.size), (egui::pos2(146.0, 131.0), egui::vec2(340.0, 640.0)));
    assert_eq!(p.id, egui::Id::unique("poly-3"));

    // Every tool kind opens at its own row of `tool_window_size` (measured 2026-11-16; News and the
    // Calendar are rulings), all from the same slot and under the same `tool-{n}` id.
    for (kind, size) in [
        (WindowKind::Account, egui::vec2(730.0, 750.0)),
        (WindowKind::Options, egui::vec2(1290.0, 800.0)),
        (WindowKind::News, egui::vec2(1200.0, 800.0)),
        (WindowKind::Calendar, egui::vec2(960.0, 640.0)),
        (WindowKind::Data, egui::vec2(1180.0, 560.0)),
        (WindowKind::Studio, egui::vec2(560.0, 440.0)),
        (WindowKind::Settings, egui::vec2(780.0, 820.0)),
    ] {
        let t = at(launch(kind));
        assert_eq!((t.screen_position, t.size), (egui::pos2(146.0, 131.0), size), "{kind:?}");
        assert_eq!(t.id, egui::Id::unique("tool-3"), "{kind:?}");
    }

    let c = at(WindowSpawnRequest::CloneWindow {
        venue: "okx".to_string(),
        symbol: "BTC-USDT".to_string(),
        interval: "1m".to_string(),
    });
    assert_eq!((c.screen_position, c.size), (egui::pos2(146.0, 131.0), egui::vec2(620.0, 380.0)));

    let test_symbol = at(WindowSpawnRequest::TestSymbol { series: last() });
    assert_eq!(
        (test_symbol.screen_position, test_symbol.size),
        (egui::pos2(146.0, 131.0), egui::vec2(700.0, 440.0))
    );

    // Eight slots, then the walk WRAPS: slot 8 lands exactly on slot 0. Today's behaviour —
    // the ids still differ, so egui keeps the two windows' state apart and either can be
    // dragged off the other.
    let slot0 = plan_window_spawn(WindowSpawnRequest::NewWindowChart, DESKTOP, 0, TRADE, None)
        .window
        .expect("a window");
    let slot8 = plan_window_spawn(WindowSpawnRequest::NewWindowChart, DESKTOP, 8, TRADE, None)
        .window
        .expect("a window");
    assert_eq!(
        slot0.screen_position, slot8.screen_position,
        "PINNED: the cascade wraps every 8 windows"
    );
    assert_ne!(slot0.id, slot8.id);
}

/// Every tool kind's opening size is at least the plain tool window and at most 80% of a 1920 x
/// 1080 screen's arena (the screen minus the caption and the status bar) on each axis, so a window
/// opened on a common screen never fills it.
#[test]
fn every_tool_window_size_is_between_the_floor_and_most_of_a_common_screen() {
    use vike_ui_theme::value::{caption, status_bar};
    let arena =
        egui::vec2(1920.0, 1080.0 - caption::CAPTION_HEIGHT - status_bar::STATUS_BAR_HEIGHT);
    let fills = |k: &WindowKind| {
        matches!(k, WindowKind::Chart | WindowKind::Trade | WindowKind::Polymarket)
    };
    for kind in WindowKind::ALL.into_iter().filter(|k| !fills(k)) {
        let size = tool_window_size(kind);
        assert!(size.x >= workspace::TOOL_WINDOW_SIZE.x && size.y >= workspace::TOOL_WINDOW_SIZE.y);
        assert!(size.x <= 0.8 * arena.x && size.y <= 0.8 * arena.y, "{kind:?}: {size:?}");
    }
}

/// **The Greeks launcher opens the Options window on its Portfolio Greeks tab** — the Greeks window
/// became that tab (2026-11-17). The window is an ordinary Options window (its kind, its size, the
/// launcher's slot and a `tool-{n}` id), and the tab rides out as `WindowSpawnPlan::options_tab_seed`
/// paired with that window's own id, which `WindowSpawnPlan::seed_window_views` (called by
/// `App::apply_spawn`) writes into the window's `WindowView` before its first frame. Every other launcher, the Options one included, carries no seed, so its
/// Options window opens on the Chain.
#[test]
fn the_greeks_launcher_opens_the_options_window_on_its_portfolio_greeks_tab() {
    let plan = plan_window_spawn(
        WindowSpawnTarget::PortfolioGreeks.request(None),
        DESKTOP,
        3,
        TRADE,
        None,
    );
    let mut views = std::collections::HashMap::new();
    plan.seed_window_views(&mut views);
    let win = plan.window.expect("the Greeks launcher opens a window");
    let tab = views.get(&win.id).map(|v: &crate::ui::windows::WindowView| v.options_tab);
    assert_eq!(tab, Some(OptionsTab::PortfolioGreeks), "the window's view opens on the tab");
    assert_eq!(win.kind, WindowKind::Options);
    assert_eq!(
        (win.screen_position, win.size),
        (egui::pos2(146.0, 131.0), tool_window_size(WindowKind::Options)),
        "where and as large as the Options launcher's window"
    );
    assert_eq!(win.id, egui::Id::unique("tool-3"));
    assert_eq!(plan.options_tab_seed, Some((win.id, OptionsTab::PortfolioGreeks)));
    assert_eq!((plan.next_window_number, plan.ensure_feed.is_none()), (4, true));

    for kind in WindowKind::ALL {
        let plan = plan_window_spawn(
            WindowSpawnTarget::Window(kind).request(None),
            DESKTOP,
            3,
            TRADE,
            None,
        );
        assert_eq!(plan.options_tab_seed, None, "{kind:?}: no tab seed");
        assert_eq!(plan.window.map(|w| w.kind), Some(kind), "{kind:?}: its own kind");
    }
    // An unseeded Options window opens on the Chain.
    assert_eq!(crate::ui::windows::WindowView::default().options_tab, OptionsTab::Chain);
}

/// A launcher's cockpit token is read for the Polymarket launcher alone, through the ladder every
/// cockpit seeding path shares (trimmed; blank or absent is the placeholder).
#[test]
fn a_launcher_reads_the_cockpit_token_for_the_polymarket_window_alone() {
    let token = |o: WindowSpawnTarget, env: Option<&str>| match o.request(env) {
        WindowSpawnRequest::Kind { polymarket_seed_token, data_destination_seed: None, .. } => {
            polymarket_seed_token
        }
        other => panic!("{o:?}: {other:?}"),
    };
    let polymarket = WindowSpawnTarget::Window(WindowKind::Polymarket);
    assert_eq!(token(polymarket, Some(" 0xfeed ")), "0xfeed");
    assert_eq!(token(polymarket, None), POLYMARKET_PLACEHOLDER_TOKEN);
    assert_eq!(token(polymarket, Some("  ")), POLYMARKET_PLACEHOLDER_TOKEN);
    for kind in WindowKind::ALL.into_iter().filter(|k| *k != WindowKind::Polymarket) {
        assert_eq!(token(WindowSpawnTarget::Window(kind), Some("0xfeed")), "", "{kind:?}");
    }
    assert_eq!(
        WindowSpawnTarget::PortfolioGreeks.request(Some("0xfeed")),
        WindowSpawnRequest::PortfolioGreeks
    );
}

/// **The Studio opens once and comes to the front** (the live desktop held three, each opened behind a
/// maximized chart): a second Studio request brings the one there is back — closed or minimized —
/// over the maximized chart, which stays maximized (the owner's pass of 2026-11-21: it was restored);
/// a request for any other kind is left to spawn.
#[test]
fn a_second_studio_request_brings_the_one_there_is_back_over_a_maximized_chart() {
    let studio_req = || WindowSpawnTarget::Window(WindowKind::Studio).request(None);
    let area = egui::Rect::from_min_size(DESKTOP, egui::vec2(1600.0, 900.0));
    let mut chart = chart_on("chart-0", Some(&last()), area);
    crate::ui::window::maximize(&mut chart, area);
    chart.pending = None; // the frame consumed the forced geometry
    let maximized = (chart.screen_position, chart.size, chart.pre_maximize);
    let mut wins = vec![chart];
    assert_eq!(reopen_studio(&mut wins, &studio_req()), None, "no Studio yet: it spawns");

    let mut studio = WindowState::tool("tool-1", WindowKind::Studio, area);
    studio.open = false;
    studio.minimized = true;
    let id = studio.id;
    wins.push(studio);
    let chart_req = WindowSpawnTarget::Window(WindowKind::Chart).request(None);
    assert_eq!(reopen_studio(&mut wins, &chart_req), None, "another kind is not the Studio's");
    assert!(wins[0].maximized, "...and touches nothing");

    assert_eq!(reopen_studio(&mut wins, &studio_req()), Some(id));
    assert!(wins[1].open && !wins[1].minimized, "the Studio is back on the desktop");
    assert!(wins[0].maximized, "the chart stays maximized: the raised Studio is drawn over it");
    // `show_window` raises a window with pending geometry, which would put the chart over the Studio.
    assert_eq!(wins[0].pending, None, "the chart is not touched");
    assert_eq!((wins[0].screen_position, wins[0].size, wins[0].pre_maximize), maximized);
    assert_eq!(wins.len(), 2, "and no second Studio was made");
}
