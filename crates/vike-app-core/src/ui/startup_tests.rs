use super::*;
use crate::ui::tool_views::POLY_PLACEHOLDER_TOKEN;

fn area() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0))
}

fn env_with_shot_win(win: &str) -> StartupEnv {
    StartupEnv { shot_win: Some(win.to_string()), ..StartupEnv::default() }
}

/// A saved workspace carrying one chart window per `(venue, symbol, interval)`, built through
/// the REAL `persist::capture` so the fixture can never drift from the serde shape `plan`
/// consumes. The four globals are stamped on afterwards (`capture` deliberately leaves
/// `indicator_favs` empty — see its doc).
fn workspace_with(wins: &[(&str, &str, &str)]) -> persist::Workspace {
    let states: Vec<WinState> = wins
        .iter()
        .map(|(venue, symbol, interval)| {
            let mut w = WinState::new(
                symbol,
                symbol,
                interval,
                WinKind::Chart,
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0)),
            );
            w.venue = venue.to_string();
            w
        })
        .collect();
    let mut ws = persist::capture(&states, DisplayTz::parse("UTC"), 7.0, true);
    ws.indicator_favs = vec!["rsi".to_string()];
    ws
}

// ── the default layout ──────────────────────────────────────────────────────────────────

#[test]
fn no_knobs_and_no_workspace_opens_one_btcusdt_chart_with_three_indicators() {
    let l = plan(area(), &StartupEnv::default(), None);
    assert_eq!(l.wins.len(), 1);
    assert_eq!(l.wins[0].symbol, "BTCUSDT");
    assert_eq!(l.wins[0].interval, "1m");
    assert_eq!(l.wins[0].kind, WinKind::Chart);
    assert_eq!(l.wins[0].indicators.len(), 3, "sma + rsi + macd");
    assert!(l.display.is_none(), "nothing restored ⇒ App's own field defaults stand");
    assert_eq!(l.restored_windows, None);
    assert!(l.resolve_poly_token.is_none());
}

/// The default chart MUST bring its feed with it — the whole point of returning
/// `ensure_feeds` rather than letting the caller guess. A layout with a window and no feed
/// renders a permanently empty chart.
#[test]
fn the_default_chart_ensures_its_binance_1m_feed() {
    let l = plan(area(), &StartupEnv::default(), None);
    assert_eq!(
        l.ensure_feeds,
        vec![FeedSpec {
            venue: workspace::DEFAULT_VENUE.to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            asset_class: None,
        }]
    );
}

/// `App::new` created the default chart FLOATING at `area.min + (40,30)`, deliberately NOT
/// maximized to `area` — see the long comment at that site: maximizing here gave the startup
/// chart a stale `pre_max` and min/max/restore misbehaved.
#[test]
fn the_default_chart_is_floating_at_the_documented_offset_not_maximized() {
    let a = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(1920.0, 1080.0));
    let l = plan(a, &StartupEnv::default(), None);
    assert_eq!(l.wins[0].pos, a.min + egui::vec2(40.0, 30.0));
    assert_eq!(l.wins[0].size, egui::vec2(700.0, 440.0));
    assert_ne!(l.wins[0].size, a.size(), "must not be maximized to the construction-time area");
    assert!(!l.wins[0].maximized, "a stale pre_max is what min/max/restore used to break on");
}

/// An unrecognized `VIKE_SHOT_WIN` must fall through to the default chart, not open nothing.
#[test]
fn an_unrecognized_shot_win_falls_through_to_the_default_chart() {
    let l = plan(area(), &env_with_shot_win("nope"), None);
    assert_eq!(l.wins.len(), 1);
    assert_eq!(l.wins[0].kind, WinKind::Chart);
    assert_eq!(l.ensure_feeds.len(), 1);
}

// ── the saved-workspace restore ─────────────────────────────────────────────────────────

#[test]
fn a_restored_workspace_carries_its_four_global_display_settings() {
    let l = plan(area(), &StartupEnv::default(), Some(workspace_with(&[("okx", "ETHUSDT", "5m")])));
    let d = l.display.expect("a restored workspace always yields display settings");
    assert_eq!(d.display_tz, DisplayTz::parse("UTC"));
    assert_eq!(d.of_backfill_hours, 7.0);
    assert!(d.gpu_render);
    assert_eq!(d.indicator_favs, vec!["rsi".to_string()]);
}

/// The bug class this move exists to close: a restored window must ensure a feed **on its own
/// venue**, not on the default one. Routing every restored chart to binance is exactly the
/// venue-discarding shape `core_sync`'s bug 1 had.
#[test]
fn restored_windows_ensure_feeds_on_their_own_venues_not_the_default_one() {
    let ws = workspace_with(&[
        ("okx", "ETHUSDT", "5m"),
        ("bybit", "SOLUSDT", "1m"),
        ("binance", "BTCUSDT", "1h"),
    ]);
    let l = plan(area(), &StartupEnv::default(), Some(ws));
    let venues: Vec<&str> = l.ensure_feeds.iter().map(|f| f.venue.as_str()).collect();
    assert_eq!(venues, vec!["okx", "bybit", "binance"]);
    let symbols: Vec<&str> = l.ensure_feeds.iter().map(|f| f.symbol.as_str()).collect();
    assert_eq!(symbols, vec!["ETHUSDT", "SOLUSDT", "BTCUSDT"]);
}

/// One feed per restored window, and the count reported for the log line must match — a
/// silently-short `ensure_feeds` is a chart that never ticks.
#[test]
fn every_restored_window_gets_exactly_one_feed_and_the_count_is_reported() {
    let ws = workspace_with(&[("okx", "ETHUSDT", "5m"), ("bybit", "SOLUSDT", "1m")]);
    let l = plan(area(), &StartupEnv::default(), Some(ws));
    assert_eq!(l.wins.len(), 2);
    assert_eq!(l.ensure_feeds.len(), 2);
    assert_eq!(l.restored_windows, Some(2));
}

/// A restored workspace must NOT also open the default BTCUSDT chart, and must not ensure a
/// binance feed nobody asked for.
#[test]
fn a_restored_workspace_suppresses_the_default_chart_entirely() {
    let l = plan(area(), &StartupEnv::default(), Some(workspace_with(&[("okx", "ETHUSDT", "5m")])));
    assert_eq!(l.wins.len(), 1);
    assert_eq!(l.wins[0].symbol, "ETHUSDT");
    assert_eq!(l.ensure_feeds.len(), 1);
}

/// An EMPTY saved workspace still counts as restored: it must open nothing, not silently fall
/// back to the default chart (that would resurrect windows a user deliberately closed).
#[test]
fn an_empty_restored_workspace_opens_no_windows_rather_than_the_default_chart() {
    let l = plan(area(), &StartupEnv::default(), Some(workspace_with(&[])));
    assert!(l.wins.is_empty());
    assert!(l.ensure_feeds.is_empty());
    assert_eq!(l.restored_windows, Some(0));
    assert!(l.display.is_some(), "its globals still apply");
}

/// `VIKE_SHOT` suppresses the restore at the `restored_workspace` boundary, so `plan` sees
/// `None` — but the QA window arms must still beat the restore when both are somehow present.
#[test]
fn a_restored_workspace_outranks_every_shot_win_arm() {
    let env = env_with_shot_win("studio");
    let l = plan(area(), &env, Some(workspace_with(&[("okx", "ETHUSDT", "5m")])));
    assert_eq!(l.wins[0].kind, WinKind::Chart, "restore is the first branch");
}

// ── the VIKE_SHOT_WIN QA arms ───────────────────────────────────────────────────────────

/// The HAND-WRITTEN arms, one row each. The Data Manager's are deliberately absent: they are
/// no longer arms, and `the_data_manager_arms_select_the_destination_they_name` iterates the
/// real roster rather than a copy of it that could fall behind.
#[test]
fn each_shot_win_value_opens_exactly_its_own_tool_window_and_no_feed() {
    for (val, kind) in [
        ("studio", WinKind::Studio),
        ("connections", WinKind::Connections),
        ("connections-account", WinKind::Connections),
        ("polymarket", WinKind::Polymarket),
        ("trade", WinKind::Trade),
        ("account", WinKind::Account),
        (DATA_SHOT_WIN_ALIAS, WinKind::Data),
        ("options", WinKind::Options),
        ("tearsheet", WinKind::Tearsheet),
        ("settings", WinKind::Settings),
    ] {
        let l = plan(area(), &env_with_shot_win(val), None);
        assert_eq!(l.wins.len(), 1, "{val}");
        assert_eq!(l.wins[0].kind, kind, "{val}");
        assert!(l.ensure_feeds.is_empty(), "{val}: tool windows open no kline feed");
        assert_eq!(l.restored_windows, None, "{val}");
    }
}

/// **Every** rail destination is reachable, and each one opens on the destination it NAMES.
///
/// ⚠ Neither test above can see this, and that is the whole reason this one exists: both assert
/// the window KIND, and every Data-Manager value opens the SAME kind. An arm that pointed the
/// Venues capture at the stored grid would pass them both and silently put the wrong pane in
/// the contact sheet — a capture that ran, produced a file, and proved nothing about the
/// surface it claimed to show.
///
/// ⚠ This doc used to add that "`tool_views::data`'s `debug_assert` guards the index from the
/// other side". It did not: that assert ran in no lane (see
/// [`crate::ui::tool_views::data_rail`]'s module doc), so this test was the ONLY guard, not one of
/// two. It still is — the other side is a type now rather than a second check.
///
/// ⚠ **It iterates the real `DataDest::ALL`, and that is what makes it a COLLISION gate too.**
/// [`data_shot_dest`] is the last arm in [`plan`]'s chain, so a rail label that folded to a
/// shipped value (`options`, `trade`, `studio`) would leave its destination unreachable rather
/// than hijacking the shipped capture — a safe failure, and an invisible one. Here it is loud:
/// the shadowed destination arrives with `data_dest == None` and fails by name. A hand-written
/// twelve-row list would have proved the twelve rows somebody remembered.
#[test]
fn the_data_manager_arms_select_the_destination_they_name() {
    for want in DataDest::ALL {
        let val = shot_key(want.label());
        let l = plan(area(), &env_with_shot_win(&val), None);
        assert_eq!(l.wins.len(), 1, "{val}");
        assert_eq!(l.wins[0].kind, WinKind::Data, "{val}: shadowed by an earlier arm");
        let (id, dest) = l.data_dest.unwrap_or_else(|| panic!("{val}: no destination selected"));
        assert_eq!(dest, want, "{val}: the arm selected a destination it does not name");
        assert_eq!(
            id, l.wins[0].id,
            "{val}: the selection must bind to the window this arm opened"
        );
        assert!(l.ensure_feeds.is_empty(), "{val}: tool windows open no kline feed");
    }
}

/// The four spellings that shipped before the derivation still resolve exactly where they did.
///
/// ⚠ **Three of them survive by DERIVATION and one by alias, and the difference is the point.**
/// `venues` / `instruments` / `overview` are what [`shot_key`] folds their destinations' own
/// labels to, so they cost nothing and would break only under a rail RENAME —
/// which this test is what catches. [`DATA_SHOT_WIN_ALIAS`] is the one value that names no
/// label at all and exists purely so `scripts/qa_shots.sh`'s `06-data-manager` keeps capturing
/// the stored grid.
///
/// The failure this guards is silent by construction: a broken spelling still produces a PNG,
/// still lands in the sheet under the destination's filename, and shows a BTCUSDT chart.
#[test]
fn the_shipped_spellings_still_resolve_where_they_always_did() {
    for (val, want) in [
        (DATA_SHOT_WIN_ALIAS, DATA_SHOT_DEST),
        ("venues", DataDest::VenueArming),
        ("overview", DataDest::Overview),
        ("instruments", DataDest::Instruments),
    ] {
        assert_eq!(
            data_shot_dest(val),
            Some(want),
            "{val}: a spelling `scripts/qa_shots.sh` and the operator's notes both use"
        );
        let l = plan(area(), &env_with_shot_win(val), None);
        assert_eq!(l.data_dest.map(|(_, d)| d), Some(want), "{val}: through the real plan");
    }
}

/// Two destinations that folded to one spelling would make the second unreachable — `find`
/// returns the FIRST match — and the reader of `DataDest::ALL` would have no way to see it.
#[test]
fn every_destination_folds_to_its_own_distinct_spelling() {
    let mut seen: Vec<String> = Vec::new();
    for d in DataDest::ALL {
        let key = shot_key(d.label());
        assert!(!key.is_empty(), "{d:?}: an empty label is a destination nobody can name");
        assert!(!seen.contains(&key), "{d:?} folds to `{key}`, which another destination owns");
        seen.push(key);
    }
}

/// The operator types the rail label they are looking at, in whichever shell-safe shape they
/// reach for, and every shape reaches the same screen. A value naming no destination resolves
/// to `None` — it must never be rounded to the nearest one, because the capture that
/// destination produces is indistinguishable from the one it was meant to produce.
#[test]
fn the_spelling_is_forgiving_about_case_spaces_and_underscores() {
    for val in ["all-series", "all_series", "All series", "  ALL-SERIES  "] {
        assert_eq!(data_shot_dest(val), Some(DataDest::AllSeries), "{val}");
    }
    for val in ["", "   ", "nope", "all seires", "series", "all-series-2"] {
        assert_eq!(data_shot_dest(val), None, "{val}: a near-miss must not resolve");
    }
}

/// A value that names a NON-Data window must not be claimed by the derived arm — it names no
/// destination, so the hand-written arm above it is the one that answers.
#[test]
fn the_derived_arm_claims_no_hand_written_window_value() {
    for val in [
        "studio",
        "connections",
        "connections-account",
        "polymarket",
        "trade",
        "account",
        "options",
        "tearsheet",
    ] {
        assert_eq!(data_shot_dest(val), None, "{val}");
    }
}

/// The Connections arms are the SECOND pair that shares a window kind, and this is their
/// `the_data_manager_arms_select_the_sub_tab_they_name`.
///
/// ⚠ Neither iterating test above can tell them apart — both open `WinKind::Connections`, so an
/// arm that opened the panel on the DEFAULT account would pass them both while the capture it
/// produced showed nothing the plain `connections` shot does not already show. That capture
/// would still save a PNG, still be counted in the index, and still prove nothing about the
/// chip strip or the removal line it was added for.
///
/// The other half is the non-negotiable one: `connections` must select NOTHING, so the arm that
/// shipped opens exactly what it always opened.
#[test]
fn the_labelled_account_arm_selects_the_account_it_names() {
    let l = plan(area(), &env_with_shot_win("connections-account"), None);
    let (id, account) = l.connections_account.clone().expect("the arm exists to carry a selection");
    assert_eq!(account, shot_account_label(), "the arm selected an account it does not name");
    assert_eq!(
        id, l.wins[0].id,
        "the selection must bind to the window this arm opened, not to a stale id"
    );

    let plain = plan(area(), &env_with_shot_win("connections"), None);
    assert_eq!(
        plain.connections_account, None,
        "the connections arm that shipped must still open the DEFAULT account"
    );
}

/// The label the arm selects has to be one `AccountLabel::parse` accepts, because
/// [`shot_account_label`] cannot panic — it degrades to the default account, and a degraded
/// capture is indistinguishable from an ordinary `connections` shot.
///
/// Reddens on an edit to [`SHOT_ACCOUNT_LABEL`] that breaks the `[A-Z0-9]`/length/reserved
/// rules — the failure this test exists for is the SILENT one, where the arm still opens the
/// right window and the account dimension quietly leaves the frame.
#[test]
fn the_labelled_account_arm_names_a_valid_label() {
    assert_eq!(
        shot_account_label().text(),
        Some(SHOT_ACCOUNT_LABEL),
        "SHOT_ACCOUNT_LABEL must survive AccountLabel::parse — an unparseable one silently \
             captures the DEFAULT account under a labelled-account arm's name"
    );
}

/// ⚠ **The four Data-Manager rows are pinned BY HAND on purpose, although the size is derived
/// now.** [`data_shot_size`]'s rule reproduces them, and a rule that quietly stopped doing so
/// would change what four frames already on the contact sheet show — which is a comparison
/// nobody would notice losing. Deriving the expectation from the code under test is the
/// assertion-that-cannot-fail shape; these stay literals.
#[test]
fn the_shot_win_arms_keep_their_documented_sizes() {
    for (val, size) in [
        ("studio", egui::vec2(1520.0, 800.0)),
        ("connections", egui::vec2(760.0, 620.0)),
        // ⚠ EQUAL to `connections` on purpose, not by coincidence: the two captures sit side by
        // side on the contact sheet and the account dimension is meant to be the ONLY thing
        // that differs between them. A size change here silently costs that comparison.
        ("connections-account", egui::vec2(760.0, 620.0)),
        ("polymarket", egui::vec2(760.0, 620.0)),
        ("trade", egui::vec2(600.0, 560.0)),
        ("account", egui::vec2(900.0, 700.0)),
        (DATA_SHOT_WIN_ALIAS, egui::vec2(1200.0, 760.0)),
        ("overview", egui::vec2(1200.0, 760.0)),
        ("venues", egui::vec2(1200.0, 700.0)),
        ("instruments", egui::vec2(1200.0, 700.0)),
        ("options", egui::vec2(1200.0, 820.0)),
        ("settings", egui::vec2(760.0, 620.0)),
    ] {
        let l = plan(area(), &env_with_shot_win(val), None);
        assert_eq!(l.wins[0].size, size, "{val}");
        assert_eq!(l.wins[0].pos, area().min + egui::vec2(8.0, 8.0), "{val}");
    }
}

/// Every destination is capturable at a size a human can review, and at the ONE body width the
/// rail-plus-grid layout is designed for. The widths must not drift apart: two Data-Manager
/// frames of different widths on one sheet read as a layout change rather than a screen change.
#[test]
fn every_destination_opens_at_the_one_body_width() {
    for d in DataDest::ALL {
        let l = plan(area(), &env_with_shot_win(&shot_key(d.label())), None);
        let size = l.wins[0].size;
        assert_eq!(size.x, 1200.0, "{d:?}: the rail plus the grid wants one width");
        assert!(size.y >= 700.0, "{d:?}: {size:?} is too short to review a body in");
    }
}

// ── the cockpit token ladder ────────────────────────────────────────────────────────────

/// Unseeded ⇒ the window carries the placeholder AND asks for background Gamma resolution.
/// Losing the resolve request is what leaves the ladder stuck on STALE forever.
#[test]
fn an_unseeded_cockpit_gets_the_placeholder_and_requests_a_gamma_resolve() {
    let l = plan(area(), &env_with_shot_win("polymarket"), None);
    assert_eq!(l.wins[0].symbol, POLY_PLACEHOLDER_TOKEN);
    assert_eq!(l.resolve_poly_token, Some(l.wins[0].id));
}

/// A seeded token must be used verbatim and must NOT trigger a resolve that would overwrite it.
#[test]
fn a_seeded_cockpit_token_is_used_and_suppresses_the_gamma_resolve() {
    let env = StartupEnv {
        shot_win: Some("polymarket".to_string()),
        poly_cockpit_token: Some(
            "  71321045679252212594626385532706912750332728571942532289631379312455583992563  "
                .to_string(),
        ),
        ..StartupEnv::default()
    };
    let l = plan(area(), &env, None);
    assert_eq!(
        l.wins[0].symbol,
        "71321045679252212594626385532706912750332728571942532289631379312455583992563"
    );
    assert_eq!(l.resolve_poly_token, None);
}

#[test]
fn the_cockpit_window_is_titled_and_only_the_polymarket_arm_reads_the_token() {
    let l = plan(area(), &env_with_shot_win("polymarket"), None);
    assert_eq!(l.wins[0].title, "Polymarket · Cockpit");
    // The token knob must not leak into any other arm.
    let env = StartupEnv {
        shot_win: Some("trade".to_string()),
        poly_cockpit_token: Some("abc".to_string()),
        ..StartupEnv::default()
    };
    assert_eq!(plan(area(), &env, None).resolve_poly_token, None);
}

#[test]
fn poly_cockpit_seed_token_ladder() {
    assert_eq!(poly_cockpit_seed_token(None), POLY_PLACEHOLDER_TOKEN);
    assert_eq!(poly_cockpit_seed_token(Some("")), POLY_PLACEHOLDER_TOKEN);
    assert_eq!(poly_cockpit_seed_token(Some("   ")), POLY_PLACEHOLDER_TOKEN);
    assert_eq!(poly_cockpit_seed_token(Some(" 12345 ")), "12345");
}

// ── the VIKE_STYLE / VIKE_SCALE capture overrides ───────────────────────────────────────

#[test]
fn vike_style_forces_every_window_to_the_indexed_chart_style() {
    let ws = workspace_with(&[("okx", "ETHUSDT", "5m"), ("bybit", "SOLUSDT", "1m")]);
    let env = StartupEnv { style: Some("3".to_string()), ..StartupEnv::default() };
    let l = plan(area(), &env, Some(ws));
    assert_eq!(l.wins.len(), 2);
    for w in &l.wins {
        assert_eq!(w.style, ChartStyle::ALL[3]);
    }
}

#[test]
fn vike_scale_maps_only_zero_one_two() {
    for (raw, want) in [("0", ScaleMode::Linear), ("1", ScaleMode::Log), ("2", ScaleMode::Percent)]
    {
        let env = StartupEnv { scale: Some(raw.to_string()), ..StartupEnv::default() };
        assert_eq!(plan(area(), &env, None).wins[0].scale, want, "{raw}");
    }
}

/// Garbage in either knob must be INERT — an unparseable or out-of-range value leaves the
/// windows exactly as the layout built them, never panics and never picks arm 0.
#[test]
fn unparseable_or_out_of_range_style_and_scale_are_inert() {
    let baseline = plan(area(), &StartupEnv::default(), None);
    for env in [
        StartupEnv { style: Some("nope".to_string()), ..StartupEnv::default() },
        StartupEnv { style: Some("9999".to_string()), ..StartupEnv::default() },
        StartupEnv { style: Some("-1".to_string()), ..StartupEnv::default() },
        StartupEnv { scale: Some("nope".to_string()), ..StartupEnv::default() },
        StartupEnv { scale: Some("3".to_string()), ..StartupEnv::default() },
    ] {
        let l = plan(area(), &env, None);
        assert_eq!(l.wins[0].style, baseline.wins[0].style, "{env:?}");
        assert_eq!(l.wins[0].scale, baseline.wins[0].scale, "{env:?}");
    }
}

/// The overrides run LAST, so they reach the QA tool windows too — the same order `App::new`
/// applied them in (after every layout arm, before `next_win_n`).
#[test]
fn the_capture_overrides_apply_after_every_layout_arm_including_the_tool_windows() {
    let env = StartupEnv {
        shot_win: Some("trade".to_string()),
        style: Some("2".to_string()),
        scale: Some("1".to_string()),
        ..StartupEnv::default()
    };
    let l = plan(area(), &env, None);
    assert_eq!(l.wins[0].style, ChartStyle::ALL[2]);
    assert_eq!(l.wins[0].scale, ScaleMode::Log);
}

fn trade_seed_env(shot_win: Option<&str>) -> StartupEnv {
    StartupEnv { shot_win: shot_win.map(str::to_string), trade_seed: true, ..StartupEnv::default() }
}

/// The DEFAULT (unset) disposition, stated as a test because it is the constraint the whole
/// capture family is built on: a knob nobody set changes nothing.
#[test]
fn an_unset_trade_seed_adds_no_window_and_no_feed() {
    for shot_win in [None, Some("trade"), Some("tearsheet")] {
        let env = StartupEnv { shot_win: shot_win.map(str::to_string), ..Default::default() };
        let seeded = plan(area(), &env, None);
        assert!(
            !seeded.wins.iter().any(|w| w.id == egui::Id::new("capture-fill-clock")),
            "{shot_win:?}: an unset VIKE_TRADE_SEED must open no fill clock"
        );
    }
}

/// The fill clock's two load-bearing properties — see the arm's comment for why each one is
/// what makes the pose work rather than a detail.
#[test]
fn the_trade_seed_fill_clock_is_a_closed_chart_carrying_its_own_feed() {
    let l = plan(area(), &trade_seed_env(Some("trade")), None);
    let clock = l
        .wins
        .iter()
        .find(|w| w.id == egui::Id::new("capture-fill-clock"))
        .expect("VIKE_TRADE_SEED must open the fill clock");
    assert_eq!(clock.kind, WinKind::Chart, "only a Chart window holds a feed open");
    assert!(!clock.open, "an OPEN clock would tile the capture into two windows");
    assert_eq!(clock.interval, crate::ui::capture_seed::FILL_CLOCK_INTERVAL);
    assert_eq!(clock.symbol, crate::ui::capture_seed::SEED_SYMBOL);
    assert!(
        l.ensure_feeds.iter().any(|f| f.symbol == clock.symbol
            && f.interval == clock.interval
            && f.venue == clock.venue),
        "the clock window without its feed is a window that clocks nothing: {:?}",
        l.ensure_feeds
    );
}

/// The reaper is the reason `open` may be false: `live_window_keys` — the REAL one, driven
/// here rather than restated — must still count this window, or the feed it opened is torn
/// down on the next frame and the market order never fills.
#[test]
fn the_closed_fill_clock_still_counts_as_a_live_feed_consumer() {
    let l = plan(area(), &trade_seed_env(Some("trade")), None);
    let live = crate::ui::feed_lifecycle::live_window_keys(&l.wins);
    for f in &l.ensure_feeds {
        let key = crate::ui::workspace::series_key(&f.venue, &f.symbol, &f.interval);
        assert!(live.contains(&key), "{key} would be reaped the frame after it was opened");
    }
}

/// The pose guarantee: a seeded Trade capture must still be a ONE-open-window layout, because
/// that is what `initial_arrange` maximizes on. Asserted through the REAL open-count rule
/// rather than by eye.
#[test]
fn a_seeded_trade_capture_still_has_exactly_one_open_window() {
    let l = plan(area(), &trade_seed_env(Some("trade")), None);
    assert_eq!(l.wins.iter().filter(|w| w.open).count(), 1);
    assert_eq!(l.wins.len(), 2, "the Trade window plus the hidden clock");
}

/// The default layout already charts this series, so the clock must not add a second window
/// (and a second rail chip) for a feed that is already open.
#[test]
fn the_fill_clock_is_not_added_when_the_layout_already_charts_that_series() {
    let l = plan(area(), &trade_seed_env(None), None);
    assert_eq!(l.wins.len(), 1, "the default chart already IS the clock: {:?}", l.wins.len());
    assert_eq!(l.wins[0].kind, WinKind::Chart);
    assert!(l.wins[0].open);
}
