//! The exhaustiveness gate over `vike_data::series_cadence::SERIES_CADENCE`: a stored kind cannot
//! exist without a declared cadence, and a declared cadence cannot outlive the code it was read
//! from.
//!
//! # Why a gate and not a comment
//!
//! `docs/decisions/0007-gates-not-prose.md` is the rule, and this table is the textbook case: every
//! number in it is a hand copy of something three crates away — a stream name inside a `format!`,
//! a measurement in a spec, a `narrow` that silently drops a stream. The failure this whole table
//! exists for was itself a hand copy rotting in silence for forty days, so a prose table would be
//! the same defect wearing the uniform of the fix.
//!
//! So the table is checked against the CODE, in the shape
//! `crates/vike-data/tests/store_kind_gate.rs` established for the layout table beside it: derive
//! the live set from the tree, then demand a declared row for each entry. A declared list checked
//! against itself gates nothing.
//!
//! # What is derived, and what each derivation is blind to
//!
//! 1. [`every_store_kind_has_a_default_cadence_row`] — the kind roster, from
//!    `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS`. Blind to a venue.
//! 2. [`every_recordable_venue_lane_has_a_refining_row`] — the (venue, kind) pairs a recorder can
//!    actually subscribe TODAY, derived from the venue modules under
//!    `crates/vike-recorder/src/venues/` crossed with each venue's own
//!    `vike_model::venue_caps::caps_for` row. Blind to a kind nothing records.
//! 3. [`no_row_claims_a_lane_its_venue_does_not_serve`] — the inverse, so a row cannot invent a
//!    lane. `NotServed` is therefore DERIVED and is never typed into the table.
//! 4. [`row_prose_cites_things_that_exist`] — the citations inside the row STRING literals, which
//!    `crates/vike-ops/tests/citation_gate.rs` structurally cannot see (it reads `//` lines only).
//!
//! # And four PINNED CONTRADICTIONS
//!
//! Step 1 of the playbook writes down today's reality including the places two sites disagree, and
//! pins each so that whoever fixes one is told to update the table rather than leaving it stale.
//! Each of the four tests below asserts a DEFECT still exists. **A red one is good news** — read
//! its message, it names the row to update.
//!
//! Text-only, like the layout gate beside it: no cargo invocation, no engine, no new dependency.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use vike_data::coverage::TICK_KINDS;
use vike_data::series_cadence::{Cadence, SERIES_CADENCE, SeriesCadence};
use vike_data::store_kind::STORE_KINDS;

// ---- the files this gate reads ---------------------------------------------------------------

/// The workspace root — `crates/vike-data/../..`. Every path below is repo-relative, so a file
/// MOVE between crates turns this gate red rather than silently unchecking a claim.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn read_rel(rel: &str) -> String {
    read(&repo_root().join(rel))
}

/// `needle` occurs in `hay` as a WHOLE token, not as a fragment of a longer identifier. Copied
/// deliberately from `crates/vike-data/tests/store_kind_gate.rs` rather than shared: an integration
/// test is its own crate, and a shared helper would need a `test-support` surface for two functions.
fn contains_token(hay: &str, needle: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    hay.match_indices(needle).any(|(i, _)| {
        let before = hay[..i].chars().next_back().is_some_and(ident);
        let after = hay[i + needle.len()..].chars().next().is_some_and(ident);
        !before && !after
    })
}

/// The backtick-delimited spans of `text`, each paired with the text immediately AFTER it — which
/// is how a `` `path`'s `SYMBOL` `` citation is recognised.
fn backticked(text: &str) -> Vec<(&str, &str)> {
    let parts: Vec<&str> = text.split('`').collect();
    assert!(
        parts.len() % 2 == 1,
        "unbalanced backticks in row prose — a citation cannot be read:\n{text}"
    );
    parts
        .iter()
        .skip(1)
        .step_by(2)
        .zip(parts.iter().skip(2).step_by(2))
        .map(|(t, a)| (*t, *a))
        .collect()
}

/// A row's human name, for an assertion message.
fn id(c: &SeriesCadence) -> String {
    match c.venue {
        Some(v) => format!("{}/{v}", c.kind),
        None => format!("{} (default)", c.kind),
    }
}

// ---- 1. the kind roster ------------------------------------------------------------------------

/// Every kind the store writes has a DEFAULT cadence row.
///
/// This is the completeness property the playbook asks for: a new `kind=` reddens here until
/// somebody classifies it, exactly as it reddens `store_kind_gate.rs` until somebody declares its
/// layout. It is derived from `STORE_KINDS` rather than from a second list, because `STORE_KINDS`
/// is itself derived from the code that writes the store.
#[test]
fn every_store_kind_has_a_default_cadence_row() {
    for k in STORE_KINDS {
        let found = SERIES_CADENCE.iter().any(|c| c.kind == k.kind && c.venue.is_none());
        assert!(
            found,
            "`kind={}` is written by this store and has no DEFAULT row in \
             `vike_data::series_cadence::SERIES_CADENCE`. Add one: say whether its rate is \
             Sampled (a clock declares a ceiling), EventDriven (the market decides) or Collected \
             (a batch job decides), and cite where you read that. If you do not know, \
             `Cadence::EventDriven` is the honest answer — it yields no ceiling, so nothing can \
             threshold it.",
            k.kind
        );
    }
}

/// ...and no row invents a kind the store does not write.
#[test]
fn every_cadence_row_names_a_stored_kind() {
    let kinds: BTreeSet<&str> = STORE_KINDS.iter().map(|k| k.kind).collect();
    for c in SERIES_CADENCE {
        assert!(
            kinds.contains(c.kind),
            "{}: `kind={}` is not in `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS`. \
             A cadence for a kind the store never writes is a row nothing can ever consult.",
            id(c),
            c.kind
        );
    }
}

// ---- 2 & 3. the venue lanes --------------------------------------------------------------------

/// The venues a recorder can mount today, derived from the modules under
/// `crates/vike-recorder/src/venues/` rather than from a list here.
///
/// Each module declares its own `pub const VENUE`, and that string is the `venue=` segment of every
/// key it produces — so this is the same "read the tree, then demand a row" shape the layout gate
/// uses, and a new venue module joins the derived set the moment it lands.
fn recordable_venues() -> BTreeSet<String> {
    let dir = repo_root().join("crates").join("vike-recorder").join("src").join("venues");
    let mut out = BTreeSet::new();
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .filter_map(Result::ok);
    for entry in entries {
        let path = entry.path();
        if path.file_name().and_then(|f| f.to_str()) == Some("mod.rs") {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let src = read(&path);
        let Some(rest) = src.split("pub const VENUE: &str = \"").nth(1) else { continue };
        let Some(venue) = rest.split('"').next() else { continue };
        out.insert(venue.to_string());
    }
    assert!(
        out.len() >= 2,
        "the recorder-venue derivation collapsed to {out:?} — it used to find at least binance and \
         polymarket. If a venue module moved, fix the walk; do not lower the floor."
    );
    out
}

/// Whether `venue` serves `kind` as a LIVE lane, straight off `vike_model::venue_caps` so this
/// gate and the capability roster cannot disagree.
fn serves(venue: &str, kind: &str) -> bool {
    let d = vike_model::caps_for(venue).live_data;
    match kind {
        "quote" => d.quotes,
        "trade" => d.trades,
        "book" => d.book,
        "depth" => d.depth,
        _ => false,
    }
}

/// Every (venue, kind) lane a recorder can subscribe TODAY has its own refining row.
///
/// The kind default is a safe fallback but a poor declaration: it cannot carry a venue's stream
/// interval, and the thin-pair measurement in the table's module doc is the reason a ceiling has to
/// be read per venue. So a lane that actually records is required to have been LOOKED AT.
#[test]
fn every_recordable_venue_lane_has_a_refining_row() {
    for venue in recordable_venues() {
        for kind in TICK_KINDS {
            if !serves(&venue, kind) {
                continue;
            }
            let found = SERIES_CADENCE.iter().any(|c| c.kind == kind && c.venue == Some(&*venue));
            assert!(
                found,
                "`kind={kind}/venue={venue}` is a lane this recorder subscribes \
                 (`crates/vike-recorder/src/venues/` has a module for it and \
                 `vike_model::venue_caps` says the venue serves it) and \
                 `vike_data::series_cadence::SERIES_CADENCE` has no row for it. Declare one, with \
                 the code or the measurement you read it from."
            );
        }
    }
}

/// ...and no refining row claims a lane its venue does not serve.
///
/// This is what makes the NotServed cells DERIVED rather than typed: the table never states that
/// binance serves no `book`, it simply cannot carry a row saying it does. Keeping that judgement in
/// `vike_model::venue_caps` means one roster, not two.
#[test]
fn no_row_claims_a_lane_its_venue_does_not_serve() {
    for c in SERIES_CADENCE {
        let Some(venue) = c.venue else { continue };
        assert!(
            vike_model::VENUES.contains(&venue),
            "{}: `{venue}` is not in `crates/vike-model/src/venues.rs`'s `VENUES`",
            id(c)
        );
        if !TICK_KINDS.contains(&c.kind) {
            continue;
        }
        assert!(
            serves(venue, c.kind),
            "{}: `vike_model::venue_caps` says {venue} serves no live `{}` lane, so no series key \
             of this shape can exist. Either the caps row is wrong or this row is.",
            id(c),
            c.kind
        );
    }
}

/// A non-tick kind is `Collected` by construction: nothing subscribes it, so it can never reach a
/// live watchdog whatever a row claims.
#[test]
fn every_non_tick_kind_is_collected() {
    for c in SERIES_CADENCE {
        if TICK_KINDS.contains(&c.kind) {
            continue;
        }
        assert_eq!(
            c.cadence,
            Cadence::Collected,
            "{}: `{}` is not a tick lane (`vike_data::coverage::TICK_KINDS`), so no live \
             subscription produces it and its cadence is a collector's window. If that changed, \
             the change is in `crates/vike-recorder/src/session.rs`'s `Stream` and this gate is \
             the thing telling you to say so here.",
            id(c),
            c.kind
        );
    }
}

// ---- 4. the citations --------------------------------------------------------------------------

/// Every repo-anchored path a row's PROSE cites exists, and a `` `path`'s `SYMBOL` `` citation
/// names something that file still contains.
///
/// `crates/vike-ops/tests/citation_gate.rs` enforces exactly this repo-wide — but only on lines
/// whose trimmed form starts with `//`, and every word of [`SeriesCadence::evidence`],
/// [`SeriesCadence::measured`] and [`SeriesCadence::notes`] lives inside a STRING literal. So the
/// three fields carrying the entire justification for every number here are the three the repo-wide
/// gate cannot see, which is the worst pairing available.
#[test]
fn row_prose_cites_things_that_exist() {
    const ANCHORS: [&str; 5] = ["crates/", "docs/", "scripts/", "deploy/", ".github/"];
    let root = repo_root();
    let mut checked = 0usize;
    for c in SERIES_CADENCE {
        let fields = [("evidence", c.evidence), ("measured", c.measured), ("notes", c.notes)];
        for (field, text) in fields {
            let toks = backticked(text);
            for (i, (tok, after)) in toks.iter().enumerate() {
                if !ANCHORS.iter().any(|a| tok.starts_with(a)) {
                    continue;
                }
                let path = root.join(tok);
                assert!(path.exists(), "{}.{field}: cited path {tok} does not exist", id(c));
                checked += 1;
                if !after.trim_start().starts_with("'s") {
                    continue;
                }
                let Some((symbol, _)) = toks.get(i + 1) else { continue };
                assert!(
                    path.is_file() && contains_token(&read(&path), symbol),
                    "{}.{field}: `{tok}` no longer contains `{symbol}`",
                    id(c)
                );
            }
        }
    }
    assert!(checked >= 40, "the row-prose citation scan collapsed to {checked} citations");
}

// ---- the PINNED CONTRADICTIONS -----------------------------------------------------------------

/// **PINNED:** the polymarket `quote` lane is written but is in no watchdog's expected set.
///
/// `narrow` drops `Stream::Quotes` whenever `Stream::Book` is requested — correctly, because the
/// book pump already emits the derived L1 — so the recorder writes `kind=quote/venue=polymarket`
/// rows (329 M of them in 40 days) under a series key `expected_series` never names. Neither the
/// recency watchdog nor any rate check can see that lane.
///
/// A RED here means somebody fixed it. Update the `quote`/`polymarket` row's `notes`, and note that
/// the lane has become judgeable.
#[test]
fn the_polymarket_quote_lane_is_still_invisible_to_every_watchdog() {
    let src = read_rel("crates/vike-recorder/src/venues/polymarket.rs");
    assert!(
        src.contains("has_book && *s == Stream::Quotes"),
        "`crates/vike-recorder/src/venues/polymarket.rs`'s `narrow` no longer drops Quotes when \
         Book is requested. The `quote`/`polymarket` row in \
         `vike_data::series_cadence::SERIES_CADENCE` pins that it does — update it."
    );
}

/// **PINNED:** the offline quality scorer cannot score the kind that broke.
///
/// `crates/vike-data/src/quality.rs` has `QualityLane::{Book, Quote, Trade}` and no `Depth`, so the
/// lane that recorded at 4 % for forty days is one it structurally cannot look at. This is pinned
/// rather than fixed because step 1 changes no behaviour — and because the fix belongs with the
/// second consumer of this table, not beside its declaration.
#[test]
fn the_quality_scorer_still_has_no_depth_lane() {
    let src = read_rel("crates/vike-data/src/quality.rs");
    assert!(
        !contains_token(&src, "Depth"),
        "`crates/vike-data/src/quality.rs` has grown a Depth lane. That is the fix this row was \
         waiting for — wire its `max_gap_ms` to \
         `vike_data::series_cadence::cadence_for(kind, venue)` and delete this pin."
    );
}

/// **PINNED, with the arithmetic:** the quality scorer's one threshold would have scored the broken
/// lane PERFECT.
///
/// `QualityConfig::default`'s `max_gap_ms` is 60,000 and only a segment STRICTLY LONGER than it
/// counts as uncovered. The broken binance depth lane's worst measured inter-arrival was 17,864 ms
/// (§12.2 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`), so no segment
/// ever exceeded the threshold: `uncovered_ms == 0`, `gap_pct == 0.0`, `is_perfect() == true`, for
/// forty days at 4 % of the true rate.
///
/// This is the single most important sentence in the whole table's justification — that a
/// gap-based check cannot see a rate collapse — so it is asserted rather than written down.
#[test]
fn the_default_gap_threshold_would_score_the_broken_lane_perfect() {
    /// The worst inter-arrival measured on the broken lane, in ms. §12.2 of the wire spec.
    const BROKEN_LANE_WORST_INTER_ARRIVAL_MS: i64 = 17_864;

    // Read the real threshold out of the source rather than restating it here — a second copy of
    // the number is the failure mode this whole table exists for.
    let src = read_rel("crates/vike-data/src/quality.rs");
    let declared_ms: i64 = src
        .split("Self { max_gap_ms: ")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit() && c != '_').next())
        .and_then(|n| n.replace('_', "").parse().ok())
        .expect(
            "`crates/vike-data/src/quality.rs`'s `QualityConfig` default no longer spells \
             `Self { max_gap_ms: <number> }`, which is how this pin reads the real value",
        );
    assert!(
        BROKEN_LANE_WORST_INTER_ARRIVAL_MS < declared_ms,
        "the gap threshold is now {declared_ms} ms, which the broken lane's worst measured \
         inter-arrival of {BROKEN_LANE_WORST_INTER_ARRIVAL_MS} ms WOULD have exceeded — so a \
         gap-based scorer would have caught it after all, and the cadence table's premise needs \
         re-arguing rather than this test relaxing"
    );
}

/// **PINNED:** the `depth` lane counts stream-health MARKERS as rows, so a reconnect loop inflates
/// the very number a rate check reads.
///
/// `stream_status` routes `GapStart`/`Stale`/`LiveResume` into `Msg::Depth`, which reaches `ingest`
/// and bumps `Liveness::rows`. Its own doc measures ~20,000 reconnect cycles a day on the broken
/// lane. Any consumer of this table must leave margin for that; a floor derived as a fraction of a
/// declared ceiling does, and an absolute floor sized to the observed broken rate does not.
#[test]
fn a_status_marker_still_counts_as_a_depth_row() {
    let src = read_rel("crates/vike-data/src/live_rec.rs");
    assert!(
        contains_token(&src, "stream_status"),
        "`crates/vike-data/src/live_rec.rs`'s `stream_status` is gone — the marker-inflation \
         paragraph in `vike_data::series_cadence`'s module doc cites it."
    );
    assert!(
        src.contains("\"depth\" => self.send(Msg::Depth"),
        "`crates/vike-data/src/live_rec.rs`'s `stream_status` no longer routes depth markers into \
         the depth lane. If markers stopped counting toward `Liveness::rows`, the margin argument \
         in `vike_data::series_cadence`'s module doc is stale — update it."
    );
}
