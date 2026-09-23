# `tests/fixtures/api/` provenance

> **Moved here on 2026-08-25**, byte-identical (each file's digest compared before and after), from
> the dissolving vike-research crate's own `tests/fixtures/api/` directory. The heading above still
> names that directory deliberately: it is where these bodies were captured to, and every capture
> command, measurement and correction below is unchanged from the day it was written.
>
> Their consumer is now `crates/vike-backfill/tests/vikedata_captured_pages.rs`, which drives each
> body through `crates/vike-backfill/src/vikedata/parse.rs` — the data.vike.io collector the wire
> rules moved into under
> `docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md`, where a study reads the store
> and the collector owns the wire. Sections below name symbols and test names from the old crate
> (`CohortClient`, `by_size`, `MetricRow`, `fetch_axis`, `FixtureHttp` and the tests around them):
> read those as the record of what a body was captured to prove, not as live pointers.

All captures taken 2026-08-10 (~09:00-10:00 UTC) via the CI box, against the live
`https://data.vike.io/v1/hyperliquid/coins/BTC/cohort-metrics` endpoint, using the API key at
`/var/lib/vike/vike_trader_rust/settings/secrets.env` (`VIKE_API_KEY`). Every command below
was run as `ssh the CI box` (curl on the CI box, never on the dev box, which holds no credential).

## `cohort_size_page1.json`

```sh
curl -s -H "X-API-KEY: $K" \
  "https://data.vike.io/v1/hyperliquid/coins/BTC/cohort-metrics?axis=size&hours=6&start=2026-08-10T05:00:00Z"
```

One page, `nextCursor: null` by construction: `start` was chosen close enough to capture time that
the whole requested window (05:00-08:00 UTC) came back in a single page. Needed this way because
`cohort_size_page1.json` (`SIZE_P1`) is used STANDALONE (a single queued body) in three tests
(`the_start_that_goes_on_the_wire_is_floored_to_the_hour`,
`an_explicit_start_and_the_page_size_are_always_sent`,
`label_basis_is_sent_for_pnl_and_withheld_for_size`) — a page carrying a cursor would make the
client request a second page those tests never queue, failing on "fixture transport exhausted"
rather than on what the test actually means to check.

## `cohort_pnl_page1.json` / `cohort_pnl_page2.json`

```sh
curl -s -H "X-API-KEY: $K" \
  "https://…/cohort-metrics?axis=pnl&hours=1&start=2026-08-10T04:00:00Z&labelBasis=point_in_time"
# -> cohort_pnl_page1.json ; one hour (08:00Z), nextCursor decodes to
#    {"m":"cohort_metrics","t":"2026-08-10 08:00:00"}
curl -s -H "X-API-KEY: $K" --data-urlencode axis=pnl --data-urlencode hours=5 \
  --data-urlencode start=2026-08-10T04:00:00Z --data-urlencode labelBasis=point_in_time \
  --data-urlencode "cursor=$(jq -r .nextCursor cohort_pnl_page1.json)" -G "https://…/cohort-metrics"
# -> cohort_pnl_page2.json ; four hours (04:00-07:00Z), nextCursor: null
```

Verified before committing, per the task's own load-bearing check:

```sh
jq -r '.nextCursor // "null"' cohort_pnl_page1.json   # a real, non-empty string
jq -r '.nextCursor // "null"' cohort_pnl_page2.json   # "null"
```

Page 1 deliberately covers ONE hour and page 2 covers FOUR (rather than 2-and-1, the first attempt)
for a second, independent reason beyond "page1 has a cursor, page2 terminates": the dedup test
(`an_hour_repeated_across_a_page_boundary_is_deduped_not_doubled`) compares
`sum(long_usd)` between fetching page2 alone (`n1`, one page's notional) and fetching
`[page1, page1, page2]` deduped (`n2`, three distinct hours' notional) against the bound
`sum(n2) < 2.0 * sum(n1)`. Real BTC pnl-axis hourly notional is fairly uniform (~$1.1-1.2B/hour
measured across several consecutive hours), so with page1 and page2 close in size the bound is
violated by construction (3 roughly-equal hours summed exceeds 2× one hour) regardless of whether
dedup works. Making page1 the SMALLEST possible page (one hour) and page2 several hours (so
`page1_total < page2_total`) gives `sum(n2) = page1+page2 < 2*page2 = 2*sum(n1)` a real, comfortable
margin (measured: page1 ≈ $1.12B, page2 ≈ $4.50B, n2 ≈ $5.62B < 2×$4.50B = $9.00B) without touching
the assertion itself.

⚠ **Known limitation, not silently papered over:** `cohort_pnl_page1.json` (`PNL_P1`) is ALSO used
standalone in `label_basis_is_sent_for_pnl_and_withheld_for_size` in the plan's original verbatim
form (`vec![PNL_P1.into()]`). The live endpoint always returns the NEWEST hours first and walks
backward via cursor — there is no way to request "as of 2026-08-06" data from a live query made on
2026-08-10, so any honestly-captured page1 with a real cursor has `oldest ts` a few hours before
*today's* capture instant, never 60 days before the test's fixed `NOW` constant
(`2026-08-06T13:02:17Z`, baked into the test file). The client's page-bound-by-window early-exit
(`now_secs - oldest >= days * 86400`) therefore cannot fire on page 1 alone, so a standalone fetch of
a real cursor'd `PNL_P1` requests a second page that a single-body `FixtureHttp` never queues, and
`fetch_axis(...).unwrap()` panics on "fixture transport exhausted". This is a structural interaction
between the fixed `NOW` constant, the live endpoint's newest-first paging, and the plan's own
explicit requirement that `PNL_P1` carry a real cursor for the pagination tests — not a capture
mistake, and unavoidable with any real single-page `PNL_P1`.

**Resolution taken (the one deviation from the plan's verbatim test code):**
`label_basis_is_sent_for_pnl_and_withheld_for_size`'s pnl half now queues
`vec![PNL_P1.into(), PNL_P2.into()]` instead of `vec![PNL_P1.into()]`. The assertion itself
(`a.calls()[0].param("labelBasis")`) is unchanged and still checks call 0 — queuing page 2 only lets
the walk terminate the way it genuinely would against the live endpoint, instead of starving on a
call the original single-body form never provisioned. Flagged here and in the task's final report
rather than silently kept.

## `cohort_tier_page1.json`

**Metrics rows VERBATIM from the db_data_jobs PR #685 capture — still NOT a live-endpoint HTTP
capture from this crate.** The tier axis's API landed in db_data_jobs PR #683 with a five-label
taxonomy read off a stale DB column; PR #685 (branch `refactor/position-size-taxonomy-unify`)
regenerated the query layer onto the canonical TEN position-notional buckets and captured the
regenerated path against the CI box `hl_perps` (read-only) on 2026-08-11:
`tests/unit/fixtures/hl_position_size_taxonomy_capture.json` in that repo, whose
`cohort_metrics_tier_BTC_3h` object is `cohort_metrics(BTC, axis=tier, hours=3)` — 30 rows,
3 hours (08:00–10:00 UTC) × all ten buckets. This fixture's `metrics` array is that capture's,
byte-for-byte (`jq -S`-verified at generation). Row order is the capture's: buckets DESCENDING
by notional within each hour (`>$2.5m` first, ≈$1.59B, down to `<$1k`), hours newest-first —
the same paging convention as the other axes. The capture's envelope carried `"hours": 3` and a
real hour-keyed `nextCursor` (decoding to `{"m":"cohort_metrics","t":"2026-08-11 08:00:00"}`);
this fixture keeps the three hours and sets `nextCursor: null` because it is used STANDALONE
(single queued body), the same reasoning as `cohort_size_page1.json` above. `bias` /
`position_count*` / `total_position_size*` are shape-fidelity fields the loader never reads
(`MetricRow` consumes `ts`/`cohort`/`total_position_value`/`total_position_value_long`/
`label_basis` only).

The wire contract, as pinned by the tests that use this fixture: the axis selector is the
`axis` query parameter (`size`/`pnl`/`tier`); tier labels are the website's notional ladder
**verbatim** in the `cohort` field (`>$2.5m`, `$1m-$2.5m`, … `$1k-$10k`, `<$1k` — with the
`$ < > - .` characters on the wire; the client normalises them to db_data_jobs' own slugs,
`above_2_5m` … `below_1k`, before they become column names); `short = total_position_value −
total_position_value_long` (the existing client derivation); an hour with no snapshot rows
yields NO bucket and a tier with no positions yields NO row — never fabricated zeros, so the
absent-cell→NaN pivot semantics apply exactly as for the other axes. `labelBasis` on the tier
axis is ALWAYS `point_in_time` server-side: omitting it and sending `point_in_time` are both
accepted, `labelBasis=current` is a 400 ("axis='tier' is always 'point_in_time'…") — this
client omits it, the always-valid spelling and the same request shape as size.

**Re-capture from the live endpoint once the regenerated API DEPLOYS** — same curl as the size
page, with `axis=tier` and no `labelBasis` — and replace this section with the capture command.
Until then every tier test in this crate is fixture-driven and the LIVE tier fetch is
UNVERIFIED.

## `cohort_pnl_no_label_basis.json`

The ONE edited-from-a-capture fixture the plan calls for. Built from `cohort_pnl_page1.json` (the
1-hour capture above) with the envelope's `"label_basis"` line deleted by hand
(`jq 'del(.label_basis)'`) — the exact shape an API predating the flag returns: 200, per-row
`label_basis` still present, but the envelope's own field is gone. No live endpoint produces this
shape any more.

## `cohort_pnl_junk_labels.json`

Base: one real captured hour (`2026-08-06T09:00:00Z`) pulled from a wide historical pnl-axis scan
(`axis=pnl&hours=2000&start=2026-06-10T09:00:00Z&labelBasis=point_in_time`), containing all 13 real
labels the pnl ladder + unrankable bucket produce, including a genuine `"unknown"` row.
`nextCursor` is explicitly `null` (single page) since this fixture is used standalone.

⚠ **One row added, not a real capture.** A systematic search for a genuine empty-string (`""`)
cohort label — every asset the `/coins/{SYMBOL}/cohort-metrics` endpoint was tried against (BTC,
ETH, SOL, kBONK, CRV, ENA, DOGE, ARB, AVAX, LINK, HYPE, XRP, WLD, kPEPE), both axes, full retained
history — found none. ClickHouse forensics on the CI box explain why: `hl_perps.
cohort_bias_hourly_pnl_realized` (the aggregated table the pnl axis reads) has exactly the 12 ladder
names plus `unknown`, never a blank; `hl_perps.position_snapshots_hourly.size_cohort` (raw, backing
the size axis) is one of the 12 clean names for every row in the whole table, no blank, no
`unknown`, across its full history. The raw per-position `pnl_cohort_frozen` column DOES default to
`''` for un-frozen legacy positions (39% of raw rows: 559,458,095 of 1,439,560,903), but the
endpoint's aggregation evidently filters that out before a response is ever built — consistent with
finding zero over an exhaustive live-endpoint search. So the fixture's one `cohort: ""` row (same
hour, `total_position_value: 1234.56`, `total_position_value_long: 600.0`, `n_wallets: 2`) is
HAND-ADDED, modelled on a real row's field shape, because current production data cannot produce
this case through the public endpoint. Flagged as a concern in the task's final report.

## ⚠ CORRECTION (2026-08-10): the search above was NOT exhaustive, and the row was NOT necessary

**The empty-string label is REAL and is now captured from live data** — `fixtures/hl_cohort/loader.json`
(the parity export, Task 31) carries **six genuine `cohort: ""` rows on the SIZE axis**, and its
`manifest.adversarial_cases` holds the forensics.

Two claims above are therefore wrong, and the reason is worth stating rather than quietly editing:

1. **"across its full history" was a window artefact.** The search that found zero began at
   **2026-06-10**. `position_snapshots_hourly.size_cohort` holds the 12 clean ladder names only
   from **2026-04-03 05:42** onward; **before** that cutover it holds the empty string, across
   559,458,095 rows. The blank was never absent — it was outside the window that was looked at,
   and "full retained history" described the intent of the search rather than its range.
2. **The axis was wrong too.** The blank is a **size**-axis value. This file reasoned about it as a
   pnl-axis phenomenon (via `pnl_cohort_frozen`), which is a different column with a different
   story.

**What this means for the hand-added row:** it stays, because the JSON fixture it lives in is a
`cohort_pnl_junk_labels` capture and the tests around it are green and meaningful — but it should be
understood as a SYNTHETIC row exercising a real behaviour, not as evidence about what the endpoint
emits. The authority for what the data actually contains is `fixtures/hl_cohort/loader.json`, which
is a real capture.

**The transferable lesson:** "an exhaustive search found none" is only as strong as the window it
ran over, and a negative result should always record its range. A schema cutover — here 2026-04-03 —
makes recent data unrepresentative of history by construction.
