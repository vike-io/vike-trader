use super::{
    GAP_SEGMENTS, GapMode, LOG_SEGMENTS, LogPlane, MEMBER_SEGMENTS, MemberCut, PROVIDER_SEGMENTS,
    ProviderPlane, STALE_SORTS, backfillable, dataset_groups, ds_load_form, feed_cell,
    fmt_series_dt, gapped_series_keys, log_plane, member_cell, member_is_stale, member_keys,
    member_series, provider_names_a_venue, span_label, stale_cutoff_ms, stale_series_keys,
};
use crate::tools::ToolView;
use vike_data::SeriesCoverage;
use vike_data::datasets::DataSet;
use vike_data_manager::model::{SeriesRow, VenueNode};
use vike_data_manager::{GapMap, SeriesKey, SortColumn, SymbolNode};
use vike_tradehub_client::wire::{WireDirectory, WireDirectoryVenue};
use vike_ui_theme::components::segmented::Segment;

/// One day in epoch-ms — every timestamp below is built from it so the arithmetic in the
/// assertions is readable rather than a wall of thirteen-digit literals.
const DAY: i64 = 86_400_000;

/// A one-venue tree: `binance` with two symbols, each carrying one series whose `last_ts` is
/// the caller's. `first_ts` is pinned at day 0 so the tree-wide window is the caller's to set
/// through the LATEST row.
fn tree(rows: &[(&str, &str, Option<&str>, i64)]) -> Vec<VenueNode> {
    let symbols: Vec<SymbolNode> = rows
        .iter()
        .map(|(symbol, kind, interval, last_day)| SymbolNode {
            symbol: (*symbol).to_string(),
            grouped: false,
            series: vec![SeriesRow {
                kind: (*kind).to_string(),
                interval: interval.map(str::to_string),
                cov: SeriesCoverage { first_ts: 0, last_ts: last_day * DAY, ..Default::default() },
            }],
            total: Default::default(),
        })
        .collect();
    vec![VenueNode { venue: "binance".to_string(), symbols, total: Default::default() }]
}

/// One planted row for [`member_tree`]: venue, symbol, grouped, kind, interval, last-day index.
///
/// ⚠ A `type` rather than the tuple spelled at the parameter: `clippy::type_complexity` counts
/// six components as too many to read at a call site, and `-D warnings` makes that a build
/// failure rather than advice. The sibling `Member` alias exists for the same lint one screen
/// up — this one was written as a bare tuple and the lane was the first thing to say so.
type MemberRow<'a> = (&'a str, &'a str, bool, &'a str, Option<&'a str>, i64);

/// A MULTI-venue tree for the member-symbol derivations: one row per SERIES, as
/// `(venue, symbol, grouped, kind, interval, last_day)`, folded into venue → symbol nodes in
/// the order given.
///
/// ⚠ `tree` above cannot serve these and the reason is the whole point of the tests below: it
/// builds exactly ONE venue, always named `binance`, with one series per symbol and
/// `grouped: false` on every node — and the provider scoping, the `Auto` span and the
fn member_tree(rows: &[MemberRow<'_>]) -> Vec<VenueNode> {
    let mut venues: Vec<VenueNode> = Vec::new();
    for (venue, symbol, grouped, kind, interval, last_day) in rows {
        let series = SeriesRow {
            kind: (*kind).to_string(),
            interval: interval.map(str::to_string),
            cov: SeriesCoverage { first_ts: 0, last_ts: last_day * DAY, ..Default::default() },
        };
        // ⚠ Indices, not `iter_mut().find()`: the borrow that a `find` holds over the match
        // scrutinee extends into the `None` arm that wants to push, which NLL rejects. A
        // `position` returns a plain `usize` and holds nothing.
        let vi = match venues.iter().position(|v| v.venue == *venue) {
            Some(i) => i,
            None => {
                venues.push(VenueNode {
                    venue: (*venue).to_string(),
                    symbols: Vec::new(),
                    total: Default::default(),
                });
                venues.len() - 1
            }
        };
        let v = &mut venues[vi];
        // Keyed on (label, GROUPED) exactly as `build_tree` keys it — a grouped node and a
        // per-symbol one may share a label and are different instruments.
        let si = match v.symbols.iter().position(|s| s.symbol == *symbol && s.grouped == *grouped) {
            Some(i) => i,
            None => {
                v.symbols.push(SymbolNode {
                    symbol: (*symbol).to_string(),
                    grouped: *grouped,
                    series: Vec::new(),
                    total: Default::default(),
                });
                v.symbols.len() - 1
            }
        };
        v.symbols[si].series.push(series);
    }
    venues
}

fn key(symbol: &str, kind: &str, interval: Option<&str>) -> SeriesKey {
    SeriesKey {
        venue: "binance".to_string(),
        symbol: symbol.to_string(),
        kind: kind.to_string(),
        interval: interval.map(str::to_string),
    }
}

fn dataset() -> DataSet {
    DataSet {
        name: "Majors".to_string(),
        provider: "binance".to_string(),
        interval: "5m".to_string(),
        benchmark: "BTCUSDT".to_string(),
        symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
        user: true,
    }
}

#[test]
fn series_dt_renders_utc_and_dashes_the_empty_sentinel() {
    assert_eq!(fmt_series_dt(1_700_000_000_000), "2023-11-14 22:13");
    // The "no coverage" sentinel and anything before the epoch stay an em-dash.
    assert_eq!(fmt_series_dt(0), "—");
    assert_eq!(fmt_series_dt(-1), "—");
}

#[test]
fn load_form_copies_the_dataset_into_the_editor_and_clears_the_symbol_pick() {
    let mut tv = ToolView { ds_sym_sel: Some("STALE".to_string()), ..Default::default() };
    ds_load_form(&mut tv, &dataset());
    assert_eq!(tv.ds_sel.as_deref(), Some("Majors"));
    assert_eq!(tv.ds_name, "Majors");
    assert_eq!(tv.ds_provider, "binance");
    assert_eq!(tv.ds_interval, "5m");
    assert_eq!(tv.ds_benchmark, "BTCUSDT");
    // Symbols land in the multiline editor as the comma-joined text the tab edits.
    assert_eq!(tv.ds_symbols_text, "BTCUSDT, ETHUSDT");
    // A previous tab's symbol pick must NOT survive a DataSet switch (it would "Test" a
    // symbol that is not in the newly-loaded set).
    assert_eq!(tv.ds_sym_sel, None);
}

// ===== The action strips' pure folds =====

#[test]
fn stale_keys_are_the_series_lagging_more_than_a_third_of_the_window() {
    // A 100-day window, so the 35% threshold falls at 35 days behind the newest row: a series
    // whose last row is at day 64 is 36 days behind and stale, one at day 66 is 34 and is not.
    // The window is passed EXPLICITLY rather than through `global_span`, which reads each
    // venue's rollup — the fixture leaves those at their defaults because the fold under test
    // walks the series rows and never looks at a rollup.
    let t = tree(&[
        ("BTCUSDT", "bar", Some("1m"), 100),
        ("ETHUSDT", "trade", None, 64),
        ("SOLUSDT", "bar", Some("1h"), 66),
    ]);
    let stale = stale_series_keys(&t, 0, 100 * DAY);
    assert_eq!(stale, vec![key("ETHUSDT", "trade", None)]);
    // The predicate is the shared crate's, so the two cannot disagree about a row.
    assert!(vike_data_manager::is_stale(64 * DAY, 0, 100 * DAY));
    assert!(!vike_data_manager::is_stale(66 * DAY, 0, 100 * DAY));
}

#[test]
fn a_zero_width_window_makes_nothing_stale() {
    // An empty or single-instant tree: `global_span` answers `(0, 0)` and `is_stale` refuses a
    // window whose end is not after its start, so the screen must offer to update NOTHING
    // rather than every row it can see.
    let t = tree(&[("BTCUSDT", "bar", Some("1m"), 100)]);
    assert!(stale_series_keys(&t, 0, 0).is_empty());
    assert_eq!(stale_cutoff_ms(0, 0), None);
}

#[test]
fn the_stale_cutoff_is_the_thirty_five_percent_point_of_the_window() {
    // 100 days wide ⇒ the cut-off sits 35 days before the newest row. This is the date the
    // strip prints, so it has to be the same arithmetic `is_stale` applies.
    assert_eq!(stale_cutoff_ms(0, 100 * DAY), Some(65 * DAY));
    // ...and a row exactly ON the cut-off is NOT stale (`is_stale` is a strict `>`), which is
    // why the legend says "before" rather than "on or before".
    assert!(!vike_data_manager::is_stale(65 * DAY, 0, 100 * DAY));
}

#[test]
fn gapped_keys_skip_a_series_that_was_fetched_and_found_whole() {
    // ⚠ The distinction this test exists for: `GapMap` holds an entry for every series whose
    // gaps were FETCHED, so a whole series sits in the map with an EMPTY list. Counting the
    // map's length — which the rail badge does deliberately, for cost — would put that series
    // on a screen whose whole claim is that every row on it has a hole.
    let t = tree(&[
        ("BTCUSDT", "bar", Some("1m"), 100),
        ("ETHUSDT", "trade", None, 100),
        ("SOLUSDT", "quote", None, 100),
    ]);
    let mut gaps = GapMap::new();
    gaps.insert(key("BTCUSDT", "bar", Some("1m")), vec![(10 * DAY, 12 * DAY)]);
    gaps.insert(key("ETHUSDT", "trade", None), Vec::new());
    // SOLUSDT is absent from the map entirely — never fetched, treated as gap-free.
    assert_eq!(gaps.len(), 2, "the map itself holds two entries");
    assert_eq!(gapped_series_keys(&t, &gaps), vec![key("BTCUSDT", "bar", Some("1m"))]);
}

#[test]
fn backfillable_is_bar_with_an_interval_and_carries_no_venue_term() {
    // Mirrors `crate::data::backfill_plan::plan_backfill_jobs`' ONE client-side gate. A bar series
    // with no interval cannot be requested at any resolution, so it is a skip like the rest.
    let keys = [
        key("BTCUSDT", "bar", Some("1m")),
        key("ETHUSDT", "bar", None),
        key("SOLUSDT", "trade", None),
        key("XRPUSDT", "quote", None),
    ];
    assert_eq!(backfillable(&keys), 1);
    // ⚠ And the property the design mock's hover text would break: the venue is NOT part of
    // the gate. An off-roster venue is still planned and sent, so the server's own refusal
    // names the server's supported set — see `backfillable`'s doc.
    let off_roster = [SeriesKey {
        venue: "a-venue-this-build-never-heard-of".to_string(),
        symbol: "BTCUSDT".to_string(),
        kind: "bar".to_string(),
        interval: Some("1m".to_string()),
    }];
    assert_eq!(backfillable(&off_roster), 1);
}

#[test]
fn span_label_states_the_window_or_says_it_has_none() {
    assert_eq!(span_label(0, 10 * DAY), "1970-01-01 → 1970-01-11 · 10 d");
    // A zero-width or inverted window prints no dates at all rather than a one-day span that
    // was never measured.
    assert_eq!(span_label(0, 0), "no span yet");
    assert_eq!(span_label(10 * DAY, 0), "no span yet");
}

#[test]
fn log_plane_classifies_both_of_todays_producers_as_feeds() {
    // These are the EXACT two shapes written today — this module's Refresh, and
    // `crates/vike-desktop/src/app_ui.rs`'s `data_delete` drain. If either ever reads as
    // something else, the log's default `Feeds` segment silently stops showing the only lines
    // that exist.
    assert_eq!(log_plane("17:42:06  Refreshed · 14 series"), LogPlane::Feeds);
    assert_eq!(log_plane("17:05:12  Stopped BTCUSDT@1m"), LogPlane::Feeds);
}

#[test]
fn log_plane_routes_the_store_and_backfill_vocabulary() {
    assert_eq!(log_plane("17:37:55  bybit 1m bar — 4,320 rows appended"), LogPlane::Backfill);
    assert_eq!(log_plane("17:36:02  okx rollup refreshed"), LogPlane::Store);
    assert_eq!(log_plane("16:51:44  gap sweep — 7 series"), LogPlane::Store);
    // ⚠ Precedence, stated as a test rather than only in prose: a backfill line routinely
    // names the store it wrote into, and "what did this do" is the more specific question.
    assert_eq!(log_plane("17:19:55  backfill wrote to the store"), LogPlane::Backfill);
}

/// Every segmented control offers each of its values once, gives each a hover text, and opens on
/// its leftmost segment — the value a screen falls back to when egui evicts its temp memory. The
/// segment carries its value, so the index arrays and `as usize` casts this replaced are gone.
#[test]
fn every_segment_roster_offers_each_value_once_and_leads_with_the_default() {
    fn check<T: PartialEq + Copy + std::fmt::Debug>(
        name: &str,
        segs: &[Segment<'_, T>],
        lead: T,
        all: &[T],
    ) {
        assert_eq!(segs[0].value, lead, "{name}: a fresh screen opens on the leftmost segment");
        assert_eq!(segs.len(), all.len(), "{name}");
        for v in all {
            assert_eq!(segs.iter().filter(|s| s.value == *v).count(), 1, "{name}: {v:?}");
        }
        assert!(segs.iter().all(|s| !s.why.is_empty()), "{name}: a segment without a hover");
    }
    check("gap", &GAP_SEGMENTS, GapMode::default(), &[GapMode::PerSeries, GapMode::CrossKind]);
    check(
        "member",
        &MEMBER_SEGMENTS,
        MemberCut::default(),
        &[MemberCut::AllMembers, MemberCut::StaleOnly],
    );
    check(
        "provider",
        &PROVIDER_SEGMENTS,
        ProviderPlane::default(),
        &[ProviderPlane::Historical, ProviderPlane::Event, ProviderPlane::Streaming],
    );
    check(
        "log",
        &LOG_SEGMENTS,
        None,
        &[None, Some(LogPlane::Feeds), Some(LogPlane::Backfill), Some(LogPlane::Store)],
    );
    assert_eq!(MEMBER_SEGMENTS[0].label, "all members");
    assert_eq!(MEMBER_SEGMENTS[1].label, "stale only");
}

/// One Cached-feeds cell: the key is split at `@`, the bar count is grouped, and the bounds are UTC.
/// With no directory the Source column is the feed's venue KEY, never a spelling invented here.
#[test]
fn feed_cells_split_the_key_and_format_the_bounds() {
    let feed = ("BTCUSDT@1m".to_string(), 1_440, 1_700_000_000_000, 0);
    let row: Vec<String> = (0..6).map(|c| feed_cell(&feed, c, None)).collect();
    assert_eq!(row, ["BTCUSDT", "1m", "1,440", "2023-11-14 22:13", "—", "binance"]);
}

fn titled(pairs: &[(&str, &str)]) -> WireDirectory {
    WireDirectory {
        venues: pairs
            .iter()
            .map(|(name, title)| WireDirectoryVenue {
                name: (*name).to_string(),
                title: Some((*title).to_string()),
            })
            .collect(),
        accounts: vec![],
    }
}

/// I15(b) of the Trade window plan: a feed's Source is ITS OWN venue (the `venue:` prefix of its
/// key; a bare key is the default venue's), spelled as the settings database spells it. The
/// column read "Binance" for every feed, an okx one included.
#[test]
fn a_feeds_source_is_its_own_venue_spelled_by_the_directory() {
    let okx = ("okx:BTC-USDT@5m".to_string(), 1, 0, 0);
    let dir = titled(&[("okx", "OKX")]);
    assert_eq!(feed_cell(&okx, 0, Some(&dir)), "BTC-USDT", "the venue is the Source column's");
    assert_eq!(feed_cell(&okx, 1, Some(&dir)), "5m");
    assert_eq!(feed_cell(&okx, 5, Some(&dir)), "OKX");
    assert_eq!(feed_cell(&okx, 5, None), "okx", "no directory: the key");
}

/// I15(a): the DataSet tree's groups are the providers the sets actually name, sorted, each
/// spelled as the settings database spells it (else its key) — never a venue list in code. `Auto`
/// is the form's sentinel, not a venue, so it heads no group; "All" and "My DataSets" stay.
#[test]
fn the_dataset_groups_are_the_providers_the_sets_name_spelled_by_the_directory() {
    let set = |name: &str, provider: &str, user: bool| DataSet {
        name: name.into(),
        symbols: vec![],
        provider: provider.into(),
        interval: "1m".into(),
        benchmark: String::new(),
        user,
    };
    let sets = vec![
        set("FX", "dukascopy", false),
        set("Mine", "Auto", true),
        set("Crypto", "binance", false),
        set("Alts", "binance", true),
    ];
    let dir = titled(&[("binance", "Binance")]);
    let groups = dataset_groups(&sets, Some(&dir));
    let labels: Vec<&str> = groups.iter().map(|g| g.label.as_str()).collect();
    assert_eq!(labels, ["All", "Binance", "dukascopy", "My DataSets"]);
    let members = |label: &str| -> Vec<&str> {
        let g = groups.iter().find(|g| g.label == label).expect("a group");
        sets.iter().filter(|d| g.has(d)).map(|d| d.name.as_str()).collect()
    };
    assert_eq!(members("All"), ["FX", "Mine", "Crypto", "Alts"]);
    assert_eq!(members("Binance"), ["Crypto", "Alts"]);
    assert_eq!(members("dukascopy"), ["FX"]);
    assert_eq!(members("My DataSets"), ["Mine", "Alts"]);
    let bare: Vec<String> = dataset_groups(&sets, None).into_iter().map(|g| g.label).collect();
    assert_eq!(bare, ["All", "binance", "dukascopy", "My DataSets"], "no directory: the keys");
}

// ===== The DataSet member-symbol derivations =====
//
// ⚠ These had NO tests while they were block-local inside the `DataSets` arm — the
// `#[cfg(test)]` module cannot reach a block-local item — and closing that is what the move to
// module scope was for. Each test below pins a claim that previously lived only in a doc
// comment.

#[test]
fn provider_names_a_venue_refuses_only_the_sentinel_and_the_blank() {
    assert!(provider_names_a_venue("binance"));
    assert!(provider_names_a_venue("dukascopy"));
    // `Auto` is what the form's dropdown OPENS on, so it is the common case, not the odd one.
    assert!(!provider_names_a_venue("Auto"));
    assert!(!provider_names_a_venue("AUTO"), "the sentinel is case-blind");
    assert!(!provider_names_a_venue(""));
    assert!(!provider_names_a_venue("  \t "), "whitespace is blank");
    // ⚠ The sentinel is refused as a WHOLE value, never as a substring — a venue whose name
    // merely begins with those four letters is a venue.
    assert!(provider_names_a_venue("autotrader"));
}

#[test]
fn an_unstored_member_is_not_stale_because_it_has_no_last_ts_to_judge() {
    // The same 100-day window the sibling stale tests use: 35% puts the cut-off at day 65.
    let behind = vec![(key("DOGEUSDT", "bar", Some("1m")), 40 * DAY)];
    let fresh = vec![(key("BTCUSDT", "bar", Some("1m")), 100 * DAY)];
    assert!(member_is_stale(&behind, 0, 100 * DAY));
    assert!(!member_is_stale(&fresh, 0, 100 * DAY));
    // ⚠ THE CLAIM the `· not stored` row marker and the strip's legend both rest on: a member
    // the store holds NOTHING for is not stale. There is no `last_ts` to judge, and "never
    // fetched" is a different condition from "fallen behind" — conflating them would make
    // `stale only` quietly mean two things. An `any` over an empty slice is `false` by
    // construction, so this test exists for the OTHER direction: a future "treat unstored as
    // maximally stale" edit has to argue with something rather than reading as a fix.
    assert!(!member_is_stale(&[], 0, 100 * DAY));
    // ...and it stays false with no window at all, so an empty tree cannot make every member
    // on every DataSet stale at once.
    assert!(!member_is_stale(&[], 0, 0));
}

#[test]
fn a_member_is_stale_when_any_one_of_its_series_is() {
    // `any`, not `all`: a symbol whose 1m bars are current but whose trade tape stopped months
    // ago IS behind, and the screen must not hide it because one series of several is fresh.
    let mixed = vec![
        (key("ETHUSDT", "bar", Some("1m")), 100 * DAY),
        (key("ETHUSDT", "trade", None), 20 * DAY),
    ];
    assert!(member_is_stale(&mixed, 0, 100 * DAY));
}

#[test]
fn member_series_matches_case_insensitively_in_both_directions() {
    let t = member_tree(&[
        ("binance", "btcusdt", false, "bar", Some("1m"), 100),
        ("bybit", "BTCUSDT", false, "bar", Some("1m"), 40),
    ]);
    // ⚠ `datasets::parse_symbols` UPPER-CASES whatever the operator typed, while the store's
    // spelling is the store's. Match either side exactly and a venue that writes `btcusdt`
    // reads as "not stored" on this screen while holding a year of bars.
    assert_eq!(member_series(&t, "binance", "BTCUSDT").len(), 1);
    assert_eq!(member_series(&t, "bybit", "btcusdt").len(), 1);
    // The PROVIDER is compared the same way, and trimmed — the field is free text on a form.
    assert_eq!(member_series(&t, "  BINANCE ", "BTCUSDT").len(), 1);
}

#[test]
fn auto_spans_every_venue_while_a_named_provider_refuses_the_others() {
    let t = member_tree(&[
        ("binance", "BTCUSDT", false, "bar", Some("1m"), 100),
        ("bybit", "BTCUSDT", false, "bar", Some("1m"), 40),
        ("okx", "ETHUSDT", false, "bar", Some("1m"), 40),
    ]);
    // `Auto` is what the form opens on and it means "wherever this lives", so it spans venues.
    let spanned = member_series(&t, "Auto", "BTCUSDT");
    let venues: Vec<&str> = spanned.iter().map(|(k, _)| k.venue.as_str()).collect();
    assert_eq!(venues, vec!["binance", "bybit"]);
    // An empty provider is the same sentinel by another spelling: a set naming no venue cannot
    // scope to one.
    assert_eq!(member_series(&t, "", "BTCUSDT").len(), 2);
    // ⚠ A named provider refuses the other venues' rows OUTRIGHT. The member reads as "not
    // stored" at that venue even though the symbol exists elsewhere — which is what the set
    // declared, and what makes the synthesized backfill key below the right answer for it.
    assert!(member_series(&t, "okx", "BTCUSDT").is_empty());
}

#[test]
fn a_grouped_node_is_never_matched_as_a_member_symbol() {
    // ⚠ A GROUPED node's `symbol` field holds the GROUP NAME, not a symbol
    // (`vike_data_manager::SymbolNode::grouped`'s own doc). A member called `MAJORS` must
    // therefore NOT pick up a grouped node of the same name: it would be judged stale on this
    // screen and queued for backfill under a name no venue trades. `crate::data::stored_load`
    // records the same class of bug from the load side, which is why the skip is pinned here
    // rather than resting on a comment.
    let t = member_tree(&[
        ("binance", "MAJORS", true, "bar", Some("1m"), 10),
        ("binance", "MAJORS", false, "bar", Some("1m"), 100),
    ]);
    let hit = member_series(&t, "binance", "MAJORS");
    assert_eq!(hit.len(), 1, "only the per-symbol node may match");
    // ...and it is the PER-SYMBOL one. The grouped node's day-10 row would have flipped this
    // member to stale against a 100-day window, so the skip is load-bearing, not tidiness.
    assert_eq!(hit[0].1, 100 * DAY);
    assert!(!member_is_stale(&hit, 0, 100 * DAY));
    // With ONLY the grouped node present, the member resolves to nothing at all — and then
    // reads as `· not stored`, which is the honest answer: no venue holds `MAJORS`.
    let grouped_only = member_tree(&[("binance", "MAJORS", true, "bar", Some("1m"), 10)]);
    assert!(member_series(&grouped_only, "binance", "MAJORS").is_empty());
}

#[test]
fn member_keys_prefer_the_stored_series_over_the_sets_declaration() {
    // BRANCH 1. Real keys carry the venue, kind and interval the store actually holds, and are
    // what `crate::data::backfill_plan::plan_backfill_jobs` can look gap ranges up for — a
    // synthesized key would instead re-fetch a default lookback over data already on disk.
    let stored = vec![
        (key("BTCUSDT", "bar", Some("1m")), 100 * DAY),
        (key("BTCUSDT", "trade", None), 100 * DAY),
    ];
    let keys = member_keys(&stored, "binance", "5m", "BTCUSDT");
    assert_eq!(keys, vec![key("BTCUSDT", "bar", Some("1m")), key("BTCUSDT", "trade", None)]);
    // ⚠ BOTH are returned, the non-`bar` one included: the PLANNER counts that one as a skip
    // and the button's hover reports it, so a kind this window cannot fetch is surfaced rather
    // than silently dropped here.
    assert_eq!(backfillable(&keys), 1);
    // The set's own interval is NOT imposed on a stored series — `5m` appears nowhere.
    assert!(keys.iter().all(|k| k.interval.as_deref() != Some("5m")));
}

#[test]
fn an_unstored_member_synthesizes_one_bar_key_from_the_sets_own_declaration() {
    // BRANCH 2 — the case the Has-gaps and Stale strips never meet, because they queue rows
    // that are in the store by construction. A DataSet is a WISHLIST, and its most useful day
    // is the one where none of its symbols has been fetched yet.
    let keys = member_keys(&[], "binance", "5m", "SOLUSDT");
    assert_eq!(keys, vec![key("SOLUSDT", "bar", Some("5m"))]);
    // `bar` is not a guess: it is the only kind an INTERVAL can mean, and it is exactly what
    // the downstream gate asks for.
    assert_eq!(backfillable(&keys), 1);
    // Both fields are trimmed on the way in, so a padded form value still makes a clean key.
    assert_eq!(member_keys(&[], " binance ", " 5m ", "SOLUSDT"), keys);
    // ⚠ NO client-side venue roster, here either: an off-roster venue still produces a key, is
    // still sent, and is answered by the SERVER's refusal naming the server's own set. That is
    // the property `backfillable`'s doc argues for and the design mock's hover text broke.
    let off_roster = member_keys(&[], "a-venue-this-build-never-heard-of", "1m", "BTCUSDT");
    assert_eq!(off_roster.len(), 1);
    assert_eq!(backfillable(&off_roster), 1);
}

#[test]
fn auto_or_a_blank_interval_synthesizes_nothing_and_the_member_counts_as_a_skip() {
    // BRANCH 3. ⚠ `Auto` is a UI sentinel, not a venue: a key carrying it would reach the
    // datahub as a venue nobody has, and the refusal coming back would read as "the server
    // cannot do this" when the truth is that this window sent a non-name. The button withholds
    // the key and its disabled hover names both form fields instead.
    assert!(member_keys(&[], "Auto", "5m", "BTCUSDT").is_empty());
    assert!(member_keys(&[], "auto", "5m", "BTCUSDT").is_empty());
    assert!(member_keys(&[], "", "5m", "BTCUSDT").is_empty());
    assert!(member_keys(&[], "   ", "5m", "BTCUSDT").is_empty());
    // An absent interval names no bar, so a real venue is not sufficient on its own.
    assert!(member_keys(&[], "binance", "", "BTCUSDT").is_empty());
    assert!(member_keys(&[], "binance", "  ", "BTCUSDT").is_empty());
    // ⚠ ...and the branch ORDER, which is the part a reader could get backwards: a STORED
    // series is still returned under `Auto` with no interval at all, because the store supplies
    // the venue and the resolution the set declined to name. Only the SYNTHESIS needs them.
    let stored = vec![(key("BTCUSDT", "bar", Some("1m")), 100 * DAY)];
    assert_eq!(
        member_keys(&stored, "Auto", "", "BTCUSDT"),
        vec![key("BTCUSDT", "bar", Some("1m"))]
    );
}

#[test]
fn the_stale_sort_presets_point_the_way_their_labels_claim() {
    // ⚠ `cmp_flat_row` compares `cov.last_ts` for `Updated` and `cov.rows` for `Rows`, so
    // "oldest first" is ASCENDING and "most rows first" is DESCENDING. Either direction
    // inverted produces a screen that is precisely backwards and looks fine.
    let (label, sort) = (STALE_SORTS[0].label, STALE_SORTS[0].value.expect("a preset"));
    assert_eq!(label, "oldest first");
    assert_eq!(sort.column, SortColumn::Updated);
    assert!(sort.ascending, "oldest first means the smallest last_ts leads");
    let (label, sort) = (STALE_SORTS[1].label, STALE_SORTS[1].value.expect("a preset"));
    assert_eq!(label, "most rows first");
    assert_eq!(sort.column, SortColumn::Rows);
    assert!(!sort.ascending, "most rows first means the largest row count leads");
}

/// One member-list cell: the symbol, or why it is marked. The old list's `· stale` / `· not stored`
/// suffixes are now a column of their own. The order is the old list's: stale wins over empty.
#[test]
fn member_cells_name_the_symbol_and_why_it_is_marked() {
    let behind: Vec<(SeriesKey, i64)> = vec![(key("DOGEUSDT", "bar", Some("1m")), 40 * DAY)];
    let stale = ("DOGEUSDT".to_string(), behind);
    let unstored = ("SOLUSDT".to_string(), Vec::new());
    let fresh = ("BTCUSDT".to_string(), vec![(key("BTCUSDT", "bar", Some("1m")), 100 * DAY)]);
    let w = (0, 100 * DAY);
    assert_eq!(member_cell(&stale, 0, w.0, w.1), "DOGEUSDT");
    assert_eq!(member_cell(&stale, 1, w.0, w.1), "stale");
    assert_eq!(member_cell(&unstored, 1, w.0, w.1), "not stored");
    assert_eq!(member_cell(&fresh, 1, w.0, w.1), "");
}
