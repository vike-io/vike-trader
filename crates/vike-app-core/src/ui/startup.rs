//! `startup` — the STARTUP LAYOUT decision: which windows a fresh `App` opens, which feeds it must
//! ensure for them, and which global display settings it starts with.
//!
//! Moved down out of `vike-app`'s `main.rs` (`App::new`'s tail) for the same reason as
//! [`crate::ui::core_sync`] and [`crate::ui::feed_lifecycle`] before it: that file was in **no
//! gate** — `justfile`'s `ci_crates` omitted `vike-app` and `xtask/src/ci/tables/roster.rs` listed it in
//! `EXCLUDE_FROM_CI` — so nothing compiled, clippied or tested it. (The shell, `vike-desktop` now,
//! is still outside the roster; the `app-check` job compiles and clippy-gates it since, and runs
//! none of this tail.) This particular tail is a
//! **branch over several environment knobs** — `VIKE_SHOT`, one arm per `VIKE_SHOT_WIN` value,
//! `VIKE_STYLE`, `VIKE_SCALE`, `VIKE_POLY_COCKPIT_TOKEN` — plus a
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
//! ("`data` hardcodes `DATA_SHOT_DESTINATION`, so a destination with no arm of its own is reachable by no
//! capture"), which is the shape of a dispatch that wants to be a lookup. `data_shot_destination` is
//! that lookup: it folds the value through [`shot_key`] and matches it against
//! `crates/vike-app-core/src/ui/windows/data_manager/rail.rs`'s `DataManagerDestination::ALL` by each destination's own
//! `DataManagerDestination::label`, so a THIRTEENTH destination is capturable the moment it joins the rail and
//! nothing here has to be edited for it.
//!
//! **The environment is not read here.** `vike-desktop`'s `main.rs` reads the knobs and passes
//! them in as [`StartupEnvironment`] — the `Injected` shape `vike_config::settings`' module doc names as the
//! target state ("libraries take configuration as parameters; only binaries read the process
//! environment"). That keeps every `VIKE_*` settings-registry row where it is (`vike-desktop` /
//! `Layer::Binary` — `vike-app` until the shell's rename re-keyed them) *and* makes this planner
//! testable without `env::set_var`, which is a
//! process-global mutation the workspace's grouped test binaries forbid.
//!
//! **Data in, decisions out.** [`plan`] is pure: it returns a [`StartupLayout`] describing the
//! windows, the feeds to ensure and the per-window pre-seeds — it never touches
//! `App`. The caller applies it, in the order documented on [`StartupLayout`], because a `fn new`'s
//! startup ORDER is load-bearing. The one impure sibling, [`restored_workspace`], is the file-I/O
//! half (the named-layout-over-`workspace.json` preference) and is deliberately kept out of [`plan`]
//! so the branch logic stays a pure function of its inputs.

use crate::ui::window::{WindowKind, WindowState, workspace};
use crate::ui::windows::DataManagerDestination;
use vike_chart::{ChartScaleMode, chart::style::ChartStyle};
use vike_model::accounts::account_keys::AccountLabel;

/// The startup knobs `vike-desktop`'s `main.rs` reads off the process environment and hands down.
///
/// Every field is the ALREADY-READ value, never a variable name — see the module doc for why the
/// read stays in the binary. `Default` is the "no knob set" configuration, which is exactly the
/// normal desktop launch (a restored workspace or the single default chart, empty).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StartupEnvironment {
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
    /// `data_shot_destination`'s fold over `DataManagerDestination::ALL`, which lives in another module and may grow
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
    /// [`polymarket_cockpit_seed_token`] for the trim / empty / placeholder ladder.
    pub polymarket_cockpit_token: Option<String>,
}

/// One `(venue, symbol, interval, asset_class)` subscription the caller must ensure — the argument
/// list of `feed_lifecycle::ensure_feed_on`, captured as data so [`plan`] stays pure.
/// It is also what `feed_lifecycle::ensure_feed_on` takes by reference: the four values that used
/// to be four loose arguments on `App::ensure_feed_on`, bundled so the function stays under clippy's
/// `too_many_arguments` threshold once the two state bundles are also parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesSpec {
    /// Venue slug the subscription routes to (`"binance"`, `"okx"`, …).
    pub venue: String,
    /// The window's symbol, BEFORE venue-native instrument resolution.
    pub symbol: String,
    /// Kline interval (`"1m"`), or a client-side tick/volume interval (`"100t"`/`"10v"`).
    pub interval: String,
    /// The catalog asset class, when the symbol came from a catalog hit — drives
    /// `venue_bar_instrument`'s one refusal (`Combo`). `None` = the spot/quick-pick path.
    pub asset_class: Option<vike_model::AssetClass>,
}

impl SeriesSpec {
    /// The series a window shows: its venue, symbol, interval and asset class.
    pub fn of_window(window: &WindowState) -> Self {
        SeriesSpec {
            venue: window.venue.clone(),
            symbol: window.symbol.clone(),
            interval: window.interval.clone(),
            asset_class: window.asset_class,
        }
    }
}

/// What a fresh `App` starts with — and, critically, **in what order the caller must apply it**.
///
/// The order below is the order `App::new` performed these steps before the move, and it is
/// reproduced exactly:
///
/// 1. [`indicator_favourites`](Self::indicator_favourites) — assign the global setting (restored
///    workspace only).
/// 2. [`ensure_feeds`](Self::ensure_feeds) — `ensure_feed_on` each spec. Both arms that ensure
///    anything did so BEFORE publishing their windows, so this stays ahead of step 4.
/// 3. [`restored_windows`](Self::restored_windows) — emit the `workspace restored (N windows)` log.
/// 4. [`wins`](Self::wins) — publish the window list.
/// 5. `next_window_number = wins.len()`.
///
/// `Default` only — `WindowState` carries live egui dialog/pane state and derives nothing, so this
/// struct is neither `Clone` nor `Debug`. It is built once and consumed once.
#[derive(Default)]
pub struct StartupLayout {
    /// The indicator favourites a restored workspace carries, its one global setting (the time zone
    /// and the GPU candle layer are `preferences.*` rows now, `crate::ui::appearance_settings`).
    /// `None` means no workspace was restored, so the `App`'s own field initializers stand — NOT that
    /// an empty list should be written as the default.
    pub indicator_favourites: Option<Vec<String>>,
    /// Feeds to `ensure_feed_on` before the windows are published.
    pub ensure_feeds: Vec<SeriesSpec>,
    /// `Some(n)` iff a saved workspace was restored, carrying its window count for the log line.
    pub restored_windows: Option<usize>,
    /// Why the saved workspace or layout was NOT read (an older build's file, or a corrupt one):
    /// the one-line hint the shell shows on its status line. The default layout opens instead.
    pub refused: Option<String>,
    /// The window list, after the `VIKE_STYLE` / `VIKE_SCALE` capture overrides.
    pub wins: Vec<WindowState>,
    /// `Some((window, destination))` when a capture arm opened a Data Manager window on a rail
    /// destination other than its default. Carried out here rather than read from the environment
    /// inside the tool body: `WindowView` is created lazily by the window loop, so the plan cannot set
    /// it directly, and a library reading `VIKE_SHOT_WIN` a second time would be a second undeclared
    /// env read.
    ///
    /// ⚠ It is set for EVERY Data-Manager capture, including the one that lands on
    /// `DataManagerDestination::default()`. Seeding the default explicitly costs nothing and keeps
    /// `the_data_manager_arms_select_the_destination_they_name` able to iterate the whole rail —
    /// a destination that relied on "we set nothing and the default happens to be right" would be
    /// indistinguishable from an arm that silently failed to select anything.
    pub data_destination: Option<(egui::Id, DataManagerDestination)>,
    /// `Some((window, account))` when a capture arm opened a Data Manager window that must render a
    /// LABELLED account rather than the default one. Carried out here for exactly the reason
    /// [`Self::data_destination`] is — `WindowView` is created lazily by the window loop, so the plan
    /// cannot set it directly, and a library reading `VIKE_SHOT_WIN` a second time would be a
    /// second undeclared env read.
    ///
    /// ⚠ It is a ONE-SHOT: the caller seeds `WindowView::gateway_account`, and the tool body
    /// `take`s it on the first frame that renders. Nothing re-applies it, so an operator's own chip
    /// click is never stomped by a knob they set at launch.
    pub gateway_account: Option<(egui::Id, AccountLabel)>,
}

/// The Data Manager destination the `VIKE_SHOT_WIN=data` capture arm opens.
///
/// ⚠ This was `DATA_SUBTAB_STORED: usize = 5`, and its doc claimed the tool body `debug_assert`s the
/// index still points at `"Stored"`, "so a reordered tab list trips a test rather than silently
/// capturing the wrong pane". **No test ever ran either assert** — nothing in this crate's `tests/`
/// constructs a `WindowView` or calls the tool body, and `vike-desktop` is excluded from the CI
/// roster. The guarantee was prose. Naming a `DataManagerDestination` instead makes the wrong-pane capture a type
/// error rather than something a test was believed to catch.
///
/// It is now reachable under TWO spellings — this legacy alias and the destination's own derived
/// one (`all-series`) — and that is deliberate rather than tolerated: see [`DATA_SHOT_WIN_ALIAS`].
pub(crate) const DATA_SHOT_DESTINATION: DataManagerDestination = DataManagerDestination::AllSeries;

/// The ONE `VIKE_SHOT_WIN` value that names a Data-Manager destination without spelling it.
///
/// ⚠ **It is kept for compatibility and must stay that way.** `scripts/qa_shots.sh` captures
/// `06-data-manager` with this value, and an operator's own notes use it; a rename here would not
/// fail anything — the capture would still run, still save a PNG and still be counted in the index
/// — it would simply fall through to the default chart and put a chart on the contact
/// sheet under the Data Manager's name. That is the exact failure mode
/// [`crate::ui::windows::data_manager::rail`]'s module doc records the old index-based dispatch having, so
/// the alias is pinned by `the_shipped_spellings_still_resolve_where_they_always_did` rather
/// than left to a reader's care.
///
/// The other three shipped spellings — `instruments`, `overview`, `gateway` — need no alias:
/// they already ARE what [`shot_key`] derives from their destinations' own labels, which is why
/// the derivation could replace their arms without a compatibility table. `venues` was a fourth
/// such spelling, derived from the old `ArmingAccount` destination's label, retired along with that
/// destination. `gateway` was spelled `credentials` until the destination was renamed (decision
/// 0164 verdict 3): the rename is a spelling change, not an alias, and `credentials` is no longer
/// read — every caller (`scripts/qa_shots.sh` and the contact sheet) says `gateway` now.
///
/// ⚠ **This used to say `credentials` was "deliberately NOT substituted into its place here —
/// updating `scripts/qa_shots.sh`'s capture plus this module's two spelling/size-pinning tests to
/// the new destinations is a separate follow-up, left undone on purpose."** That follow-up landed
/// (Connections-merges-into-Data-Manager, Task 4): `scripts/qa_shots.sh`'s former `10-venues`
/// capture is `10-gateway` now, spelling `VIKE_SHOT_WIN=gateway`, which resolves by the
/// same derivation as `instruments`/`overview` and is pinned alongside them by
/// `the_shipped_spellings_still_resolve_where_they_always_did` — whose own doc carries this
/// history now, since it is the test that would catch a regression here. `backend` needs no entry
/// in that compatibility list for a different reason: unlike `venues`→`gateway`, nothing ever
/// shipped a legacy spelling for it to preserve, so it has nothing to be pinned AGAINST — it still
/// resolves by the same plain derivation, which `every_destination_folds_to_its_own_distinct_spelling`
/// covers for all thirteen destinations rather than by name.
pub(crate) const DATA_SHOT_WIN_ALIAS: &str = "data";

/// Fold one `VIKE_SHOT_WIN` value — or one `DataManagerDestination::label` — to the spelling the lookup matches
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
/// **DERIVED, not enumerated.** The roster is `DataManagerDestination::ALL` and the spelling is each
/// destination's own `DataManagerDestination::label` folded through [`shot_key`] — so this function has no list
/// in it to fall behind the rail, and a thirteenth destination is capturable the day it is added.
/// The rail grows the same way (a destination is one more row in `DataManagerDestination::ALL`), and this is
/// what keeps that true of the capture harness as well.
///
/// ⚠ **`label` is the right key and `icon` is not, even though both are per-destination.** The
/// label is what the operator READS off the rail, so the value they type is the thing they are
/// looking at; nothing else in the type is human-typable. The cost is real and accepted: renaming
/// a rail label renames a capture spelling, which is why [`DATA_SHOT_WIN_ALIAS`] exists at all and
/// why `scripts/qa_shots.sh`'s spellings are pinned by a test in this file rather than merely
/// derived.
#[must_use]
pub(crate) fn data_shot_destination(win: &str) -> Option<DataManagerDestination> {
    let key = shot_key(win);
    if key == shot_key(DATA_SHOT_WIN_ALIAS) {
        return Some(DATA_SHOT_DESTINATION);
    }
    DataManagerDestination::ALL.into_iter().find(|d| shot_key(d.label()) == key)
}

/// The capture window's size for one destination.
///
/// ⚠ **Deliberately an EXHAUSTIVE `match` with no `_` arm**, which is the opposite choice from
/// [`data_shot_destination`] one item above and is the point of the split. Reachability is a property of
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
/// The three sizes that shipped before the derivation (`data`/`overview` tall, `instruments`
/// short) fall out of that rule unchanged, and
/// `the_shot_win_arms_keep_their_documented_sizes` still pins them by hand — a rule that quietly
/// stopped reproducing them would change what existing contact-sheet frames show. (A fourth,
/// `venues`, was pinned here until `DataManagerDestination::ArmingAccount` — the destination it captured — was
/// removed; `gateway`/`backend` do not take its place in this test, see
/// [`DATA_SHOT_WIN_ALIAS`]'s doc.)
///
/// The WIDTH never varies: every destination shares one body width with the rail beside it
/// (`crates/vike-ui-theme/ui-theme.toml`'s `RAIL_WIDTH`), and a narrower capture would
/// review a column layout nobody runs.
#[must_use]
fn data_shot_size(destination: DataManagerDestination) -> egui::Vec2 {
    match destination {
        DataManagerDestination::Overview
        | DataManagerDestination::AllSeries
        | DataManagerDestination::HasGaps
        | DataManagerDestination::Stale
        | DataManagerDestination::CachedFeeds
        | DataManagerDestination::ActivityLog
        | DataManagerDestination::SymbolSets => egui::vec2(1200.0, 760.0),
        DataManagerDestination::ByVenue
        | DataManagerDestination::Providers
        | DataManagerDestination::Gateway
        | DataManagerDestination::Backend
        | DataManagerDestination::Instruments
        | DataManagerDestination::Store => egui::vec2(1200.0, 700.0),
    }
}

/// The account label the `gateway-account` capture arm selects — and therefore the label
/// `scripts/qa_shots.sh` must give the account row it seeds a credential into, for that capture to
/// show anything.
///
/// It is a CONSTANT rather than a knob because the two halves have to agree and only one of them
/// is Rust: the arm picks a label, the sheet seeds a store containing an account row carrying it.
/// They are held equal by `crates/vike-app-core/tests/qa_shot_account.rs`, which parses the
/// script's own fixture heredoc, writes it into a throwaway store the way the script does (the
/// account with its tier, then its fields) and reads the real `vike_account_manager::AccountManagerGrids`
/// back, asserting the label set is exactly the one this constant names — the sheet is the only
/// place a human ever sees this surface, and a label that drifted out of step would capture an
/// account no row carries, which renders as the `(new)` chip and cells with no account.
///
/// ⚠ Deliberately NOT a `VIKE_*` variable. A second env read inside a library is the thing
/// [`plan`]'s module doc exists to prevent, and injecting one through [`StartupEnvironment`] would buy a
/// `vike_config::settings::SETTINGS` row for a value nobody but this script sets.
pub const SHOT_ACCOUNT_LABEL: &str = "QA";

/// [`SHOT_ACCOUNT_LABEL`] as the validated type the rest of the workspace addresses accounts with.
///
/// ⚠ It cannot panic, on purpose — this runs inside a GUI's construction, where an `expect` on a
/// capture-only constant would take the whole app down. An unparseable edit degrades to the DEFAULT
/// account instead, and `the_labelled_account_arm_names_a_valid_label` is what refuses one: the
/// degraded capture would otherwise look like an ordinary Gateway shot and prove nothing.
#[must_use]
pub fn shot_account_label() -> AccountLabel {
    AccountLabel::parse(SHOT_ACCOUNT_LABEL).unwrap_or_default()
}

/// The initial cockpit token-id: the caller's `VIKE_POLY_COCKPIT_TOKEN` when set and non-empty
/// after trimming, else [`POLYMARKET_PLACEHOLDER_TOKEN`](crate::ui::windows::POLYMARKET_PLACEHOLDER_TOKEN),
/// until [`crate::ui::windows::follow_cockpit`] points the window at the live market the datahub's recorder holds.
pub fn polymarket_cockpit_seed_token(from_env: Option<&str>) -> String {
    from_env
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::ui::windows::POLYMARKET_PLACEHOLDER_TOKEN.to_string())
}

/// Load the workspace a fresh `App` should restore: `Ok(None)` when there is none, `Err(hint)` when
/// the file exists and is refused (an older build's layout: the hint names it and what to do).
///
/// Saved workspace wins over the default layout — but never under `VIKE_SHOT`, which needs the
/// deterministic default windows for QA captures. When a named layout was the last one explicitly
/// used (and still exists), it is preferred over the single default `workspace.json`, and its
/// refusal is the answer (the default layout opens, never another saved one under its hint); an
/// install with no named layouts is unaffected (`last_layout()` returns `None` → the plain
/// `load()` runs).
///
/// File I/O, hence kept OUT of the pure [`plan`] below: pass the result in.
pub fn restored_workspace(shot_mode: bool) -> Result<Option<workspace::WindowWorkspace>, String> {
    if shot_mode {
        return Ok(None);
    }
    match workspace::last_layout() {
        Some(name) => workspace::load_layout(&name).map(Some),
        None => workspace::load(),
    }
}

/// The one server-policy row this desktop reads itself: the Trade window's per-order notional guard
/// (`vike_tradehub_client::order_checks::OrderLimits`, seeded in `vike-desktop`'s `main`).
const DESKTOP_POLICY_ROW: &str = "policy.max_notional_per_order";

/// **The startup disclosure lines a desktop logs:** `vike_config::boot_lines` without the server's
/// policy rows. That renderer prints every `policy.*` row, set or not, because on a NODE the ceilings
/// are the safety story; a desktop places no order through them (the node's own policy judges each
/// one), so a thin client logged about fifteen `policy.* = <unset> [default]` lines at every start
/// for settings it never reads (the owner, 2026-11-16). The settings directory, the rows this PC's
/// database sets, the summary line and the credential-store lines stay, and so does the one policy
/// row the desktop reads (`DESKTOP_POLICY_ROW`). Chosen by each line's setting KEY
/// (`vike_config::BootLine::setting`), never by its words. PURE.
pub fn desktop_boot_lines(
    lines: &[vike_config::BootLine],
) -> impl Iterator<Item = &vike_config::BootLine> {
    lines.iter().filter(|l| {
        l.setting
            .as_deref()
            .is_none_or(|key| !key.starts_with("policy.") || key == DESKTOP_POLICY_ROW)
    })
}

/// Decide the whole startup layout. Pure: same inputs ⇒ same [`StartupLayout`].
///
/// `area` is the window's content rect at construction (`cc.egui_ctx.content_rect()`); `restored`
/// is [`restored_workspace`]'s result. A refused file restores nothing and its hint is
/// [`StartupLayout::refused`].
pub fn plan(
    area: egui::Rect,
    env: &StartupEnvironment,
    restored: Result<Option<workspace::WindowWorkspace>, String>,
) -> StartupLayout {
    let mut out = StartupLayout::default();
    let shot_win = env.shot_win.as_deref();
    let restored = restored.unwrap_or_else(|hint| {
        out.refused = Some(hint);
        None
    });

    if let Some(ws) = restored {
        out.indicator_favourites = Some(ws.indicator_favourites.clone());
        let wins = workspace::apply(&ws);
        for w in &wins {
            out.ensure_feeds.push(SeriesSpec::of_window(w));
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
        out.wins.push(WindowState::tool("tool-studio", WindowKind::Studio, r));
    } else if shot_win == Some("gateway-compact") {
        // QA: VIKE_SHOT_WIN=gateway-compact opens Data Manager landed on `DataManagerDestination::Gateway`
        // instead of the default chart, for headless VIKE_SHOT captures of the per-venue Status
        // column — same QA-hook family as VIKE_SHOT_WIN=studio above. The saved workspace is
        // already bypassed under VIKE_SHOT.
        //
        // ⚠ **Updated 2026-10-05: the standalone Connections window (`WindowKind::Connections`) is
        // deleted outright**, and every site that opened it — this one included — now opens
        // `WindowKind::Data` landed on `DataManagerDestination::Gateway` instead, which calls the exact same
        // render functions the old window's Credentials tab did. Renamed with the
        // destination (decision 0164 verdict 3, the old name `connections` is not read any more):
        // the `VIKE_SHOT_WIN` id `scripts/qa_shots.sh` uses is `gateway-compact` (the
        // `05-gateway-compact` pose, captured at this exact 760x620 footprint for direct comparison
        // against the derived `gateway` spelling's `10-gateway` pose at Data Manager's native
        // width). It cannot take `gateway` itself, which [`data_shot_destination`]'s derivation owns, and
        // folding it into that derived spelling would collapse the one capture pair this file
        // exists to keep distinct.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), egui::vec2(760.0, 620.0));
        let w = WindowState::tool("tool-gateway-compact", WindowKind::Data, r);
        out.data_destination = Some((w.id, DataManagerDestination::Gateway));
        out.wins.push(w);
    } else if shot_win == Some("gateway-account") {
        // QA: VIKE_SHOT_WIN=gateway-account opens the SAME Data Manager destination as the arm
        // above, with the labelled account [`SHOT_ACCOUNT_LABEL`] already selected in its chip strip.
        //
        // ⚠ It needs its OWN arm rather than a knob on `gateway-compact`, for the reason `venues` needs
        // one beside `data`: the selection is what is being captured, and nothing in the capture
        // harness could preselect an account before this existed. With a labelled account chosen,
        // `crates/vike-account-manager/src/view/strip.rs`'s `account_strip` renders two things the headless
        // layout suites cannot judge — a WRAPPING removal-instruction line that embeds the full
        // credential-store PATH, and a `horizontal_wrapped` chip strip that grows with the account
        // count. Both are rasterized text whose failure mode is overflow or a clipped path, and
        // the contact sheet is the only place anyone looks at a rendered surface here.
        //
        // The rect is the `gateway-compact` arm's, DELIBERATELY: the two captures then differ in
        // exactly one thing, so a human can diff `05-gateway-compact` against `05b-gateway-account`
        // by eye and every difference is the account dimension.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), egui::vec2(760.0, 620.0));
        let w = WindowState::tool("tool-gateway-account", WindowKind::Data, r);
        out.data_destination = Some((w.id, DataManagerDestination::Gateway));
        out.gateway_account = Some((w.id, shot_account_label()));
        out.wins.push(w);
    } else if shot_win == Some("polymarket") {
        // QA: VIKE_SHOT_WIN=polymarket opens the Polymarket scalp-cockpit tool window instead of
        // the default chart, for headless VIKE_SHOT captures of the live probability ladder —
        // same QA-hook family as VIKE_SHOT_WIN=studio/gateway-compact above. Seed a real token with
        // VIKE_POLY_COCKPIT_TOKEN; the local process dialling Polymarket (a `vike-backend datahub`)
        // reads its own `venue.polymarket.*` rows for a live book through the Dublin proxy — this
        // GUI links no Polymarket bridge. The saved workspace is already bypassed under VIKE_SHOT.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), egui::vec2(760.0, 620.0));
        let mut ws = WindowState::tool("tool-polymarket", WindowKind::Polymarket, r);
        // Seed the YES-outcome token-id (VIKE_POLY_COCKPIT_TOKEN) so the window-loop's
        // `ensure_polymarket_book` actually subscribes its live book — WITHOUT this the symbol stays
        // the placeholder and the ladder never leaves STALE (mirrors the launcher open arm).
        ws.symbol = polymarket_cockpit_seed_token(env.polymarket_cockpit_token.as_deref());
        ws.title = "Polymarket · Cockpit".to_string();
        out.wins.push(ws);
    } else if shot_win == Some("trade") {
        // QA: VIKE_SHOT_WIN=trade opens the Trade window (ladder, ticket and status strip) at the
        // spec's size instead of the default chart, for headless VIKE_SHOT captures — same
        // QA-hook family as VIKE_SHOT_WIN=studio/gateway-compact/polymarket above. The window opens
        // with no instrument and the glue seeds one on its first frame, exactly as the launcher's
        // does. Saved workspace is already bypassed under VIKE_SHOT. ⚠ Not the launcher's size:
        // that one is as tall as the look needs (`window::spawn::trade_open_size`) and is measured
        // in the type, which egui has only from the first frame on, after this plan is made.
        let r = egui::Rect::from_min_size(
            area.min + egui::vec2(8.0, 8.0),
            vike_order_entry::trade_window::layout::spec_size(
                vike_order_entry::trade_window::TradeWindowState::default().view,
                vike_order_entry::trade_window::layout::ticket_w(
                    &vike_ui_theme::components::ThemeTokens::from_appearance(
                        &vike_ui_theme::appearance::ThemeAppearance::default(),
                    ),
                ),
            ),
        );
        out.wins.push(WindowState::tool("tool-trade", WindowKind::Trade, r));
    } else if shot_win == Some("account") {
        // QA: VIKE_SHOT_WIN=account opens the Account window (the equity header and its five tabs,
        // on Accounts) — what `trade` opened until the new Trade window took that slug — at the size
        // the window opens at (`crate::ui::window::spawn::tool_window_size`), so the capture shows the
        // window an operator gets. This is the window `--observe` mode is meant to be screenshotted in
        // (charts stay empty on the wire). Saved workspace is already bypassed under VIKE_SHOT.
        let size = crate::ui::window::spawn::tool_window_size(WindowKind::Account);
        let r = egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), size);
        out.wins.push(WindowState::tool("tool-account", WindowKind::Account, r));
    } else if shot_win == Some("options") {
        // QA: VIKE_SHOT_WIN=options opens the Options tool window (the keyless Deribit option
        // chain — live quotes, no creds, not geo-blocked) instead of the default chart, for
        // headless VIKE_SHOT captures — same QA-hook family as VIKE_SHOT_WIN=trade above.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), egui::vec2(1200.0, 820.0));
        out.wins.push(WindowState::tool("tool-options", WindowKind::Options, r));
    } else if shot_win == Some("settings") {
        // QA: VIKE_SHOT_WIN=settings opens the Settings window (its one section, Appearance) for
        // headless VIKE_SHOT captures, at whatever appearance the settings root holds — so a
        // capture per theme is one `vike-cli config set preferences.theme <word>` away.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), egui::vec2(760.0, 620.0));
        out.wins.push(WindowState::tool("tool-settings", WindowKind::Settings, r));
    } else if let Some(destination) = shot_win.and_then(data_shot_destination) {
        // QA: every Data Manager destination, in ONE arm — see [`data_shot_destination`] for the lookup
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
        // persists them: `crates/vike-app-core/src/ui/window/workspace/mod.rs`'s `capture` filters to
        // `WindowKind::Chart` and `crates/vike-desktop/src/app_ui/mod.rs`'s `persist_egui_memory` returns
        // `false`.
        let r =
            egui::Rect::from_min_size(area.min + egui::vec2(8.0, 8.0), data_shot_size(destination));
        let id_source = format!("tool-{}", shot_key(destination.label()));
        let w = WindowState::tool(&id_source, WindowKind::Data, r);
        out.data_destination = Some((w.id, destination));
        out.wins.push(w);
    } else {
        // ⚠ An unrecognised `VIKE_SHOT_WIN` falls through to the DEFAULT CHART, and it says so
        // out loud. The fall-through itself is load-bearing and pinned by
        // `an_unrecognized_shot_win_falls_through_to_the_default_chart`: a capture that opened
        // NOTHING would render an empty desktop, and an empty desktop is what a genuinely broken
        // window also looks like. But silence was the real defect — a typo'd destination
        // (`all_seires`, or a rail label renamed since the notes were written) produced a
        // perfectly good default chart under the destination's filename, and the contact sheet is
        // read by a human scanning for layout faults, not for identity ones. So a value that was
        // SET and matched nothing is warned about by name.
        //
        // Only `Some(..)` warns: an unset knob is the ordinary desktop launch and has nothing to
        // report. `tracing::warn!` rather than a field on [`StartupLayout`] — the caller is
        // `vike-desktop`'s `App::new`, which runs well after `vike_log::init` (`main` builds the
        // subscriber before `eframe::run_native`), and a field nobody reads is one more thing to
        // keep in step. [`plan`] stays pure in the sense its doc claims — same inputs, same
        // `StartupLayout` — because a diagnostic is not a decision.
        // A Help window's name (`crate::ui::help::SHOT_WINS`) poses that window over this chart:
        // it is recognised, so it is not warned about.
        if let Some(win) = shot_win.filter(|w| !crate::ui::help::SHOT_WINS.contains(w)) {
            tracing::warn!(
                "unrecognised VIKE_SHOT_WIN value {win:?} — opening the default chart instead. A \
                 Data Manager destination is spelled as its rail label, lowercased and hyphenated \
                 (Overview, All series -> all-series, Venues, ...)."
            );
        }
        // default layout: a single maximized chart, EMPTY — no symbol is compiled in (the owner's
        // ruling of 2026-11-13). It names no series, so it ensures no feed; its badge says how to
        // pick one, and a series the backend publishes is adopted into it
        // (`crate::ui::series_follow::follow_backend`). A saved workspace, the user's own choice,
        // is the arm above.
        //
        // Create it FLOATING (like a user "New chart window") and let the runtime
        // lone-window arrange maximize it with the REAL desktop. Do NOT maximize here
        // with `area`: at construction `area = content_rect()` still includes the
        // status-bar region (the real arena, `self.desktop = ui.max_rect()`, excluding
        // the bottom status bar, is only known during a frame). Maximizing to it gave the
        // startup chart a stale `pre_maximize` → min/max/restore misbehaved; a user-created
        // chart works because it's maximized later with the real desktop. Same path now.
        let w = crate::ui::window::spawn::chart_on(
            "chart-0",
            None,
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
        && let Ok(index) = s.parse::<usize>()
        && let Some(&st) = ChartStyle::ALL.get(index)
    {
        for w in &mut out.wins {
            w.style = st;
        }
    }
    // QA: VIKE_SCALE=<0|1|2> forces every window's requested price-scale
    // mode (Linear/Log/Percent) for capture — chart-UX bundle T3, same
    // pattern as VIKE_STYLE above.
    if let Some(s) = env.scale.as_deref()
        && let Ok(index) = s.parse::<usize>()
    {
        let sm = match index {
            0 => Some(ChartScaleMode::Linear),
            1 => Some(ChartScaleMode::Logarithmic),
            2 => Some(ChartScaleMode::Percent),
            _ => None,
        };
        if let Some(sm) = sm {
            for w in &mut out.wins {
                w.scale = sm;
            }
        }
    }
    out
}

#[path = "startup_tests.rs"]
#[cfg(test)]
mod startup_tests;
