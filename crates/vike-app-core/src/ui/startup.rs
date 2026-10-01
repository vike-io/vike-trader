//! `startup` — the STARTUP LAYOUT decision: which windows a fresh `App` opens, which feeds it must
//! ensure for them, and which global display settings it starts with.
//!
//! Moved down out of `vike-app`'s `main.rs` (`App::new`'s tail) for the same reason as
//! [`crate::ui::core_sync`] and [`crate::ui::feed_lifecycle`] before it: that file was in **no
//! gate** — `justfile`'s `ci_crates` omitted `vike-app` and `xtask/src/ci/tables.rs` listed it in
//! `EXCLUDE_FROM_CI` — so nothing compiled, clippied or tested it. (The shell, `vike-desktop` now,
//! is still outside the roster; the `app-check` job compiles and clippy-gates it since, and runs
//! none of this tail.) This particular tail is a
//! **branch over several environment knobs** — `VIKE_SHOT`, one arm per `VIKE_SHOT_WIN` value,
//! `VIKE_STYLE`, `VIKE_SCALE`, `VIKE_POLY_COCKPIT_TOKEN`, `VIKE_TRADE_SEED` — plus a
//! saved-workspace restore, and every
//! arm decides both *what windows exist* and *what market-data subscriptions are opened for them* —
//! the same "a decision nothing checks silently opens the wrong socket / no socket" class the
//! feed-lifecycle move exists to close.
//!
//! ⚠ **This paragraph used to carry the arm COUNT** ("a six-way branch over five environment
//! knobs, `VIKE_SHOT_WIN` ×5 values"). It was wrong: six arms existed, because `data` joined
//! without anyone updating the prose. Numbers written beside a dispatch rot every time the
//! dispatch grows, so the count is gone rather than corrected — count the
//! `shot_win == Some(..)` arms below, or read `each_shot_win_value_opens_exactly_its_own_tool_window_and_no_feed`,
//! which iterates them.
//!
//! ⚠ **The Data Manager's half of that dispatch is DERIVED and is no longer an arm per screen.**
//! It was four hand-written arms (`data`, `venues`, `instruments`, `overview`) against a rail of
//! twelve destinations, so EIGHT screens were reachable by no capture at all — and since no CI
//! runner has a GPU, a screen no capture reaches is a screen no human has ever looked at. The four
//! arms had been added one at a time, and THREE of them carried a ⚠ note making the same argument
//! ("`data` hardcodes `DATA_SHOT_DEST`, so a destination with no arm of its own is reachable by no
//! capture"), which is the shape of a dispatch that wants to be a lookup. [`data_shot_dest`] is
//! that lookup: it folds the value through [`shot_key`] and matches it against
//! `crates/vike-app-core/src/ui/tool_views/data_rail.rs`'s `DataDest::ALL` by each destination's own
//! `DataDest::label`, so a THIRTEENTH destination is capturable the moment it joins the rail and
//! nothing here has to be edited for it.
//!
//! **The environment is not read here.** `vike-desktop`'s `main.rs` reads the five knobs and passes
//! them in as [`StartupEnv`] — the `Injected` shape `vike_ops::settings`' module doc names as the
//! target state ("libraries take configuration as parameters; only binaries read the process
//! environment"). That keeps every `VIKE_*` settings-registry row where it is (`vike-desktop` /
//! `Layer::Binary` — `vike-app` until the shell's rename re-keyed them) *and* makes this planner
//! testable without `env::set_var`, which is a
//! process-global mutation the workspace's grouped test binaries forbid.
//!
//! **Data in, decisions out.** [`plan`] is pure: it returns a [`StartupLayout`] describing the
//! windows, the feeds to ensure, and the optional background Gamma resolve — it never touches
//! `App`. The caller applies it, in the order documented on [`StartupLayout`], because a `fn new`'s
//! startup ORDER is load-bearing. The one impure sibling, [`restored_workspace`], is the file-I/O
//! half (the named-layout-over-`workspace.json` preference) and is deliberately kept out of [`plan`]
//! so the branch logic stays a pure function of its inputs.

use crate::ui::tool_views::DataDest;
use crate::ui::workspace::{self, WinKind, WinState, persist};
use vike_chart::{DisplayTz, ScaleMode, chart::ChartStyle};
use vike_model::account_keys::AccountLabel;

/// The startup knobs `vike-desktop`'s `main.rs` reads off the process environment and hands down.
///
/// Every field is the ALREADY-READ value, never a variable name — see the module doc for why the
/// read stays in the binary. `Default` is the "no knob set" configuration, which is exactly the
/// normal desktop launch (a restored workspace or the single default BTCUSDT chart).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StartupEnv {
    /// `VIKE_SHOT` is present (headless QA capture). Suppresses the saved-workspace restore so a
    /// capture always renders the deterministic default windows.
    pub shot_mode: bool,
    /// `VIKE_SHOT_WIN` — which single tool window a QA capture opens instead of the default chart.
    /// Anything unrecognized (including `None`) falls through to the default chart layout.
    ///
    /// ⚠ **The recognized values are not listed here.** This line carried a five-value list while
    /// [`plan`] had seven arms, which is the same rot the module doc removed a COUNT for one rung
    /// above: an arm joins the dispatch and no author has a reason to come back and edit a doc
    /// comment on a different item. `each_shot_win_value_opens_exactly_its_own_tool_window_and_no_feed`
    /// iterates them, so the values live in a test that goes red rather than in prose that goes
    /// stale.
    ///
    /// The Data Manager's values are not merely unlisted but UNLISTABLE from here: they are
    /// [`data_shot_dest`]'s fold over `DataDest::ALL`, which lives in another module and may grow
    /// without this one being touched. `just xtask`-style enumeration is not available for a GUI
    /// knob, so the operator-facing spelling is the rail label they can already see, lowercased
    /// with its spaces hyphenated — `All series` is `all-series`.
    pub shot_win: Option<String>,
    /// `VIKE_STYLE=<0..18>` — force every window to one [`ChartStyle`] for capture. An unparseable
    /// or out-of-range value is ignored.
    pub style: Option<String>,
    /// `VIKE_SCALE=<0|1|2>` — force every window's price-scale mode (Linear/Log/Percent). An
    /// unparseable or out-of-range value is ignored.
    pub scale: Option<String>,
    /// `VIKE_POLY_COCKPIT_TOKEN` — a real YES token-id seeded into the cockpit window. See
    /// [`poly_cockpit_seed_token`] for the trim / empty / placeholder ladder.
    pub poly_cockpit_token: Option<String>,
    /// `VIKE_TRADE_SEED` is present — the capture is going to inject the paper orders
    /// [`crate::ui::capture_seed::plan_trade_seed`] mints, so this layout must ALSO open the bar feed
    /// whose closes clock their fill.
    ///
    /// ⚠ **The feed is the whole of what this flag does here, and it is not optional plumbing.**
    /// The orders themselves are submitted from the frame loop, not from this planner. But a
    /// market order rests in the paper book until `ExecutionClient::on_bar`, that clock is driven
    /// by `Ingest::BarClose` from a live feed, and `VIKE_SHOT_WIN=trade` opens a Trade window and
    /// NO feed at all — so without this the position half of the pose could never arrive, however
    /// long the capture ran. [`plan`]'s own `trade_seed` arm carries the rest — above all why the
    /// window it adds is a CLOSED one, which is what makes the feed survive the reaper while
    /// staying invisible to the capture.
    pub trade_seed: bool,
}

/// One `(venue, symbol, interval, asset_class)` subscription the caller must ensure — the argument
/// list of `feed_lifecycle::ensure_feed_on`, captured as data so [`plan`] stays pure.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedSpec {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    pub asset_class: Option<vike_model::AssetClass>,
}

/// The four GLOBAL display settings a restored workspace carries. `None` on [`StartupLayout`] means
/// no workspace was restored, so the `App`'s own field initializers stand — NOT that these values
/// should be written as defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplaySettings {
    pub display_tz: DisplayTz,
    pub of_backfill_hours: f64,
    pub gpu_render: bool,
    pub indicator_favs: Vec<String>,
}

/// What a fresh `App` starts with — and, critically, **in what order the caller must apply it**.
///
/// The order below is the order `App::new` performed these steps before the move, and it is
/// reproduced exactly:
///
/// 1. [`display`](Self::display) — assign the four global settings (restored workspace only).
/// 2. [`ensure_feeds`](Self::ensure_feeds) — `ensure_feed_on` each spec. Both arms that ensure
///    anything did so BEFORE publishing their windows, so this stays ahead of step 4.
/// 3. [`restored_windows`](Self::restored_windows) — emit the `workspace restored (N windows)` log.
/// 4. [`wins`](Self::wins) — publish the window list.
/// 5. [`resolve_poly_token`](Self::resolve_poly_token) — spawn the background Gamma resolver, which
///    the cockpit arm did AFTER pushing its window.
/// 6. `next_win_n = wins.len()`.
///
/// `Default` only — `WinState` carries live egui dialog/pane state and derives nothing, so this
/// struct is neither `Clone` nor `Debug`. It is built once and consumed once.
#[derive(Default)]
pub struct StartupLayout {
    /// Global display settings from a restored workspace; `None` when nothing was restored.
    pub display: Option<DisplaySettings>,
    /// Feeds to `ensure_feed_on` before the windows are published.
    pub ensure_feeds: Vec<FeedSpec>,
    /// `Some(n)` iff a saved workspace was restored, carrying its window count for the log line.
    pub restored_windows: Option<usize>,
    /// The window list, after the `VIKE_STYLE` / `VIKE_SCALE` capture overrides.
    pub wins: Vec<WinState>,
    /// The cockpit window whose YES token still needs background Gamma resolution.
    pub resolve_poly_token: Option<egui::Id>,
    /// `Some((window, destination))` when a capture arm opened a Data Manager window on a rail
    /// destination other than its default. Carried out here rather than read from the environment
    /// inside the tool body: `ToolView` is created lazily by the window loop, so the plan cannot set
    /// it directly, and a library reading `VIKE_SHOT_WIN` a second time would be a second undeclared
    /// env read.
    ///
    /// ⚠ It is set for EVERY Data-Manager capture, including the one that lands on
    /// `DataDest::default()`. Seeding the default explicitly costs nothing and keeps
    /// `the_data_manager_arms_select_the_destination_they_name` able to iterate the whole rail —
    /// a destination that relied on "we set nothing and the default happens to be right" would be
    /// indistinguishable from an arm that silently failed to select anything.
    pub data_dest: Option<(egui::Id, DataDest)>,
    /// `Some((window, account))` when a capture arm opened a Connections window that must render a
    /// LABELLED account rather than the default one. Carried out here for exactly the reason
    /// [`Self::data_dest`] is — `ToolView` is created lazily by the window loop, so the plan
    /// cannot set it directly, and a library reading `VIKE_SHOT_WIN` a second time would be a
    /// second undeclared env read.
    ///
    /// ⚠ It is a ONE-SHOT: the caller seeds `ToolView::connections_account`, and the tool body
    /// `take`s it on the first frame that renders. Nothing re-applies it, so an operator's own chip
    /// click is never stomped by a knob they set at launch.
    pub connections_account: Option<(egui::Id, AccountLabel)>,
}

/// The Data Manager destination the `VIKE_SHOT_WIN=data` capture arm opens.
///
/// ⚠ This was `DATA_SUBTAB_STORED: usize = 5`, and its doc claimed the tool body `debug_assert`s the
/// index still points at `"Stored"`, "so a reordered tab list trips a test rather than silently
/// capturing the wrong pane". **No test ever ran either assert** — nothing in this crate's `tests/`
/// constructs a `ToolView` or calls the tool body, and `vike-desktop` is excluded from the CI
/// roster. The guarantee was prose. Naming a `DataDest` instead makes the wrong-pane capture a type
/// error rather than something a test was believed to catch.
///
/// It is now reachable under TWO spellings — this legacy alias and the destination's own derived
/// one (`all-series`) — and that is deliberate rather than tolerated: see [`DATA_SHOT_WIN_ALIAS`].
pub const DATA_SHOT_DEST: DataDest = DataDest::AllSeries;

/// The ONE `VIKE_SHOT_WIN` value that names a Data-Manager destination without spelling it.
///
/// ⚠ **It is kept for compatibility and must stay that way.** `scripts/qa_shots.sh` captures
/// `06-data-manager` with this value, and an operator's own notes use it; a rename here would not
/// fail anything — the capture would still run, still save a PNG and still be counted in the index
/// — it would simply fall through to the default BTCUSDT chart and put a chart on the contact
/// sheet under the Data Manager's name. That is the exact failure mode
/// [`crate::ui::tool_views::data_rail`]'s module doc records the old index-based dispatch having, so
/// the alias is pinned by `the_shipped_spellings_still_resolve_where_they_always_did` rather
/// than left to a reader's care.
///
/// The other three shipped spellings — `venues`, `instruments`, `overview` — need no alias: they
/// already ARE what [`shot_key`] derives from their destinations' own labels, which is why the
/// derivation could replace their arms without a compatibility table.
pub const DATA_SHOT_WIN_ALIAS: &str = "data";

/// Fold one `VIKE_SHOT_WIN` value — or one `DataDest::label` — to the spelling the lookup matches
/// on: trimmed, ASCII-lowercased, and every space or underscore hyphenated.
///
/// Both sides go through this, so the operator types what the rail SHOWS them (`All series`) in
/// whichever of the three obvious shell-safe shapes they reach for (`all-series`, `all_series`,
/// `"All series"`). A knob whose accepted spelling is a guess is a knob that silently falls
/// through to the default chart, and the fall-through is the one outcome a capture cannot
/// distinguish from an empty screen.
#[must_use]
pub fn shot_key(raw: &str) -> String {
    raw.trim()
        .chars()
        .map(|c| match c {
            ' ' | '_' => '-',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

/// Which Data Manager destination a `VIKE_SHOT_WIN` value opens, or `None` when it names no
/// destination (every non-Data arm, and every typo).
///
/// **DERIVED, not enumerated.** The roster is `DataDest::ALL` and the spelling is each
/// destination's own `DataDest::label` folded through [`shot_key`] — so this function has no list
/// in it to fall behind the rail, and a thirteenth destination is capturable the day it is added.
/// The rail's own footer makes the same promise from the other side ("A thirteenth destination is
/// one more row here"), and this is what keeps that true of the capture harness as well.
///
/// ⚠ **`label` is the right key and `icon` is not, even though both are per-destination.** The
/// label is what the operator READS off the rail, so the value they type is the thing they are
/// looking at; nothing else in the type is human-typable. The cost is real and accepted: renaming
/// a rail label renames a capture spelling, which is why [`DATA_SHOT_WIN_ALIAS`] exists at all and
/// why `scripts/qa_shots.sh`'s spellings are pinned by a test in this file rather than merely
/// derived.
#[must_use]
pub fn data_shot_dest(win: &str) -> Option<DataDest> {
    let key = shot_key(win);
    if key == shot_key(DATA_SHOT_WIN_ALIAS) {
        return Some(DATA_SHOT_DEST);
    }
    DataDest::ALL.into_iter().find(|d| shot_key(d.label()) == key)
}

/// The capture window's size for one destination.
///
/// ⚠ **Deliberately an EXHAUSTIVE `match` with no `_` arm**, which is the opposite choice from
/// [`data_shot_dest`] one item above and is the point of the split. Reachability is a property of
/// the roster and must never need an edit here; GEOMETRY is a judgement about what a given screen
/// renders, and a catch-all would silently hand a thirteenth destination whichever size happened
/// to be the fallback. A compile error is the cheap place to make that judgement.
///
/// Two sizes, and the rule is whether the body has a CEILING a reader can see the bottom of:
///
/// * `1200x760` — it does not. The stored grid and its two filtered siblings grow with the tape,
///   the Overview's tiles wrap against the body width, and the saved DataSets, the live feed list
///   and the session log are all as long as the session made them. A short window there reviews a
///   scrollbar rather than a layout.
/// * `1200x700` — it does: one row per roster venue, per mount, or per known provider. The list
///   ends, and the frame should show where.
///
/// The four sizes that shipped before the derivation (`data`/`overview` tall, `venues`/
/// `instruments` short) fall out of that rule unchanged, and
/// `the_shot_win_arms_keep_their_documented_sizes` still pins them by hand — a rule that quietly
/// stopped reproducing them would change what four existing contact-sheet frames show.
///
/// The WIDTH never varies: every destination shares one body width with the rail beside it
/// (`crates/vike-app-core/src/ui/tool_views/data_rail.rs`'s `RAIL_W`), and a narrower capture would
/// review a column layout nobody runs.
#[must_use]
fn data_shot_size(dest: DataDest) -> egui::Vec2 {
    match dest {
        DataDest::Overview
        | DataDest::AllSeries
        | DataDest::HasGaps
        | DataDest::Stale
        | DataDest::CachedFeeds
        | DataDest::ActivityLog
        | DataDest::DataSets => egui::vec2(1200.0, 760.0),
        DataDest::ByVenue
        | DataDest::Providers
        | DataDest::VenueArming
        | DataDest::Instruments
        | DataDest::Store => egui::vec2(1200.0, 700.0),
    }
}

/// The account label the `connections-account` capture arm selects — and therefore the label
/// `scripts/qa_shots.sh` must seed a credential into for that capture to show anything.
///
/// It is a CONSTANT rather than a knob because the two halves have to agree and only one of them
/// is Rust: the arm picks a label, the sheet seeds a store containing it. They are held equal by
/// `crates/vike-app-core/tests/qa_shot_account.rs`, which parses the script's own fixture heredoc
/// and folds it through the real `AccountGrids::from_vars`, asserting the label set that enumerator
/// derives is exactly the one this constant names — the sheet is the only place a human ever sees
/// this surface, and a label that drifted out of step would capture an account holding nothing,
/// which renders as the `(new)` chip and an all-absent grid.
///
/// ⚠ That test deliberately does NOT compose the key names through
/// `vike_model::account_keys::account_key`; doing so was measured and reverted, because calling the
/// key builders enrols that crate in the generated-key grid and reddens two `vike-ops` registry
/// gates. Its own module doc carries the measurement.
///
/// ⚠ Deliberately NOT a `VIKE_*` variable. A second env read inside a library is the thing
/// [`plan`]'s module doc exists to prevent, and injecting one through [`StartupEnv`] would buy a
/// `vike_ops::settings::SETTINGS` row for a value nobody but this script sets.
pub const SHOT_ACCOUNT_LABEL: &str = "QA";

/// [`SHOT_ACCOUNT_LABEL`] as the validated type the rest of the workspace addresses accounts with.
///
/// ⚠ It cannot panic, on purpose — this runs inside a GUI's construction, where an `expect` on a
/// capture-only constant would take the whole app down. An unparseable edit degrades to the DEFAULT
/// account instead, and `the_labelled_account_arm_names_a_valid_label` is what refuses one: the
/// degraded capture would otherwise look like an ordinary Connections shot and prove nothing.
#[must_use]
pub fn shot_account_label() -> AccountLabel {
    AccountLabel::parse(SHOT_ACCOUNT_LABEL).unwrap_or_default()
}

/// The initial cockpit token-id: the caller's `VIKE_POLY_COCKPIT_TOKEN` when set and non-empty
/// after trimming, else [`POLY_PLACEHOLDER_TOKEN`](crate::ui::tool_views::POLY_PLACEHOLDER_TOKEN),
/// which is what triggers background Gamma resolution.
pub fn poly_cockpit_seed_token(from_env: Option<&str>) -> String {
    from_env
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::ui::tool_views::POLY_PLACEHOLDER_TOKEN.to_string())
}

/// Load the workspace a fresh `App` should restore, or `None`.
///
/// Saved workspace wins over the default layout — but never under `VIKE_SHOT`, which needs the
/// deterministic default windows for QA captures. When a named layout was the last one explicitly
/// used (and still exists), it is preferred over the single default `workspace.json`; an install
/// with no named layouts is unaffected (`last_layout()` returns `None` → the plain `load()`
/// fallback runs).
///
/// File I/O, hence kept OUT of the pure [`plan`] below: pass the result in.
pub fn restored_workspace(shot_mode: bool) -> Option<persist::Workspace> {
    if shot_mode {
        None
    } else {
        persist::last_layout().and_then(|name| persist::load_layout(&name)).or_else(persist::load)
    }
}

/// Decide the whole startup layout. Pure: same inputs ⇒ same [`StartupLayout`].
///
/// `area` is the window's content rect at construction (`cc.egui_ctx.content_rect()`); `restored`
/// is [`restored_workspace`]'s result.
pub fn plan(
    area: egui::Rect,
    env: &StartupEnv,
    restored: Option<persist::Workspace>,
) -> StartupLayout {
    let mut out = StartupLayout::default();
    let shot_win = env.shot_win.as_deref();

    if let Some(ws) = restored {
        // Old files (no `display_tz`) and unknown/exotic zone names both resolve via
        // `DisplayTz::parse`'s own fallback — see its doc.
        out.display = Some(DisplaySettings {
            display_tz: DisplayTz::parse(&ws.display_tz),
            of_backfill_hours: ws.of_backfill_hours,
            gpu_render: ws.gpu_render,
            indicator_favs: ws.indicator_favs.clone(),
        });
        let wins = persist::apply(&ws);
        for w in &wins {
            out.ensure_feeds.push(FeedSpec {
                venue: w.venue.clone(),
                symbol: w.symbol.clone(),
                interval: w.interval.clone(),
                asset_class: w.asset_class,
            });
        }
        out.restored_windows = Some(wins.len());
        out.wins = wins;
    } else if shot_win == Some("studio") {
        // QA: VIKE_SHOT_WIN=studio opens a large Studio tool window instead of the default
        // chart, for headless VIKE_SHOT captures of the Studio (pair with vike-studio's
        // VIKE_STUDIO_TAB to select the tool tab) — same QA-hook family as VIKE_STYLE/
        // VIKE_SCALE below. The saved workspace is already bypassed under VIKE_SHOT.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), egui::vec2(1520.0, 800.0));
        out.wins.push(WinState::tool("tool-studio", WinKind::Studio, r));
    } else if shot_win == Some("connections") {
        // QA: VIKE_SHOT_WIN=connections opens the Connections tool window instead of the
        // default chart, for headless VIKE_SHOT captures of the per-venue Status column — same
        // QA-hook family as VIKE_SHOT_WIN=studio above. The saved workspace is already bypassed
        // under VIKE_SHOT.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), egui::vec2(760.0, 620.0));
        out.wins.push(WinState::tool("tool-connections", WinKind::Connections, r));
    } else if shot_win == Some("connections-account") {
        // QA: VIKE_SHOT_WIN=connections-account opens the SAME Connections window as the arm above,
        // with the labelled account [`SHOT_ACCOUNT_LABEL`] already selected in its chip strip.
        //
        // ⚠ It needs its OWN arm rather than a knob on `connections`, for the reason `venues` needs
        // one beside `data`: the selection is what is being captured, and nothing in the capture
        // harness could preselect an account before this existed. With a labelled account chosen,
        // `crates/vike-connections/src/view.rs`'s `account_strip` renders two things the headless
        // layout suites cannot judge — a WRAPPING removal-instruction line that embeds the full
        // credential-store PATH, and a `horizontal_wrapped` chip strip that grows with the account
        // count. Both are rasterized text whose failure mode is overflow or a clipped path, and
        // the contact sheet is the only place anyone looks at a rendered surface here.
        //
        // The rect is the `connections` arm's, DELIBERATELY: the two captures then differ in
        // exactly one thing, so a human can diff `05-connections` against `05b-connections-account`
        // by eye and every difference is the account dimension.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), egui::vec2(760.0, 620.0));
        let w = WinState::tool("tool-connections-account", WinKind::Connections, r);
        out.connections_account = Some((w.id, shot_account_label()));
        out.wins.push(w);
    } else if shot_win == Some("polymarket") {
        // QA: VIKE_SHOT_WIN=polymarket opens the Polymarket scalp-cockpit tool window instead of
        // the default chart, for headless VIKE_SHOT captures of the live probability ladder —
        // same QA-hook family as VIKE_SHOT_WIN=studio/connections above. Seed a real token with
        // VIKE_POLY_COCKPIT_TOKEN; the local process dialling Polymarket (a `vike-backend datahub`)
        // reads its own `venue.polymarket.*` rows for a live book through the Dublin proxy — this
        // GUI links no Polymarket bridge. The saved workspace is already bypassed under VIKE_SHOT.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), egui::vec2(760.0, 620.0));
        let mut ws = WinState::tool("tool-polymarket", WinKind::Polymarket, r);
        // Seed the YES-outcome token-id (VIKE_POLY_COCKPIT_TOKEN) so the window-loop's
        // `ensure_poly_book` actually subscribes its live book — WITHOUT this the symbol stays
        // the placeholder and the ladder never leaves STALE (mirrors the launcher open arm).
        ws.symbol = poly_cockpit_seed_token(env.poly_cockpit_token.as_deref());
        ws.title = "Polymarket · Cockpit".to_string();
        // When no explicit VIKE_POLY_COCKPIT_TOKEN is seeded, resolve a real market off-thread
        // (like the launcher arm) so a headless render shows the resolved MARKET NAME + its live
        // book, not just the placeholder — needs a working Gamma proxy for the resolve.
        let wid = ws.id;
        let needs_resolve = ws.symbol == crate::ui::tool_views::POLY_PLACEHOLDER_TOKEN;
        out.wins.push(ws);
        if needs_resolve {
            out.resolve_poly_token = Some(wid);
        }
    } else if shot_win == Some("trade") {
        // QA: VIKE_SHOT_WIN=trade opens the Trade tool window (live orders/positions/per-venue
        // P&L/recent events) instead of the default chart, for headless VIKE_SHOT captures —
        // same QA-hook family as VIKE_SHOT_WIN=studio/connections/polymarket above. This is the
        // window `--observe` mode is meant to be screenshotted in (charts stay empty on the
        // wire). Useful beyond observe mode, so it is added unconditionally. Saved workspace is
        // already bypassed under VIKE_SHOT.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), egui::vec2(900.0, 700.0));
        out.wins.push(WinState::tool("tool-trade", WinKind::Trade, r));
    } else if shot_win == Some("options") {
        // QA: VIKE_SHOT_WIN=options opens the Options tool window (the keyless Deribit option
        // chain — live quotes, no creds, not geo-blocked) instead of the default chart, for
        // headless VIKE_SHOT captures — same QA-hook family as VIKE_SHOT_WIN=trade above.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), egui::vec2(1200.0, 820.0));
        out.wins.push(WinState::tool("tool-options", WinKind::Options, r));
    } else if shot_win == Some("tearsheet") {
        // QA: VIKE_SHOT_WIN=tearsheet opens the Tearsheet tool window — the performance summary
        // read back from THIS session's command journal
        // (`crate::ui::tool_views::tearsheet::tearsheet_tool_content`) — instead of the default chart.
        //
        // ⚠ It needs its OWN arm for the reason `venues` needed one: until this existed the panel
        // was reachable only through `VIKE_TOOL=tearsheet`, and that is a DIFFERENT planner
        // ([`crate::ui::initial_arrange::plan_initial_arrange`]) which places every QA window at the
        // desktop origin at one generic 560x400 size. A tool window that small inside a capture
        // viewport renders as a postage stamp in a field of empty desktop, which is why
        // `.trader/shots/manifest.json`'s `grid-tearsheet.png` records "no VIKE_SHOT_WIN arm" as
        // half of its `capture_gap`. This arm is the half that closes.
        //
        // ⚠ The OTHER half is not a layout problem and this arm does not touch it: the panel
        // needs `VIKE_JOURNAL_DIR` (or a run profile) to render anything but its "Journaling is
        // off" empty state, and a journal with CONTENT needs orders — which is what
        // [`StartupEnv::trade_seed`] below is for. The two knobs are independent on purpose:
        // journaling is the operator's choice and this arm only decides which window opens.
        //
        // Sized like `connections`/`polymarket` — the body is a stat grid and a trade list, not a
        // wide table, so it does not want `data`'s 1200.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), egui::vec2(900.0, 760.0));
        out.wins.push(WinState::tool("tool-tearsheet", WinKind::Tearsheet, r));
    } else if shot_win == Some("settings") {
        // QA: VIKE_SHOT_WIN=settings opens the Settings window (its one section, Appearance) for
        // headless VIKE_SHOT captures, at whatever appearance the settings root holds — so a
        // capture per theme is one `vike-cli config set preferences.theme <word>` away.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), egui::vec2(760.0, 620.0));
        out.wins.push(WinState::tool("tool-settings", WinKind::Settings, r));
    } else if let Some(dest) = shot_win.and_then(data_shot_dest) {
        // QA: every Data Manager destination, in ONE arm — see [`data_shot_dest`] for the lookup
        // and the module doc for what it replaced. The four arms this supersedes are worth reading
        // in `git log`: each was added on its own, and three of them carried a ⚠ note making the
        // SAME argument — "a destination with no arm of its own is reachable by no capture at all"
        // — while eight destinations were still waiting their turn. An argument a dispatch keeps
        // restating per arm is a lookup asking to be written.
        //
        // The Data Manager is the window whose content comes entirely from a local store, so it is
        // the one worth capturing against a real tape: point `VIKE_HIST_STORE` at one. The saved
        // workspace is already bypassed under `VIKE_SHOT`.
        //
        // ⚠ **This arm is LAST on purpose, after every hand-written one.** The spellings it
        // recognises are derived from rail labels that another module owns and another agent may
        // edit, so a future label could collide with a shipped value — `options`, `trade`,
        // `studio`. Last means the shipped capture keeps working and the new destination is the
        // thing that goes missing; first would mean a rail rename silently repointing
        // `07-trade.png` at a data grid. Neither is acceptable in silence, which is why the
        // collision is ALSO a red test rather than only a safe ordering:
        // `the_data_manager_arms_select_the_destination_they_name` iterates the whole roster, so a
        // shadowed destination fails there by name.
        //
        // ⚠ The window id is derived from the DESTINATION, not from the value the operator typed,
        // so `data` and `all-series` are one window rather than two ids for one screen. Nothing
        // outside this file has ever named these ids (they were `tool-data` / `tool-venues` /
        // `tool-instruments` / `tool-overview`, referenced nowhere else in the tree), and nothing
        // persists them: `crates/vike-app-core/src/ui/workspace/persist.rs`'s `capture` filters to
        // `WinKind::Chart` and `crates/vike-desktop/src/main.rs`'s `persist_egui_memory` returns
        // `false`.
        let r = egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), data_shot_size(dest));
        let id_src = format!("tool-{}", shot_key(dest.label()));
        let w = WinState::tool(&id_src, WinKind::Data, r);
        out.data_dest = Some((w.id, dest));
        out.wins.push(w);
    } else {
        // ⚠ An unrecognised `VIKE_SHOT_WIN` falls through to the DEFAULT CHART, and it says so
        // out loud. The fall-through itself is load-bearing and pinned by
        // `an_unrecognized_shot_win_falls_through_to_the_default_chart`: a capture that opened
        // NOTHING would render an empty desktop, and an empty desktop is what a genuinely broken
        // window also looks like. But silence was the real defect — a typo'd destination
        // (`all_seires`, or a rail label renamed since the notes were written) produced a
        // perfectly good BTCUSDT chart under the destination's filename, and the contact sheet is
        // read by a human scanning for layout faults, not for identity ones. So a value that was
        // SET and matched nothing is warned about by name.
        //
        // Only `Some(..)` warns: an unset knob is the ordinary desktop launch and has nothing to
        // report. `tracing::warn!` rather than a field on [`StartupLayout`] — the caller is
        // `vike-desktop`'s `App::new`, which runs well after `vike_log::init` (`main` builds the
        // subscriber before `eframe::run_native`), and a field nobody reads is one more thing to
        // keep in step. [`plan`] stays pure in the sense its doc claims — same inputs, same
        // `StartupLayout` — because a diagnostic is not a decision.
        if let Some(win) = shot_win {
            tracing::warn!(
                "unrecognised VIKE_SHOT_WIN value {win:?} — opening the default chart instead. A \
                 Data Manager destination is spelled as its rail label, lowercased and hyphenated \
                 (Overview, All series -> all-series, Venues, ...)."
            );
        }
        // default layout: a single maximized chart (BTCUSDT 1m).
        out.ensure_feeds.push(FeedSpec {
            venue: workspace::DEFAULT_VENUE.to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            asset_class: None,
        });
        // Create it FLOATING (like a user "New chart window") and let the runtime
        // lone-window arrange maximize it with the REAL desktop. Do NOT maximize here
        // with `area`: at construction `area = content_rect()` still includes the
        // status-bar region (the real arena, `self.desktop = ui.max_rect()`, excluding
        // the bottom status bar, is only known during a frame). Maximizing to it gave the
        // startup chart a stale `pre_max` → min/max/restore misbehaved; a user-created
        // chart works because it's maximized later with the real desktop. Same path now.
        let w = WinState::new(
            "chart-0",
            "BTCUSDT",
            "1m",
            WinKind::Chart,
            egui::Rect::from_min_size(area.min + egui::vec2(40.0, 30.0), egui::vec2(700.0, 440.0)),
        );
        out.wins.push(w);
        // demo indicators (overlay SMA + two oscillator panes) — default layout only;
        // a restored workspace carries its own indicators.
        out.wins[0].add_indicator("sma", &[]);
        out.wins[0].add_indicator("rsi", &[]);
        out.wins[0].add_indicator("macd", &[]);
    }
    // QA: VIKE_STYLE=<0..18> forces every window to one chart style for capture.
    if let Some(s) = env.style.as_deref()
        && let Ok(idx) = s.parse::<usize>()
        && let Some(&st) = ChartStyle::ALL.get(idx)
    {
        for w in &mut out.wins {
            w.style = st;
        }
    }
    // QA: VIKE_SCALE=<0|1|2> forces every window's requested price-scale
    // mode (Linear/Log/Percent) for capture — chart-UX bundle T3, same
    // pattern as VIKE_STYLE above.
    if let Some(s) = env.scale.as_deref()
        && let Ok(idx) = s.parse::<usize>()
    {
        let sm = match idx {
            0 => Some(ScaleMode::Linear),
            1 => Some(ScaleMode::Log),
            2 => Some(ScaleMode::Percent),
            _ => None,
        };
        if let Some(sm) = sm {
            for w in &mut out.wins {
                w.scale = sm;
            }
        }
    }
    // QA: VIKE_TRADE_SEED — the paper fill CLOCK for the orders the frame loop is about to seed.
    // Appended AFTER the two style/scale sweeps so those keep operating on exactly the window set
    // they did before this arm existed.
    //
    // ⚠ **The window is deliberately CLOSED, and that is what makes this work at all.** The
    // orders need `Ingest::BarClose` on their `(venue, symbol)`; a bar feed with no window backing
    // it is torn down on the very next frame by `crate::ui::feed_lifecycle::reap_orphaned_feeds`,
    // whose live set is `live_window_keys` — and that function counts CHART and DOM windows only,
    // so a Trade/Tearsheet window holds no feed open. `live_window_keys` also counts a window
    // regardless of `open` (its own doc: a merely-hidden chart keeps its feed), so a CLOSED chart
    // is exactly the thing that is invisible to the capture and visible to the reaper.
    //
    // It also keeps the POSE. `crate::ui::initial_arrange::plan_initial_arrange` resolves its geometry
    // from `wins.iter().filter(|w| w.open).count()`, so a closed window is not counted: a
    // `VIKE_SHOT_WIN=trade` capture still sees exactly one open window and still takes the
    // `ArrangeAction::MaximizeLoneOpen` arm. An OPEN chart here would have silently turned every
    // seeded capture into a two-window Grid tile.
    //
    // Skipped when the layout already charts this series (the default layout, or a chart-pose
    // capture that also seeds): the feed would be the same key, and a second window would put a
    // second chip in the rail for a chart nobody asked for.
    if env.trade_seed {
        let mut clock = WinState::new(
            "capture-fill-clock",
            crate::ui::capture_seed::SEED_SYMBOL,
            crate::ui::capture_seed::FILL_CLOCK_INTERVAL,
            WinKind::Chart,
            egui::Rect::from_min_size(area.min, egui::vec2(700.0, 440.0)),
        );
        clock.open = false;
        if !out.wins.iter().any(|w| w.kind == WinKind::Chart && w.key() == clock.key()) {
            out.ensure_feeds.push(FeedSpec {
                venue: clock.venue.clone(),
                symbol: clock.symbol.clone(),
                interval: clock.interval.clone(),
                asset_class: None,
            });
            out.wins.push(clock);
        }
    }
    out
}

#[path = "startup_tests.rs"]
#[cfg(test)]
mod startup_tests;
