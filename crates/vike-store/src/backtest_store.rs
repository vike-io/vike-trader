//! What a Polymarket backtest could once read from, and what was given up. This module holds no code
//! and stays for this page: it is the file an operator asking "where does a Polymarket backtest read
//! from" lands in, and other crates cite it.
//!
//! **Today there is one answer: a datahub.** The selector that stood here (`--store`, `--archive` and
//! once `--clickhouse`, with its `BacktestStore`, `parse_backtest_store` and `open_backtest_store`) is
//! DELETED: its two drivers went on 2026-09-20 (#2030 and #2046), and `backtest --archive`, the last
//! caller, on the owner's ruling of 2026-11-06, together with the read-in-place store it opened. Every
//! history read goes through a datahub (`docs/decisions/0084-only-the-datahub-touches-the-store.md`).
//!
//! It existed because of the rule the owner set on 2026-09-19: **data is fetched by API or from the
//! venue directly, never by reaching ClickHouse.** Two collectors were retired under it that day
//! (`clickhouse_poly_backfill`, `clickhouse_spot_backfill`), and on 2026-09-20 the owner ruled REMOVE
//! on the rest, so **no code in this workspace reaches a ClickHouse server by any route** — the last
//! `clickhouse-client` subprocess in the tree was that module's `run_export`, and it went with it.
//!
//! # ⚠ WHAT THE DELETION COST, recorded here because an absent file explains nothing
//!
//! `crates/vike-backfill/src/backtest_bridge.rs` (`ClickHousePolyHistStore`, 1,072 lines) and
//! `crates/vike-backfill/src/clickhouse_poly/` (the export→decode→append collector, 1,150) are
//! gone. Two capabilities went with them and have NO replacement anywhere in this tree. They were
//! named at `clickhouse_poly/mod.rs`'s module doc, which is why they are restated HERE.
//!
//! 1. **`polymarket_snapshots` — the ONLY source of Polymarket L1 quotes before 2026-07-23.** The
//!    live L2 recorder's `l1_quotes` table starts on that date, and `data.vike.io/archive` serves
//!    `l1_quotes`; the pre-July tape was written by the retired Python poller into a table the
//!    archive does not carry and no API exposes. Roughly three months of quote history is therefore
//!    unreachable from this tree. ⚠ It is NOT deleted: the rows are still on the recorder's box.
//!    What is gone is every tool here that could read them, so recovering that window means a new
//!    API-side producer, not a revert.
//! 2. **The `<slug>#<outcome_index>` trade keying** the `cheap_np_run` research runner discovers
//!    its windows from (`crates/vike-strategies-check/examples/cheap_np_run.rs`). The archive is
//!    TOKEN-keyed — one `symbol=<token_id>` per row — and carries no slug column at all, so nothing
//!    in this tree writes a slug-keyed tape any more.
//!
//! **Neither of those was what held the two bins to ClickHouse**, which is what made the cut a code
//! question rather than a data one — measured 2026-09-19 against the deleted file and restated here
//! because the file it was measured on is gone: `backtest_bridge`'s `quotes_query` selected `FROM
//! {db}.l1_quotes` and named `polymarket_snapshots` nowhere; all three of its queries filtered
//! `WHERE token_id = '…'`, so the tape it read was token-keyed; and the ONLY item it imported from
//! `clickhouse_poly` was `run_export`, a generic `clickhouse-client` subprocess shell — it built its
//! own SQL and its own decoders. So the three lanes these bins actually read (`book_events`,
//! `trades`, `l1_quotes`, all keyed `venue=polymarket`/`symbol=token_id`) are exactly the three
//! `crates/vike-backfill/src/bin/vike_archive_backfill.rs` pulls from `data.vike.io/archive` over
//! HTTP.
//!
//! ⚠ **One divergence a comparison against an OLD run still has.** `vike_store::HistoryStoreDataFusion`
//! scopes a `TimestampRange` on `ts` (the venue's own "as of" stamp); the deleted ClickHouse store scoped on
//! `local_timestamp` (when the recorder READ the frame off the socket). So a number in an operator's
//! scrollback from a `--clickhouse` run is not comparable with one produced today, and the recorder's
//! two stamps still disagree on the tape. [`vike_data::window_clock::WindowClock`] carries the
//! measurement.
