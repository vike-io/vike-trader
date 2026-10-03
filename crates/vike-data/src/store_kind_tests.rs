use super::*;

#[test]
fn no_duplicate_kind_ids() {
    let mut ids: Vec<&str> = STORE_KINDS.iter().map(|k| k.kind).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), before, "duplicate kind id in STORE_KINDS");
}

/// A kind id is a path segment (`kind=<id>`), so it must survive being written into a
/// directory name on every platform and be matched byte-for-byte by every runtime dispatch.
#[test]
fn kind_ids_are_lowercase_ascii_snake() {
    for k in STORE_KINDS {
        assert!(!k.kind.is_empty());
        assert!(
            k.kind.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "kind id {:?} must be lowercase ascii with underscores",
            k.kind
        );
    }
}

/// Kinds this workspace declares but no code currently WRITES, each with the reason — a row here is
/// a claim somebody had to write down (the `NOT_DISPATCHED` idiom). Every other kind must name at
/// least one producer commit-key shape, and a kind listed here must name none: a producer coming
/// back makes its row stale, and the test says so.
const KINDS_WITHOUT_A_PRODUCER: &[(&str, &str)] = &[(
    "exec_funding",
    "its only writer, hyperliquid_funding_backfill, was deleted by docs/decisions/0094 (measured \
     unused); an account-history import returns with the `vike-cli account` plane",
)];

/// Every row is fully filled in. A blank field is the failure mode a declaration table is
/// most prone to: the row exists, so the completeness gate is satisfied, and it says nothing.
///
/// The one field this loop does NOT demand unconditionally is `commit_keys`: a kind named in
/// [`KINDS_WITHOUT_A_PRODUCER`] must have NONE (the declared, reasoned exception), and every other
/// kind must have at least one — both directions checked, so a producer coming back leaves a stale
/// exemption rather than a silent pass.
#[test]
fn every_row_is_populated() {
    for k in STORE_KINDS {
        let kind = k.kind;
        assert!(!k.row.is_empty(), "{kind}: no row type");
        assert!(!k.codec.is_empty(), "{kind}: no codec");
        assert!(k.write_verb.starts_with("append_"), "{kind}: write verb {:?}", k.write_verb);
        assert!(!k.read_verb.is_empty(), "{kind}: no read verb");
        assert!(!k.columns.is_empty(), "{kind}: no columns");
        assert!(k.identity.len() > 20, "{kind}: identity must SAY something");
        assert!(k.notes.len() > 20, "{kind}: notes must SAY something");
        match KINDS_WITHOUT_A_PRODUCER.iter().find(|(name, _)| *name == kind) {
            Some((_, why)) => assert!(
                k.commit_keys.is_empty(),
                "{kind} names a producer again — delete its KINDS_WITHOUT_A_PRODUCER row ({why})"
            ),
            None => assert!(!k.commit_keys.is_empty(), "{kind}: no producer commit-key shape"),
        }
    }
}

/// [`KINDS_WITHOUT_A_PRODUCER`]'s own two-directional check, the same shape
/// `VERB_CALL_EXEMPT`'s own reason floor takes in `crates/vike-data/tests/store_kind_gate.rs`: every
/// name it lists is a REAL `STORE_KINDS` kind (a row surviving a rename or removal would silently
/// excuse nothing, forever), and every reason actually SAYS something rather than sitting there as a
/// placeholder.
#[test]
fn kinds_without_a_producer_are_real_and_reasoned() {
    for (kind, why) in KINDS_WITHOUT_A_PRODUCER {
        assert!(
            STORE_KINDS.iter().any(|k| k.kind == *kind),
            "KINDS_WITHOUT_A_PRODUCER names {kind:?}, which STORE_KINDS does not carry — a stale row"
        );
        assert!(why.len() > 20, "{kind}: a KINDS_WITHOUT_A_PRODUCER reason must SAY something");
    }
}

/// Every column list starts with `ts`, and no kind repeats a column name.
///
/// `ts` first is not cosmetic: `commit_rows` splits a batch into `date=` partitions by the
/// `ts` vector, the manifest's `[ts_min, ts_max]` prune reads that column's row-group
/// statistics, and every codec's sort key is `(ts, …)`. A kind whose first column were
/// something else would still work and would quietly lose the pruning.
#[test]
fn every_kind_leads_with_ts_and_repeats_no_column() {
    for k in STORE_KINDS {
        assert_eq!(k.columns[0].0, "ts", "{}: first column must be ts", k.kind);
        let mut names: Vec<&str> = k.columns.iter().map(|c| c.0).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "{}: duplicate column name", k.kind);
    }
}

/// Only `bar` sub-partitions by interval, and only the kinds carrying a row-level `symbol_col`
/// can have a grouped form — a grouped part holds many symbols and has no path left to tell
/// them apart by.
#[test]
fn grouping_requires_a_row_level_symbol_column() {
    for k in STORE_KINDS {
        let has_symbol_col = k.columns.iter().any(|c| c.0 == "symbol_col");
        assert!(
            !k.grouped || has_symbol_col,
            "{}: declared grouped without a symbol_col column",
            k.kind
        );
        assert_eq!(
            k.partition == Partition::SymbolInterval,
            k.kind == "bar",
            "{}: interval sub-partitioning is the bar kind's alone",
            k.kind
        );
    }
}

/// The prefix is the literal head of the template, and an interpolation-first template has
/// none — the distinction the whole classifier rests on.
#[test]
fn a_prefix_is_the_literal_head_and_an_empty_one_is_none() {
    // ⚠ The WHOLE literal head, not the first segment: `pmxt:quote:` and `pmxt:trade:` are
    // different producers of different kinds, and folding them to `pmxt:` would let a
    // `--produced-by` aimed at one assert nothing about the other.
    assert_eq!(commit_key_prefix("pmxt:quote:{asset}:{hour}"), Some("pmxt:quote:"));
    assert_eq!(commit_key_prefix("live-{venue}-{symbol}-{}-{first_ts}"), Some("live-"));
    assert_eq!(commit_key_prefix("demo-tape:v{DEMO_TAPE_VERSION}:{}:{}"), Some("demo-tape:v"));
    assert_eq!(commit_key_prefix("{venue}:{symbol}:{date}"), None);
    assert_eq!(commit_key_prefix(""), None);
    // No interpolation at all is a whole-string prefix, not a `None`.
    assert_eq!(commit_key_prefix("fixed-key"), Some("fixed-key"));
}

/// **The NAMED-EXCEPTION gate.** The templates with no namespace prefix are EXACTLY the two
/// [`PREFIXLESS_TEMPLATES`] declares — both directions, so a third one cannot join the
/// classifier's blind spot silently and a template that gains a prefix cannot leave a stale
/// exemption behind.
#[test]
fn the_prefixless_templates_are_exactly_the_declared_ones() {
    // ⚠ `source` joined the compared TUPLE when the field landed, and that is not decoration:
    // these rows are a second COPY of a `STORE_KINDS` row, so a copy whose lane disagreed with
    // the original would leave two answers to "which lane writes this producer's keys" while
    // this gate stayed green on the two fields it used to read.
    let mut found: Vec<(&str, &str, &str)> = Vec::new();
    for k in STORE_KINDS {
        for ck in k.commit_keys {
            if commit_key_prefix(ck.template).is_none() {
                found.push((ck.producer, ck.template, ck.source));
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    let mut declared: Vec<(&str, &str, &str)> =
        PREFIXLESS_TEMPLATES.iter().map(|c| (c.producer, c.template, c.source)).collect();
    declared.sort_unstable();
    assert_eq!(
        found, declared,
        "a commit-key template with no namespace prefix is a producer whose keys cannot be \
             told from anyone else's. Give it a prefix, or add it to PREFIXLESS_TEMPLATES with \
             that trade written down."
    );
}

/// A key classifies to the producers whose prefix it carries, and to nothing else. The EMPTY
/// answer is the removed-producer case this whole feature exists for — not an error.
#[test]
fn a_key_names_its_declared_producers_and_a_removed_one_names_none() {
    let pmxt = producers_for_key("pmxt:quote:0x1234:2026-09-07T10");
    assert!(
        pmxt.iter().any(|(kind, ck)| *kind == "quote"
            && ck.producer == "crates/vike-backfill/src/pmxt/ingest.rs"),
        "{pmxt:?}"
    );
    assert!(
        producers_for_key("panel_bars:hyperliquid:BTC:1h").is_empty(),
        "a REMOVED producer's key must classify as unknown rather than as somebody else's"
    );
    // The prefix-less klines template cannot be matched by PREFIX (an empty prefix would match
    // everything), but a key of its exact SHAPE names it — that is how a day-chunked OANDA series
    // stops reading "no declared producer" on every key.
    for key in ["binance:BTCUSDT:1h:0-1", "oanda:EUR_USD:5s:1441756800000-1441843199999"] {
        let named: Vec<&str> = producers_for_key(key).iter().map(|(_, ck)| ck.producer).collect();
        assert_eq!(named, ["crates/vike-backfill/src/klines.rs"], "{key}");
    }
    // A window's EMPTY MARKER names the same producer through its OWN template, and a window key
    // never names the marker's — one template per key, so neither is reported twice.
    for (key, marker) in [("oanda:EUR_USD:5s:0-1", false), ("oanda:EUR_USD:5s:0-1:empty", true)] {
        let hits = producers_for_key(key);
        assert_eq!(hits.len(), 1, "{key}: {hits:?}");
        assert_eq!(hits[0].1.producer, "crates/vike-backfill/src/klines.rs", "{key}");
        assert_eq!(hits[0].1.template.ends_with(EMPTY_MARKER_SUFFIX), marker, "{key}");
    }
    // The other two prefix-less templates still can never be an answer.
    for key in ["yahoo:AAPL:5y:2026-09-30", "binance:BTCUSDT:2026-09-30"] {
        assert!(producers_for_key(key).is_empty(), "{key}");
    }
}

/// [`venue_window_key_prefix`] recognises a venue window key by SHAPE and refuses everything that
/// merely resembles one — each refusal below is a real key family that would otherwise be swept
/// into the venue-klines group.
#[test]
fn a_venue_window_key_is_recognised_by_shape_and_look_alikes_are_refused() {
    assert_eq!(
        venue_window_key_prefix("oanda:EUR_USD:5s:1441756800000-1441843199999"),
        Some("oanda:EUR_USD:5s:")
    );
    assert_eq!(venue_window_key_prefix("binance:BTCUSDT:1h:0-1"), Some("binance:BTCUSDT:1h:"));
    // A window's empty marker is the same shape plus one `empty` field, and folds to the SAME
    // prefix — its series' one group.
    assert_eq!(
        venue_window_key_prefix("oanda:EUR_USD:5s:1441756800000-1441843199999:empty"),
        Some("oanda:EUR_USD:5s:")
    );
    for refused in [
        "oanda:EUR_USD:5s:0-1:empty:empty", // a marker of a marker is no shape anyone builds
        "oanda:EUR_USD:5s:empty",           // a marker with no window
        "nobody:EUR_USD:5s:0-1:empty",      // a marker still needs a roster venue
        "nobody:EUR_USD:5s:0-1",            // first field is not a roster venue
        "oanda:EUR_USD:5s",                 // 3 fields: the properties-record shape, not a window
        "oanda:EUR_USD:5s:0-1:extra",       // 5 fields
        "oanda:EUR_USD:5s:abc-def",         // window is not numeric
        "oanda:EUR_USD:5s:0-",              // half a window
        "oanda::5s:0-1",                    // empty symbol
        "oanda:EUR_USD::0-1",               // empty interval
        "yahoo:AAPL:5y:2026-09-30",         // the eod key: a date, not a millisecond window
        "funding_rate:binance:BTCUSDT:0-1", // another producer's namespace
        "panel_bars:hyperliquid:BTC:1h",    // a removed producer's key
    ] {
        assert_eq!(venue_window_key_prefix(refused), None, "{refused}");
    }
}

/// A key resolves to the LANE that wrote it, and the three ways it resolves to none are kept
/// apart because they mean different things.
#[test]
fn a_key_names_its_lane_and_three_different_absences_name_none() {
    assert_eq!(source_for_key("pmxt:quote:0x1234:2026-09-07T10"), Some("pmxt"));
    assert_eq!(source_for_key("pmxt:book:0x1234:2026-09-07T10"), Some("pmxt"));
    assert_eq!(source_for_key("live-binance-BTCUSDT-q-1-2-3"), Some("recorder"));
    assert_eq!(source_for_key("perp_metrics:hyperliquid:BTC:0-1"), Some("venue"));
    assert_eq!(source_for_key("perp_panel:hyperliquid:BTC:0-1"), Some("vikedata-panel"));
    // 1. a REMOVED producer — the 2026-09-07 the CI box cleanup's keys, which are nobody's now.
    assert_eq!(source_for_key("panel_bars:hyperliquid:BTC:1h"), None);
    // 2. a producer nothing declares.
    assert_eq!(source_for_key("nobody-writes-this:1"), None);
    // 3. a PREFIXLESS producer: its keys carry no namespace, so there is nothing to match. This
    //    key is a real `klines.rs` shape and it still classifies as unknown.
    assert_eq!(source_for_key("binance:BTCUSDT:1h:0-1"), None);
}

/// ⚠ **The LONGEST-prefix rule in [`source_for_key`] is INERT on today's table, and this test
/// is what says so out loud rather than leaving a green to imply it was exercised.**
///
/// No declared prefix is a proper prefix of another, so every key matches at most one ROW
/// SHAPE and shortest-wins, first-wins and longest-wins would all agree. (Several rows do share
/// a prefix EXACTLY — `live-` is declared by five kinds — and that is the sibling invariant,
/// held by `crates/vike-data/tests/store_kind_gate.rs`'s
/// `no_two_declared_rows_share_a_prefix_while_naming_different_lanes`, which is what makes the
/// classifier well-defined where this test says nothing.) The rule exists for the case the
/// design measured — `clickhouse:quote:` and `clickhouse:spot:`, two DIFFERENT lanes under one
/// namespace, told apart only by the second segment — and those two rows were deleted on
/// 2026-09-20. When a nesting pair returns, THIS assertion is what reddens, and the author is
/// told to write the test the rule then deserves.
#[test]
fn no_declared_prefix_nests_inside_another_so_longest_wins_is_untested_by_construction() {
    let prefixes: Vec<(&str, &str)> = STORE_KINDS
        .iter()
        .flat_map(|k| k.commit_keys.iter())
        .filter_map(|c| commit_key_prefix(c.template).map(|p| (p, c.source)))
        .collect();
    // Anti-vacuity: a derivation that collapsed to nothing would satisfy every pair below.
    assert!(prefixes.len() >= 20, "the prefix derivation collapsed to {prefixes:?}");
    for (a, sa) in &prefixes {
        for (b, sb) in &prefixes {
            if a == b {
                continue;
            }
            assert!(
                !b.starts_with(a),
                "{b:?} ({sb}) nests inside {a:?} ({sa}), so `source_for_key`'s longest-wins                      rule now DECIDES an answer instead of merely agreeing with every other rule.                      Write the test that pins which lane wins, then delete this assertion's claim                      that the rule is inert."
            );
        }
    }
}

/// A source id becomes a `source=` PATH SEGMENT, so it takes the rule `kind` ids already take.
#[test]
fn every_source_id_is_a_path_safe_lowercase_token() {
    let ids = source_ids();
    // Anti-vacuity: an empty roster would pass the loop below without examining anything.
    assert!(ids.len() >= 10, "the source vocabulary collapsed to {ids:?}");
    for id in &ids {
        assert!(!id.is_empty(), "an EMPTY source id matches every key: {ids:?}");
        assert!(
            id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{id:?} is not a path-safe lowercase token — it becomes a directory NAME"
        );
    }
}

/// ⚠ **THE PINNED CONTRADICTION.** `kind=perp_metrics` declares TWO producers naming TWO
/// lanes, and both write the SAME leaf — one `_manifest.json`, one commit log, one lock —
/// because no producer is scoped and nothing writes a `source=` segment yet.
///
/// This is the accident the reverse gate was built to catch, measured on the CI box 2026-09-07 and
/// cleaned up there the same day. Step 1 of the per-venue-table playbook is to DECLARE today's
/// reality including the contradiction, so this asserts the contradiction still EXISTS. When
/// somebody scopes the kind, this test is what tells them the table is now their problem.
#[test]
fn perp_metrics_still_names_two_lanes_that_share_one_leaf() {
    assert_eq!(
        sources_for_kind("perp_metrics"),
        vec!["venue", "vikedata-panel"],
        "the two `perp_metrics` producers are the collision this dimension exists to end. If              one of them is gone, delete this test; if a THIRD arrived, say which lane it is."
    );
    // The dedup is real work, not an identity: `bar` declares SIX rows and FIVE lanes, because
    // `klines.rs` and `funding_rate.rs` are both the venue's own historical API. ⚠ It was seven
    // rows and six lanes until docs/decisions/0094 deleted the `ibkr` `CommitKey` (2026-09-28),
    // measured unused.
    let bars = sources_for_kind("bar");
    assert_eq!(bars.len(), 5, "{bars:?}");
    assert_eq!(bars.iter().filter(|s| **s == "venue").count(), 1, "{bars:?}");
    // A kind this store does not write has no lanes rather than a permissive answer.
    assert!(sources_for_kind("oracle").is_empty());
}

/// `--produced-by` takes a declared producer PATH or a literal prefix, and refuses the two
/// shapes that would assert nothing while looking like an assertion.
#[test]
fn produced_by_resolves_a_producer_path_or_a_literal_prefix() {
    assert_eq!(resolve_produced_by("panel_bars:").unwrap(), "panel_bars:");
    assert_eq!(resolve_produced_by("crates/vike-data/src/demo.rs").unwrap(), "demo-tape:v");
    assert_eq!(resolve_produced_by("crates/vike-data/src/cohort_rec.rs").unwrap(), "cohort:");
    // A prefix-less producer has nothing to assert against.
    let err = resolve_produced_by("crates/vike-data/src/properties_rec.rs").unwrap_err();
    assert!(err.contains("NO namespace prefix"), "{err}");
    // ⚠ A producer with SEVERAL prefixes refuses rather than picking one: the pmxt collector
    // writes `pmxt:quote:`, `pmxt:trade:` and `pmxt:book:`, and an assertion that silently
    // chose one of them would delete under a claim the operator did not make.
    let err = resolve_produced_by("crates/vike-backfill/src/pmxt/ingest.rs").unwrap_err();
    assert!(err.contains("different commit-key prefixes"), "{err}");
    // A path-shaped typo is refused rather than read as a literal that matches nothing.
    let err = resolve_produced_by("crates/vike-backfill/src/nope.rs").unwrap_err();
    assert!(err.contains("no row in STORE_KINDS"), "{err}");
}

/// ⚠ **A BLANK spelling is refused on the LITERAL branch too** — the branch that used to return
/// `Ok("")` because `""` contains no `/`, so the prefix-less-producer refusal right beside it
/// never applied.
///
/// An empty prefix does not match nothing, it matches EVERYTHING:
/// [`key_matches_prefix`] is `starts_with`, so `crate::removal::RemovalPlan::verdict` finds no
/// foreign key in any series and the assertion passes vacuously — and because the value is
/// `Some`, `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series` sweep gate stands down
/// as well. Two guards on an IRREVERSIBLE verb, both defeated by one blank token that a script
/// writes on its own from an unset variable (`--produced-by="$PREFIX"`, `--produced-by
/// "$PREFIX"`).
#[test]
fn a_blank_produced_by_is_refused_rather_than_matching_every_key() {
    for blank in ["", " ", "\t", "  \n "] {
        let err = resolve_produced_by(blank)
            .expect_err("a blank --produced-by must not resolve to a prefix");
        assert!(
            err.contains("matches every key"),
            "the refusal must say WHY a blank prefix is not an assertion: {err}"
        );
    }
    // …and the shortest non-blank literal still resolves, so this is a blank check and not a
    // minimum-length one.
    assert_eq!(resolve_produced_by(":").unwrap(), ":");
}

#[test]
fn lookup_finds_a_row_and_refuses_an_unknown_kind() {
    assert_eq!(store_kind("book").map(|k| k.codec), Some("BookCodec"));
    assert!(store_kind("filters").is_none(), "the pre-rename spelling is not a live kind");
    assert!(store_kind("").is_none());
    assert_eq!(kind_ids().count(), STORE_KINDS.len());
}
