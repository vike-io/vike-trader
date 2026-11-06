//! PROVISIONAL COMMITS: a one-chunk window fetched early, then again once it has settled.

use super::*;

// ── PROVISIONAL COMMITS: a one-chunk window fetched early, then again once it has settled ────
//
// Closes item 11 (docs/superpowers/specs/2026-09-29-provisional-commits-design.md): before this,
// a window whose whole span fit inside ONE chunk kept the SAME keys whether fetched while recent
// or once settled, so an early partial fetch could permanently spend the key a later, complete
// fetch needed. Settledness is judged against the real wall clock, so — like the RECENT CHUNKS
// tests above — these place themselves relative to today's UTC midnight. (A recent one-chunk
// window takes the very `StoreMode::Recent` path every recent chunk of a longer window does; these
// tests predate that merge, and pin the one-chunk case it started from.)

/// The core scenario item 11 describes: a window fetched once while still recent (partial data —
/// the feed hasn't finished publishing) and once more after settling recovers the COMPLETE data
/// with no inflated volume — where before this fix the settled pass would have silently written
/// nothing, because both passes shared the same key.
#[test]
fn a_settled_one_chunk_window_recovers_data_missed_by_an_earlier_recent_fetch() {
    let (_dir, hist) = open();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let yesterday = today - MS_PER_DAY;
    let full_tape = tape_between(yesterday, today - 1);

    // Fetched while still recent — only the early ticks have "arrived". This is a real call and
    // really exercises `StoreMode::Recent`, storing under the provisional key.
    let early = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        yesterday,
        today - 1,
        &|| false,
        |_, from, to| Ok(tape_between(from, to.min(yesterday + 80_000))),
    )
    .unwrap();
    assert_eq!(
        early, 2,
        "the 10s and 70s ticks have arrived, landing in two different minute buckets — two bars"
    );

    // Simulate the window having settled: the exact canonical/provisional keys the settled
    // branch of `backfill_quotes_then_bars` itself would use for this same `(symbol, from, to)`,
    // now with the complete tape.
    let key = quote_commit_key(SYMBOL, yesterday, today - 1);
    let provisional = provisional_quote_commit_key(SYMBOL, yesterday, today - 1);
    let quote_rows = hist
        .append_quotes_superseding(VENUE, SYMBOL, &full_tape, Some(&key), Some(&provisional))
        .unwrap();
    assert_eq!(quote_rows, full_tape.len(), "the complete tape is written");

    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(
        quotes.len(),
        full_tape.len(),
        "exactly the complete tape is stored — the provisional rows were REPLACED, not kept \
         alongside it (this is the assertion that would have failed before this fix: the settled \
         write would have found the shared key already spent and written nothing, leaving only \
         the early PARTIAL tape stored forever)"
    );
}

/// Two different recent requests over the same one-chunk window, with different raw bounds, each
/// mint their own provisional key and both legitimately store — the provisional key must
/// incorporate the request's own bounds, not collapse to one shared bucket per symbol. Both
/// windows here are short enough that `tape_between` gives them the SAME two ticks (10s, 70s;
/// the day-end-30s tick falls outside both), so the exact count that distinguishes "both fetches
/// really ran" from "the second was wrongly treated as already-done" is precisely DOUBLE one
/// fetch's own tick count, not merely "at least as many" — a weaker bound would pass even if the
/// second request silently wrote nothing.
#[test]
fn two_different_recent_one_chunk_requests_each_get_their_own_provisional_key() {
    let (_dir, hist) = open();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let yesterday = today - MS_PER_DAY;
    let one_fetch_ticks = tape_between(yesterday, yesterday + 100_000).len();

    backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        yesterday,
        yesterday + 100_000,
        &|| false,
        |_, from, to| Ok(tape_between(from, to)),
    )
    .unwrap();
    backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        yesterday,
        yesterday + 200_000,
        &|| false,
        |_, from, to| Ok(tape_between(from, to)),
    )
    .unwrap();

    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(
        quotes.len(),
        2 * one_fetch_ticks,
        "both recent requests stored their own copy of the ticks under DIFFERENT provisional \
         keys — the accepted duplicate-for-overlapping-requests cost, proving neither was \
         silently skipped as 'already provisional'"
    );
}

/// The gap the test above (and its sibling above it) both leave: both simulate "settled" by
/// calling `append_quotes_superseding` directly with hand-built keys — neither ever drives
/// `backfill_quotes_then_bars` itself through the real `StoreMode::Settled` arm with a provisional
/// entry already sitting in the store. This test does exactly that, on BOTH the quote and the bar
/// side: it PLANTS what a prior recent-fetch pass would have left (a provisional quote commit and
/// a provisional resample, under the real provisional key formatters — precisely what
/// `StoreMode::Recent` itself would have written), then calls the REAL
/// `backfill_quotes_then_bars` over a genuinely SETTLED window (a 1970-epoch date) and checks that
/// the settled pass's own `StoreMode::Settled` arm supersedes both — not merely that a hand-driven
/// supersede call can.
///
/// Two mutations to production code survive the rest of this suite without this test: changing the
/// `Settled` arm's `supersede_key` argument from `Some(&provisional)` to `None` on either the quote
/// or the bar call in `store_then_resample`, and making `Recent`'s resample use the
/// canonical key instead of the provisional one. Both are kill-proofed against this test (see the
/// branch's fix report for the lane output).
#[test]
fn a_settled_window_supersedes_a_real_prior_provisional_entry_quotes_and_bars() {
    let (_dir, hist) = open();
    // Day 5, 1970-epoch — genuinely settled relative to real wall-clock "now", the same window
    // `a_settled_chunk_with_a_spent_key_is_never_asked_for` above uses.
    let (start, end) = (5 * MS_PER_DAY, 6 * MS_PER_DAY - 1);
    let full_tape = tape_between(start, end);
    let range = TsRange::of(start, end);

    // PLANT what a prior recent-fetch pass would have left: the early partial tape, stored under
    // the real provisional formatters.
    hist.append_quotes(
        VENUE,
        SYMBOL,
        &tape_between(start, start + 80_000),
        Some(&provisional_quote_commit_key(SYMBOL, start, end)),
    )
    .unwrap();
    hist.resample_quotes_to_bars(
        VENUE,
        SYMBOL,
        INTERVAL,
        range,
        Some(&provisional_resample_key(SYMBOL, INTERVAL, range)),
    )
    .unwrap();

    // The REAL settled pass — drives `StoreMode::Settled` end to end, with the provisional entry
    // above already sitting in the store.
    let bars =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, from, to| {
            Ok(tape_between(from, to))
        })
        .unwrap();

    assert_eq!(bars, 3, "day 5's three minutes with a tick");

    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(
        quotes.len(),
        full_tape.len(),
        "exactly the complete tape is stored — the provisional rows were REPLACED, not kept \
         alongside it (doubled would mean the quote-side supersede silently stopped superseding)"
    );

    let expect = consolidate_quotes(&full_tape, STEP_MS);
    let got = hist.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    assert_eq!(
        got.len(),
        expect.len(),
        "not the provisional bars left sitting beside the settled ones"
    );
    for (e, g) in expect.iter().zip(got.iter()) {
        assert_eq!(e.ts, g.ts);
        assert_eq!(e.open.to_bits(), g.open.to_bits());
        assert_eq!(e.high.to_bits(), g.high.to_bits());
        assert_eq!(e.low.to_bits(), g.low.to_bits());
        assert_eq!(e.close.to_bits(), g.close.to_bits());
        assert_eq!(
            e.volume.to_bits(),
            g.volume.to_bits(),
            "the assertion that actually protects against inflated volume — the spec's whole \
             reason for existing"
        );
    }

    assert!(
        quote_key_is_spent(&hist, &quote_commit_key(SYMBOL, start, end)),
        "the canonical quote key is now spent"
    );
    assert_eq!(
        resample_and_store_bars(&hist, SYMBOL, INTERVAL, range).unwrap(),
        0,
        "the canonical resample key is spent too — re-running the resample writes nothing further"
    );
}

/// The BAR-side twin of `a_settled_one_chunk_window_recovers_data_missed_by_an_earlier_recent_fetch`
/// above, which only ever checks quotes — and a test that can catch a DIFFERENT, real mutation
/// (the RECENT CHUNKS tests above catch it too): `StoreMode::Recent`'s resample using the
/// CANONICAL key instead of the provisional one. If it did, a real early fetch would spend the
/// canonical resample key on its OWN partial bars — a recent chunk's resample always covers the
/// same range the settled
/// pass will later use — so the later settled pass's `resample_quotes_to_bars_superseding` would
/// find that canonical key already spent and return `Ok(0)` BEFORE ever attempting the supersede,
/// leaving the early, partial bars stuck under the canonical key forever with no error.
///
/// Like the sibling test above, this cannot use two real calls to `backfill_quotes_then_bars` to go
/// from "recent" to "settled" — settledness is judged against the real wall clock, which a test
/// cannot fake forward — so it fetches the recent half for real (genuinely exercising
/// `StoreMode::Recent`'s resample, not just its quote ingest) and then simulates "now it has
/// settled" on BOTH sides with the exact canonical/provisional keys `backfill_quotes_then_bars`
/// itself would use.
#[test]
fn a_recent_one_chunk_fetch_leaves_the_canonical_resample_key_unspent_too() {
    let (_dir, hist) = open();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let yesterday = today - MS_PER_DAY;
    let full_tape = tape_between(yesterday, today - 1);
    let range = TsRange::of(yesterday, today - 1);

    // A REAL early fetch — genuinely exercises `StoreMode::Recent`'s resample, storing
    // under whatever key that arm actually uses (the provisional one, correctly).
    let early = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        yesterday,
        today - 1,
        &|| false,
        |_, from, to| Ok(tape_between(from, to.min(yesterday + 80_000))),
    )
    .unwrap();
    assert_eq!(early, 2, "the 10s and 70s ticks have arrived — two bars from the partial tape");

    // Simulate the window having settled, on BOTH sides, with the exact keys
    // `backfill_quotes_then_bars`'s own `Settled` arm would use for this same window.
    hist.append_quotes_superseding(
        VENUE,
        SYMBOL,
        &full_tape,
        Some(&quote_commit_key(SYMBOL, yesterday, today - 1)),
        Some(&provisional_quote_commit_key(SYMBOL, yesterday, today - 1)),
    )
    .unwrap();
    hist.resample_quotes_to_bars_superseding(
        VENUE,
        SYMBOL,
        INTERVAL,
        range,
        Some(&resample_commit_key(SYMBOL, INTERVAL, range)),
        Some(&provisional_resample_key(SYMBOL, INTERVAL, range)),
    )
    .unwrap();

    let expect = consolidate_quotes(&full_tape, STEP_MS);
    let got = hist.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    assert_eq!(
        got.len(),
        expect.len(),
        "exactly the complete tape's bars — not the early PARTIAL bars stuck forever under a \
         canonical key the real early fetch should never have spent (this is the assertion that \
         would fail had Recent's resample used the canonical key instead of the \
         provisional one: the simulated settled pass above would then have found the canonical \
         key already spent and returned before ever attempting the supersede)"
    );
    for (e, g) in expect.iter().zip(got.iter()) {
        assert_eq!(e.ts, g.ts);
        assert_eq!(e.volume.to_bits(), g.volume.to_bits());
    }
}

/// The race the store's PRE-SPEND closes, driven through the real lane: a recent request decides
/// "recent" from its one clock read and then spends its fetch time, while a second request, started
/// after the window crossed the margin, decides "settled" — and finishes first. The settled pass is
/// simulated on BOTH sides with the exact keys its `Settled` arm uses (wall-clock settledness
/// cannot be faked forward, the same reason the tests above simulate it), with no provisional entry
/// in the store, so there is nothing to supersede: the settled commits still spend the provisional
/// twins. Then the REAL recent `backfill_quotes_then_bars` over the same window lands late with a
/// short tape, and must write nothing. Before the pre-spend its ticks and bars were sealed beside
/// the settled ones, and no later settled pass could ever remove them: its canonical keys were
/// already spent, so the skip-before-fetch stopped it before the supersede. (Since #9 the late
/// request meets the pre-spent provisional key at the RECENT skip, before it even fetches; one
/// whose skip ran before the settled commit landed meets it at the store's own write instead, under
/// the series lock — `crates/vike-store/tests/store/superseding_commits.rs`'s
/// `a_provisional_commit_after_its_canonical_twin_writes_nothing` pins that half.)
#[test]
fn a_recent_fetch_after_its_window_was_settled_writes_nothing() {
    let (_dir, hist) = open();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let yesterday = today - MS_PER_DAY;
    let full_tape = tape_between(yesterday, today - 1);
    let range = TsRange::of(yesterday, today - 1);

    // The settled pass, first: no provisional entry exists, so each side supersedes nothing.
    hist.append_quotes_superseding(
        VENUE,
        SYMBOL,
        &full_tape,
        Some(&quote_commit_key(SYMBOL, yesterday, today - 1)),
        Some(&provisional_quote_commit_key(SYMBOL, yesterday, today - 1)),
    )
    .unwrap();
    hist.resample_quotes_to_bars_superseding(
        VENUE,
        SYMBOL,
        INTERVAL,
        range,
        Some(&resample_commit_key(SYMBOL, INTERVAL, range)),
        Some(&provisional_resample_key(SYMBOL, INTERVAL, range)),
    )
    .unwrap();

    // The recent request, landing late with what it fetched before the day was published.
    let late = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        yesterday,
        today - 1,
        &|| false,
        |_, from, to| Ok(tape_between(from, to.min(yesterday + 80_000))),
    )
    .unwrap();
    assert_eq!(late, 0, "the settled commits pre-spent both provisional keys: nothing written");

    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(quotes.len(), full_tape.len(), "exactly the full tape, not the short one beside it");
    let expect = consolidate_quotes(&full_tape, STEP_MS);
    let got = hist.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    assert_eq!(got.len(), expect.len(), "the settled bars alone, not the recent ones beside them");
    for (e, g) in expect.iter().zip(got.iter()) {
        assert_eq!(e.ts, g.ts);
        assert_eq!(e.volume.to_bits(), g.volume.to_bits(), "volume inflated by a late duplicate");
    }
}
